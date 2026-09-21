//! N-API adapters for the transport servers.
//!
//! The transport crates own framing, sessions, request dispatch, and socket
//! teardown. This module only translates JavaScript option bags and keeps the
//! Filesystem driver alive while exposing the small lifecycle surface that
//! Node callers need.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use mount_rs_9p::{
    DirCursor as TransportP9DirCursor, FidCursorView as TransportP9FidCursorView,
    FidOpenState as TransportP9FidOpenState, FidOpenView as TransportP9FidOpenView,
    FidTable as TransportP9FidTable, FidView as TransportP9FidView,
    P9AssertionHook as TransportP9AssertionHook, P9Lock as TransportP9Lock,
    P9LockClient as TransportP9LockClient, P9LockHolder as TransportP9LockHolder,
    P9LockRequest as TransportP9LockRequest, P9LockTable as TransportP9LockTable,
    P9LockTableOptions as TransportP9LockTableOptions, P9Server as TransportP9Server,
    P9ServerHooks as TransportP9ServerHooks, P9ServerOptions as TransportP9ServerOptions,
    P9SessionErrorHook as TransportP9SessionErrorHook, P9SessionHooks as TransportP9SessionHooks,
    P9TransportError as TransportP9Error, P9TransportErrorHook as TransportP9ErrorHook,
};
use mount_rs_core::{ErrorCode, FileHandle as CoreFileHandle, FsDriver, FsError, OpenFlags, Stats};
use mount_rs_fuse::{
    FuseMountHooks as TransportFuseMountHooks, FuseTransportError as TransportFuseError,
    FuseTransportErrorHook as TransportFuseErrorHook,
};
use mount_rs_nfs::{
    FileHandleTable as TransportNfsHandleTable, NFS_V4, NFS4_PROGRAM,
    Nfs3Session as TransportNfsSession, Nfs4Clock as TransportNfs4Clock,
    Nfs4IdMap as TransportNfs4IdMap, Nfs4Session as TransportNfs4Session,
    NfsConnection as TransportNfsConnection, NfsRequestContext as TransportNfsRequestContext,
    NfsServer as TransportNfsServer, NfsServerHooks as TransportNfsServerHooks,
    NfsServerOptions as TransportNfsServerOptions, NfsSessionError as TransportNfsSessionError,
    NfsSessionErrorHook as TransportNfsSessionErrorHook, NfsTransportError as TransportNfsError,
    NfsTransportErrorHook as TransportNfsErrorHook, RpcCall as TransportNfsRpcCall,
};
use mount_rs_s3::{
    Credentials as TransportS3Credentials, HeaderEntry as TransportS3HeaderEntry,
    S3RequestBody as TransportS3RequestBody, S3RequestHead as TransportS3RequestHead,
    S3Response as TransportS3Response, S3ResponseBodyStream as TransportS3ResponseBodyStream,
    S3Server as TransportS3Server, S3ServerHooks as TransportS3ServerHooks,
    S3ServerOptions as TransportS3ServerOptions, S3Session as TransportS3Session, S3SessionOptions,
    S3StreamBody as TransportS3StreamBody, S3StreamResponse as TransportS3StreamResponse,
    S3TransportError as TransportS3Error, S3TransportErrorHook as TransportS3ErrorHook,
};
use mount_rs_webdav::{
    WebdavBody as TransportWebdavBody, WebdavError as TransportWebdavRequestError,
    WebdavError as TransportWebdavBodyError, WebdavErrorHook as TransportWebdavRequestErrorHook,
    WebdavRequestBody, WebdavRequestHead as TransportWebdavRequestHead,
    WebdavResponse as TransportWebdavResponse, WebdavServer as TransportWebdavServer,
    WebdavServerHooks as TransportWebdavServerHooks,
    WebdavServerOptions as TransportWebdavServerOptions, WebdavSession as TransportWebdavSession,
    WebdavSessionHooks as TransportWebdavSessionHooks,
    WebdavTransportError as TransportWebdavError,
    WebdavTransportErrorHook as TransportWebdavErrorHook, XmlNode as TransportWebdavXmlNode,
};
use napi::bindgen_prelude::{
    BigInt, Buffer, Either, Env, FnArgs, FromNapiValue, Function, JsObjectValue, Object,
    ReadableStream, Reader, Reference, Unknown,
};
use napi::futures_core::Stream as FuturesStream;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Error, JsValue, Status, ValueType, sys};
use napi_derive::napi;

use super::nfs_codec::{NfsRpcCall, from_call as from_nfs_call};
use super::p9_codec::{NativeP9Header, NativeP9Qid};
use super::{FileHandle, FileHandle as JsFileHandle, Filesystem, MountDriver};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub(crate) type JsTransportErrorCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
type TransportErrorCall = FnArgs<(Error, Option<String>)>;
type TransportErrorTsfn = ThreadsafeFunction<
    TransportErrorEvent,
    Unknown<'static>,
    TransportErrorCall,
    Status,
    false,
    false,
>;

#[derive(Clone)]
pub(crate) struct TransportErrorEvent {
    message: String,
    peer: Option<String>,
}

impl From<TransportNfsError> for TransportErrorEvent {
    fn from(error: TransportNfsError) -> Self {
        let TransportNfsError { message, peer, .. } = error;
        Self { message, peer }
    }
}

impl From<TransportP9Error> for TransportErrorEvent {
    fn from(error: TransportP9Error) -> Self {
        let TransportP9Error { message, peer, .. } = error;
        Self { message, peer }
    }
}

impl From<TransportWebdavError> for TransportErrorEvent {
    fn from(error: TransportWebdavError) -> Self {
        let TransportWebdavError { message, peer, .. } = error;
        Self { message, peer }
    }
}

impl From<TransportFuseError> for TransportErrorEvent {
    fn from(error: TransportFuseError) -> Self {
        Self {
            message: error.message,
            peer: None,
        }
    }
}

impl From<TransportS3Error> for TransportErrorEvent {
    fn from(error: TransportS3Error) -> Self {
        let TransportS3Error { message, peer, .. } = error;
        Self { message, peer }
    }
}

/// Owns the JavaScript callback independently from the transport hook.
///
/// The Rust transports retain their hook closure until their server is dropped,
/// so closing a Node server must explicitly abort and remove the TSFN rather
/// than merely dropping the N-API wrapper. The closed flag also makes a hook
/// callback racing with shutdown a no-op; the TSFN's aborted lock closes the
/// remaining call-versus-release race.
pub(crate) struct TransportErrorCallback {
    callback: Mutex<Option<Arc<TransportErrorTsfn>>>,
    closed: AtomicBool,
}

pub(crate) type JsWebdavErrorCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
type WebdavErrorCall = FnArgs<(Error, Option<WebdavRequestHead>)>;
type WebdavErrorTsfn =
    ThreadsafeFunction<WebdavErrorEvent, Unknown<'static>, WebdavErrorCall, Status, false, false>;

#[derive(Clone)]
struct WebdavErrorEvent {
    message: String,
    head: Option<WebdavRequestHead>,
}

/// Owns the JavaScript callback for request-level WebDAV errors.
pub(crate) struct WebdavErrorCallback {
    callback: Mutex<Option<Arc<WebdavErrorTsfn>>>,
    closed: AtomicBool,
}

impl WebdavErrorCallback {
    pub(crate) fn new(function: JsWebdavErrorCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<WebdavErrorEvent>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| {
                let event = context.value;
                Ok(FnArgs::from((
                    Error::new(Status::GenericFailure, event.message),
                    event.head,
                )))
            })?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn report(&self, error: TransportWebdavRequestError, head: TransportWebdavRequestHead) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        };
        let Some(callback) = callback else {
            return;
        };
        let event = WebdavErrorEvent {
            message: error.to_string(),
            head: Some(webdav_request_head(head)),
        };
        let _ = callback.call_with_return_value(
            event,
            ThreadsafeFunctionCallMode::NonBlocking,
            |result, _env| {
                let _ = result;
                Ok(())
            },
        );
    }

    pub(crate) fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for WebdavErrorCallback {
    fn drop(&mut self) {
        self.release();
    }
}

impl TransportErrorCallback {
    pub(crate) fn new(function: JsTransportErrorCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<TransportErrorEvent>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| {
                let event = context.value;
                Ok(FnArgs::from((
                    Error::new(Status::GenericFailure, event.message),
                    event.peer,
                )))
            })?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn report(&self, event: TransportErrorEvent) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        };
        let Some(callback) = callback else {
            return;
        };

        // This is a notification, not a transport operation that can carry a
        // JavaScript rejection. `call_with_return_value` captures a synchronous
        // throw so it cannot become an uncaught exception or abort the process;
        // the transport event itself remains delivered exactly once.
        let _ = callback.call_with_return_value(
            event,
            ThreadsafeFunctionCallMode::NonBlocking,
            |result, _env| {
                let _ = result;
                Ok(())
            },
        );
    }

    pub(crate) fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for TransportErrorCallback {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) type JsNfsErrorCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
type NfsErrorCall = FnArgs<(Error, Option<NfsRpcCall>)>;
type NfsErrorTsfn =
    ThreadsafeFunction<NfsErrorEvent, Unknown<'static>, NfsErrorCall, Status, false, false>;

#[derive(Clone)]
struct NfsErrorEvent {
    message: String,
    call: Option<TransportNfsRpcCall>,
}

/// Owns the JavaScript callback for request-level NFS errors.
pub(crate) struct NfsErrorCallback {
    callback: Mutex<Option<Arc<NfsErrorTsfn>>>,
    closed: AtomicBool,
}

impl NfsErrorCallback {
    pub(crate) fn new(function: JsNfsErrorCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<NfsErrorEvent>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| {
                let event = context.value;
                Ok(FnArgs::from((
                    Error::new(Status::GenericFailure, event.message),
                    event.call.map(from_nfs_call),
                )))
            })?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn report(&self, error: TransportNfsSessionError, call: Option<TransportNfsRpcCall>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        };
        let Some(callback) = callback else {
            return;
        };
        let message = match error.offset {
            Some(offset) => format!("{} at byte {offset}", error.message),
            None => error.message,
        };
        let event = NfsErrorEvent { message, call };
        let _ = callback.call_with_return_value(
            event,
            ThreadsafeFunctionCallMode::NonBlocking,
            |result, _env| {
                let _ = result;
                Ok(())
            },
        );
    }

    pub(crate) fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for NfsErrorCallback {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) type JsNfsIdMapNameCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
pub(crate) type JsNfsIdMapIdCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
pub(crate) type JsNfsClockCallback = Function<'static, Unknown<'static>, Unknown<'static>>;

type NfsIdMapNameCall = FnArgs<(f64, bool)>;
type NfsIdMapNameTsfn =
    ThreadsafeFunction<NfsIdMapNameEvent, Unknown<'static>, NfsIdMapNameCall, Status, false, false>;
type NfsIdMapIdCall = FnArgs<(String, bool)>;
type NfsIdMapIdTsfn =
    ThreadsafeFunction<NfsIdMapIdEvent, Unknown<'static>, NfsIdMapIdCall, Status, false, false>;
type NfsClockTsfn = ThreadsafeFunction<(), Unknown<'static>, (), Status, false, false>;

#[derive(Clone)]
struct NfsIdMapNameEvent {
    id: f64,
    group: bool,
}

#[derive(Clone)]
struct NfsIdMapIdEvent {
    name: String,
    group: bool,
}

fn parse_nfs_name(value: Unknown<'static>) -> Option<String> {
    if value.get_type().ok()? != ValueType::String {
        return None;
    }
    // SAFETY: the value type was checked above and the conversion copies it.
    unsafe { String::from_napi_value(value.value().env, value.raw()) }.ok()
}

fn parse_nfs_id(value: Unknown<'static>) -> Option<u32> {
    if value.get_type().ok()? != ValueType::Number {
        return None;
    }
    // SAFETY: the value type was checked above.
    let value = unsafe { f64::from_napi_value(value.value().env, value.raw()) }.ok()?;
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(&value) {
        return None;
    }
    Some(value as u32)
}

fn parse_nfs_clock(value: Unknown<'static>) -> Option<f64> {
    if value.get_type().ok()? != ValueType::Number {
        return None;
    }
    // SAFETY: the value type was checked above.
    let value = unsafe { f64::from_napi_value(value.value().env, value.raw()) }.ok()?;
    value.is_finite().then_some(value)
}

pub(crate) struct NfsIdMapNameCallback {
    callback: Mutex<Option<Arc<NfsIdMapNameTsfn>>>,
    closed: AtomicBool,
}

impl NfsIdMapNameCallback {
    fn new(function: JsNfsIdMapNameCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<NfsIdMapNameEvent>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| Ok(FnArgs::from((context.value.id, context.value.group))))?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn call(&self, id: u32, group: bool) -> Option<String> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        }?;
        let (sender, receiver) = sync_channel(1);
        let status = callback.call_with_return_value(
            NfsIdMapNameEvent {
                id: id as f64,
                group,
            },
            ThreadsafeFunctionCallMode::NonBlocking,
            move |result, _env| {
                let _ = sender.send(result.ok().and_then(parse_nfs_name));
                Ok(())
            },
        );
        if status != Status::Ok {
            return None;
        }
        receiver.recv().ok().flatten()
    }

    fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for NfsIdMapNameCallback {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) struct NfsIdMapIdCallback {
    callback: Mutex<Option<Arc<NfsIdMapIdTsfn>>>,
    closed: AtomicBool,
}

impl NfsIdMapIdCallback {
    fn new(function: JsNfsIdMapIdCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<NfsIdMapIdEvent>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| {
                Ok(FnArgs::from((context.value.name, context.value.group)))
            })?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn call(&self, name: &str, group: bool) -> Option<u32> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        }?;
        let (sender, receiver) = sync_channel(1);
        let status = callback.call_with_return_value(
            NfsIdMapIdEvent {
                name: name.to_owned(),
                group,
            },
            ThreadsafeFunctionCallMode::NonBlocking,
            move |result, _env| {
                let _ = sender.send(result.ok().and_then(parse_nfs_id));
                Ok(())
            },
        );
        if status != Status::Ok {
            return None;
        }
        receiver.recv().ok().flatten()
    }

    fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for NfsIdMapIdCallback {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) struct NfsClockCallback {
    callback: Mutex<Option<Arc<NfsClockTsfn>>>,
    closed: AtomicBool,
}

impl NfsClockCallback {
    fn new(function: JsNfsClockCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<()>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|_| Ok(()))?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn call(&self) -> Option<f64> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        }?;
        let (sender, receiver) = sync_channel(1);
        let status = callback.call_with_return_value(
            (),
            ThreadsafeFunctionCallMode::NonBlocking,
            move |result, _env| {
                let _ = sender.send(result.ok().and_then(parse_nfs_clock));
                Ok(())
            },
        );
        if status != Status::Ok {
            return None;
        }
        receiver.recv().ok().flatten()
    }

    fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for NfsClockCallback {
    fn drop(&mut self) {
        self.release();
    }
}

struct NfsJsClock {
    callback: Arc<NfsClockCallback>,
    state: Mutex<NfsJsClockState>,
}

struct NfsJsClockState {
    epoch_ms: Option<f64>,
    anchor: Instant,
    last: Instant,
}

impl NfsJsClock {
    fn new(callback: Arc<NfsClockCallback>) -> Arc<Self> {
        let anchor = Instant::now();
        Arc::new(Self {
            callback,
            state: Mutex::new(NfsJsClockState {
                epoch_ms: None,
                anchor,
                last: anchor,
            }),
        })
    }

    fn now(&self) -> Instant {
        let value = self.callback.call();
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(value) = value else {
            // A callback throw, Promise, invalid number, or a callback that
            // has already been released must not make a monotonic lease clock
            // jump backwards to a fresh process instant.
            return state.last;
        };
        let Some(epoch_ms) = state.epoch_ms else {
            state.epoch_ms = Some(value);
            return state.last;
        };
        let delta_ms = value - epoch_ms;
        if delta_ms.is_finite() && delta_ms > 0.0 && delta_ms < u64::MAX as f64 {
            let duration = Duration::from_millis(delta_ms as u64);
            if let Some(candidate) = state.anchor.checked_add(duration)
                && candidate > state.last
            {
                state.last = candidate;
            }
        }
        state.last
    }
}

#[derive(Default)]
struct NfsCallbackKeepalive {
    idmap_name: Option<Arc<NfsIdMapNameCallback>>,
    idmap_id: Option<Arc<NfsIdMapIdCallback>>,
    clock: Option<Arc<NfsClockCallback>>,
}

impl NfsCallbackKeepalive {
    fn release(&self) {
        if let Some(callback) = &self.idmap_name {
            callback.release();
        }
        if let Some(callback) = &self.idmap_id {
            callback.release();
        }
        if let Some(callback) = &self.clock {
            callback.release();
        }
    }
}

pub(crate) type JsP9SessionErrorCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
type P9SessionErrorCall = FnArgs<(Error, Option<NativeP9Header>)>;
type P9SessionErrorTsfn = ThreadsafeFunction<
    P9SessionErrorEvent,
    Unknown<'static>,
    P9SessionErrorCall,
    Status,
    false,
    false,
>;

#[derive(Clone)]
struct P9SessionErrorEvent {
    error: FsError,
    header: Option<NativeP9Header>,
}

/// Owns the JavaScript callback for request-level 9P errors.
pub(crate) struct P9SessionErrorCallback {
    callback: Mutex<Option<Arc<P9SessionErrorTsfn>>>,
    closed: AtomicBool,
}

impl P9SessionErrorCallback {
    pub(crate) fn new(function: JsP9SessionErrorCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<P9SessionErrorEvent>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| {
                let event = context.value;
                Ok(FnArgs::from((
                    super::to_js_error(event.error),
                    event.header,
                )))
            })?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn report(&self, error: FsError, header: Option<NativeP9Header>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        };
        let Some(callback) = callback else {
            return;
        };
        let _ = callback.call_with_return_value(
            P9SessionErrorEvent { error, header },
            ThreadsafeFunctionCallMode::NonBlocking,
            |result, _env| {
                let _ = result;
                Ok(())
            },
        );
    }

    pub(crate) fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for P9SessionErrorCallback {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) type JsP9AssertionCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
type P9AssertionCall = FnArgs<(String,)>;
type P9AssertionTsfn =
    ThreadsafeFunction<String, Unknown<'static>, P9AssertionCall, Status, false, false>;

/// Owns the JavaScript callback for debug-mode 9P assertion failures.
pub(crate) struct P9AssertionCallback {
    callback: Mutex<Option<Arc<P9AssertionTsfn>>>,
    closed: AtomicBool,
}

impl P9AssertionCallback {
    pub(crate) fn new(function: JsP9AssertionCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<String>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| Ok(FnArgs::from((context.value,))))?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn report(&self, message: String) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        };
        let Some(callback) = callback else {
            return;
        };
        let _ = callback.call_with_return_value(
            message,
            ThreadsafeFunctionCallMode::NonBlocking,
            |result, _env| {
                let _ = result;
                Ok(())
            },
        );
    }

    pub(crate) fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for P9AssertionCallback {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) fn nfs_hooks(
    transport_callback: Option<&Arc<TransportErrorCallback>>,
    session_callback: Option<&Arc<NfsErrorCallback>>,
) -> TransportNfsServerHooks {
    TransportNfsServerHooks {
        on_transport_error: transport_callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |error: TransportNfsError| callback.report(error.into()))
                as TransportNfsErrorHook
        }),
        on_error: session_callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(
                move |error: TransportNfsSessionError, call: Option<TransportNfsRpcCall>| {
                    callback.report(error, call);
                },
            ) as TransportNfsSessionErrorHook
        }),
    }
}

pub(crate) fn p9_hooks(callback: Option<&Arc<TransportErrorCallback>>) -> TransportP9ServerHooks {
    TransportP9ServerHooks {
        on_transport_error: callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |error: TransportP9Error| callback.report(error.into()))
                as TransportP9ErrorHook
        }),
    }
}

pub(crate) fn p9_session_hooks(
    error: Option<&Arc<P9SessionErrorCallback>>,
    assertion: Option<&Arc<P9AssertionCallback>>,
) -> TransportP9SessionHooks {
    TransportP9SessionHooks {
        on_error: error.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(
                move |error: FsError, header: Option<mount_rs_9p::P9Header>| {
                    let header = header.map(|header| NativeP9Header {
                        size: header.size,
                        type_: header.type_,
                        tag: header.tag,
                    });
                    callback.report(error, header);
                },
            ) as TransportP9SessionErrorHook
        }),
        on_assertion: assertion.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |message: String| callback.report(message)) as TransportP9AssertionHook
        }),
    }
}

pub(crate) fn webdav_hooks(
    callback: Option<&Arc<TransportErrorCallback>>,
) -> TransportWebdavServerHooks {
    TransportWebdavServerHooks {
        on_transport_error: callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |error: TransportWebdavError| callback.report(error.into()))
                as TransportWebdavErrorHook
        }),
    }
}

pub(crate) fn webdav_session_hooks(
    callback: Option<&Arc<WebdavErrorCallback>>,
) -> TransportWebdavSessionHooks {
    TransportWebdavSessionHooks {
        on_error: callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(
                move |error: TransportWebdavRequestError, head: TransportWebdavRequestHead| {
                    callback.report(error, head);
                },
            ) as TransportWebdavRequestErrorHook
        }),
    }
}

pub(crate) fn fuse_hooks(
    callback: Option<&Arc<TransportErrorCallback>>,
) -> TransportFuseMountHooks {
    TransportFuseMountHooks {
        on_transport_error: callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |error: TransportFuseError| callback.report(error.into()))
                as TransportFuseErrorHook
        }),
    }
}

fn s3_hooks(callback: Option<&Arc<TransportErrorCallback>>) -> TransportS3ServerHooks {
    TransportS3ServerHooks {
        on_transport_error: callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |error: TransportS3Error| callback.report(error.into()))
                as TransportS3ErrorHook
        }),
    }
}

fn config_error(message: impl Into<String>) -> Error {
    super::to_js_error(
        FsError::new(ErrorCode::Einval)
            .with_syscall("createServer")
            .with_message(message),
    )
}

fn transport_error(operation: &str, error: impl std::fmt::Display) -> Error {
    Error::new(
        Status::GenericFailure,
        format!("mount-rs {operation} failed: {error}"),
    )
}

fn number(name: &str, value: Option<f64>, default: usize) -> Result<usize, Error> {
    let Some(value) = value else {
        return Ok(default);
    };
    if !value.is_finite()
        || value.fract() != 0.0
        || !(0.0..=MAX_SAFE_INTEGER).contains(&value)
        || value > usize::MAX as f64
    {
        return Err(config_error(format!(
            "{name} must be an integer between 0 and {MAX_SAFE_INTEGER}"
        )));
    }
    Ok(value as usize)
}

fn positive_number(name: &str, value: Option<f64>, default: usize) -> Result<usize, Error> {
    let value = number(name, value, default)?;
    if value == 0 {
        return Err(config_error(format!("{name} must be greater than zero")));
    }
    Ok(value)
}

fn u16_number(name: &str, value: Option<f64>, default: u16) -> Result<u16, Error> {
    let value = number(name, value, default as usize)?;
    u16::try_from(value).map_err(|_| config_error(format!("{name} must be between 0 and 65535")))
}

fn u32_number(name: &str, value: Option<f64>, default: u32) -> Result<u32, Error> {
    let value = number(name, value, default as usize)?;
    u32::try_from(value).map_err(|_| config_error(format!("{name} must fit in a uint32")))
}

fn duration_ms(name: &str, value: Option<f64>, default: Duration) -> Result<Duration, Error> {
    let default_ms = default.as_millis();
    let value = number(
        name,
        value,
        usize::try_from(default_ms).unwrap_or(usize::MAX),
    )?;
    Ok(Duration::from_millis(u64::try_from(value).map_err(
        |_| config_error(format!("{name} is too large")),
    )?))
}

fn strip_brackets(host: &str) -> Result<&str, Error> {
    match (host.starts_with('['), host.ends_with(']')) {
        (true, true) if host.len() > 2 => Ok(&host[1..host.len() - 1]),
        (false, false) if !host.contains(['[', ']']) => Ok(host),
        _ => Err(config_error(format!("invalid host {host:?}"))),
    }
}

fn ip_host(host: Option<String>, default: &str) -> Result<(String, IpAddr), Error> {
    let host = host.unwrap_or_else(|| default.to_owned());
    let bare = strip_brackets(&host)?;
    let address = if bare.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else {
        bare.parse::<IpAddr>().map_err(|_| {
            config_error(format!("host {host:?} must be an IP address or localhost"))
        })?
    };
    Ok((host, address))
}

fn bracketed_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

fn valid_s3_bucket_name(bucket: &str) -> bool {
    !bucket.is_empty()
        && bucket != "."
        && bucket != ".."
        // The pinned JavaScript oracle measures the public string as UTF-16
        // code units, not UTF-8 bytes. Keep the native preflight identical so
        // non-ASCII bucket names do not diverge at the N-API boundary.
        && bucket.encode_utf16().count() <= 255
        && bucket
            .chars()
            .all(|character| {
                let code = character as u32;
                code >= 0x20 && code != 0x7f && character != '/' && character != '\\'
            })
}

fn verifier(value: Option<Buffer>) -> Result<Option<[u8; 8]>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    let bytes = value.as_ref();
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| config_error("verifier must contain exactly 8 bytes"))?;
    Ok(Some(array))
}

struct Nfs4IdMapCallbacks {
    name: Option<Arc<NfsIdMapNameCallback>>,
    id: Option<Arc<NfsIdMapIdCallback>>,
}

fn nfs4_idmap(options: Nfs4IdMap) -> Result<(TransportNfs4IdMap, Nfs4IdMapCallbacks), Error> {
    let Nfs4IdMap {
        domain,
        users,
        groups,
        name_of,
        id_of,
    } = options;
    let name = name_of.map(NfsIdMapNameCallback::new).transpose()?;
    let id = id_of.map(NfsIdMapIdCallback::new).transpose()?;
    let mut output = TransportNfs4IdMap::new(domain);
    let mut user_ids = HashMap::<u32, String>::new();
    for (name, value) in users.unwrap_or_default() {
        let id = u32_number(&format!("nfs4.idmap.users.{name}"), Some(value), 0)?;
        if let Some(previous) = user_ids.insert(id, name.clone()) {
            return Err(config_error(format!(
                "nfs4.idmap.users maps {previous:?} and {name:?} to uid {id}"
            )));
        }
        output = output.with_user(name, id);
    }
    let mut group_ids = HashMap::<u32, String>::new();
    for (name, value) in groups.unwrap_or_default() {
        let id = u32_number(&format!("nfs4.idmap.groups.{name}"), Some(value), 0)?;
        if let Some(previous) = group_ids.insert(id, name.clone()) {
            return Err(config_error(format!(
                "nfs4.idmap.groups maps {previous:?} and {name:?} to gid {id}"
            )));
        }
        output = output.with_group(name, id);
    }
    if let Some(callback) = &name {
        let callback = Arc::clone(callback);
        output = output.with_name_of(move |id, group| callback.call(id, group));
    }
    if let Some(callback) = &id {
        let callback = Arc::clone(callback);
        output = output.with_id_of(move |name, group| callback.call(name, group));
    }
    Ok((output, Nfs4IdMapCallbacks { name, id }))
}

#[napi(object)]
pub struct Nfs4IdMap {
    /// Domain used to qualify mapped owner names on the wire.
    pub domain: Option<String>,
    /// Name-to-uid entries. Unmapped ids retain numeric wire form.
    pub users: Option<HashMap<String, f64>>,
    /// Name-to-gid entries. Unmapped ids retain numeric wire form.
    pub groups: Option<HashMap<String, f64>>,
    #[napi(ts_type = "(id: number, group: boolean) => string | undefined")]
    pub name_of: Option<JsNfsIdMapNameCallback>,
    #[napi(ts_type = "(name: string, group: boolean) => number | undefined")]
    pub id_of: Option<JsNfsIdMapIdCallback>,
}

#[napi(object)]
pub struct Nfs4StateKnobs {
    pub idmap: Option<Nfs4IdMap>,
    #[napi(ts_type = "() => number")]
    pub now: Option<JsNfsClockCallback>,
    pub lease_seconds: Option<f64>,
    pub seed: Option<f64>,
    pub max_sessions: Option<f64>,
    pub max_fore_slots: Option<f64>,
    pub max_operations: Option<f64>,
    pub max_request_size: Option<f64>,
    pub max_cached_response_size: Option<f64>,
    pub max_opens_per_file: Option<f64>,
    pub max_locks_per_file: Option<f64>,
    pub require_reclaim_complete: Option<bool>,
}

#[napi(object)]
pub struct NfsServerOptions {
    pub port: Option<f64>,
    pub host: Option<String>,
    pub allow_remote: Option<bool>,
    pub max_record: Option<f64>,
    pub max_in_flight: Option<f64>,
    pub use_driver_ino: Option<bool>,
    #[napi(ts_type = "Uint8Array")]
    pub verifier: Option<Buffer>,
    pub max_handles: Option<f64>,
    pub rtmax: Option<f64>,
    pub wtmax: Option<f64>,
    pub dtpref: Option<f64>,
    pub snapshot_cache: Option<f64>,
    pub claim_ownership: Option<bool>,
    pub nfs4: Option<Nfs4StateKnobs>,
    #[napi(ts_type = "(error: unknown, peer: string | undefined) => void")]
    pub on_transport_error: Option<JsTransportErrorCallback>,
    #[napi(ts_type = "(error: unknown, call: NfsRpcCall | undefined) => void")]
    pub on_error: Option<JsNfsErrorCallback>,
}

type ParsedNfsOptions = (
    String,
    u16,
    TransportNfsServerOptions,
    Option<JsTransportErrorCallback>,
    Option<JsNfsErrorCallback>,
    NfsCallbackKeepalive,
);

fn nfs_options(options: Option<NfsServerOptions>) -> Result<ParsedNfsOptions, Error> {
    let options = options.unwrap_or(NfsServerOptions {
        port: None,
        host: None,
        allow_remote: None,
        max_record: None,
        max_in_flight: None,
        use_driver_ino: None,
        verifier: None,
        max_handles: None,
        rtmax: None,
        wtmax: None,
        dtpref: None,
        snapshot_cache: None,
        claim_ownership: None,
        nfs4: None,
        on_transport_error: None,
        on_error: None,
    });
    let on_transport_error = options.on_transport_error;
    let on_error = options.on_error;
    let mut callback_keepalive = NfsCallbackKeepalive::default();
    let (host, address) = ip_host(options.host, "127.0.0.1")?;
    let port = u16_number("port", options.port, 0)?;
    let mut output = TransportNfsServerOptions {
        bind: SocketAddr::new(address, port),
        ..TransportNfsServerOptions::default()
    };
    output.allow_remote = options.allow_remote.unwrap_or(false);
    output.record_limit = positive_number("maxRecord", options.max_record, output.record_limit)?;
    output.max_in_flight =
        positive_number("maxInFlight", options.max_in_flight, output.max_in_flight)?;
    output.session.use_driver_ino = options.use_driver_ino.unwrap_or(true);
    output.session.verifier = verifier(options.verifier)?;
    output.session.max_handles = options
        .max_handles
        .map(|value| number("maxHandles", Some(value), 0))
        .transpose()?;
    output.session.rtmax = positive_number("rtmax", options.rtmax, output.session.rtmax)?;
    output.session.wtmax = positive_number("wtmax", options.wtmax, output.session.wtmax)?;
    output.session.dtpref = positive_number("dtpref", options.dtpref, output.session.dtpref)?;
    output.session.snapshot_cache = number(
        "snapshotCache",
        options.snapshot_cache,
        output.session.snapshot_cache,
    )?;
    output.session.claim_ownership = options.claim_ownership.unwrap_or(true);
    if let Some(nfs4) = options.nfs4 {
        if let Some(idmap) = nfs4.idmap {
            let (idmap, callbacks) = nfs4_idmap(idmap)?;
            output.session.nfs4.idmap = Some(idmap);
            callback_keepalive.idmap_name = callbacks.name;
            callback_keepalive.idmap_id = callbacks.id;
        }
        if let Some(now) = nfs4.now {
            let callback = NfsClockCallback::new(now)?;
            let clock = NfsJsClock::new(Arc::clone(&callback));
            output.session.nfs4.clock = TransportNfs4Clock::from_fn(move || clock.now());
            callback_keepalive.clock = Some(callback);
        }
        output.session.nfs4.lease_seconds = u32_number(
            "nfs4.leaseSeconds",
            nfs4.lease_seconds,
            output.session.nfs4.lease_seconds,
        )?;
        output.session.nfs4.seed = u32_number("nfs4.seed", nfs4.seed, output.session.nfs4.seed)?;
        output.session.nfs4.max_sessions = positive_number(
            "nfs4.maxSessions",
            nfs4.max_sessions,
            output.session.nfs4.max_sessions,
        )?;
        output.session.nfs4.max_fore_slots = positive_number(
            "nfs4.maxForeSlots",
            nfs4.max_fore_slots,
            output.session.nfs4.max_fore_slots,
        )?;
        output.session.nfs4.max_operations = positive_number(
            "nfs4.maxOperations",
            nfs4.max_operations,
            output.session.nfs4.max_operations,
        )?;
        output.session.nfs4.max_request_size = positive_number(
            "nfs4.maxRequestSize",
            nfs4.max_request_size,
            output.session.nfs4.max_request_size,
        )?;
        output.session.nfs4.max_cached_response_size = number(
            "nfs4.maxCachedResponseSize",
            nfs4.max_cached_response_size,
            output.session.nfs4.max_cached_response_size,
        )?;
        output.session.nfs4.max_opens_per_file = positive_number(
            "nfs4.maxOpensPerFile",
            nfs4.max_opens_per_file,
            output.session.nfs4.max_opens_per_file,
        )?;
        output.session.nfs4.max_locks_per_file = positive_number(
            "nfs4.maxLocksPerFile",
            nfs4.max_locks_per_file,
            output.session.nfs4.max_locks_per_file,
        )?;
        output.session.nfs4.require_reclaim_complete =
            nfs4.require_reclaim_complete.unwrap_or(true);
    }
    Ok((
        host,
        port,
        output,
        on_transport_error,
        on_error,
        callback_keepalive,
    ))
}

#[napi]
pub struct NfsSession {
    inner: TransportNfsSession,
    v4_inner: TransportNfs4Session,
}

#[napi(object)]
pub struct NfsSessionStats {
    pub requests: f64,
    pub replies: f64,
    pub errors: f64,
    pub dropped: f64,
    pub procedures: HashMap<String, f64>,
}

#[napi(object)]
pub struct NfsHandleEntry {
    pub id: BigInt,
    pub fileid: BigInt,
    pub key: Option<String>,
    pub path: String,
}

fn nfs_handle_entries(table: &TransportNfsHandleTable) -> Vec<NfsHandleEntry> {
    table
        .entries()
        .into_iter()
        .map(|entry| NfsHandleEntry {
            id: BigInt::from(entry.id),
            fileid: BigInt::from(entry.fileid),
            key: entry.key,
            path: entry.path,
        })
        .collect()
}

/// Read-only N-API view of the versioned NFS sessions owned by a server.
#[napi]
impl NfsSession {
    /// Handle one unframed NFSv3 or NFSv4 RPC record. Malformed records return
    /// `null`; decoded calls return one encoded RPC reply.
    #[napi]
    pub async fn handle_call(&self, bytes: Buffer) -> Option<Buffer> {
        let context = TransportNfsRequestContext::default();
        let reply = if is_nfs_v4(bytes.as_ref()) {
            self.v4_inner.handle_call(bytes.as_ref(), context).await
        } else {
            self.inner.handle_call(bytes.as_ref(), context).await
        };
        reply.map(Buffer::from)
    }

    /// The NFSv4.1 session routed by this server. Its state is read-only at the
    /// N-API boundary and shares the server-owned driver lifetime.
    #[napi(getter)]
    pub fn v4(&self) -> Nfs4Session {
        Nfs4Session {
            inner: self.v4_inner.clone(),
        }
    }

    #[napi(getter)]
    pub fn stats(&self) -> NfsSessionStats {
        let stats = self.inner.stats();
        NfsSessionStats {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            dropped: stats.dropped as f64,
            procedures: stats
                .procedures
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
        }
    }

    #[napi(getter)]
    pub fn mounts(&self) -> Vec<Vec<String>> {
        self.inner
            .mounts()
            .into_iter()
            .map(|(hostname, directory)| vec![hostname, directory])
            .collect()
    }

    /// Stable read-only snapshots of the shared v3/v4 file-handle table.
    /// Handles are BigInts because the transport identity is u64.
    #[napi(getter)]
    pub fn handles(&self) -> Vec<NfsHandleEntry> {
        nfs_handle_entries(&self.inner.handles)
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.inner.destroyed() && self.v4_inner.destroyed()
    }

    /// Destroy both versioned sessions and release their shared server state.
    #[napi]
    pub async fn destroy(&self) {
        self.inner.destroy().await;
        self.v4_inner.destroy().await;
    }
}

/// Read-only N-API view of the NFSv4.1 session routed by an [`NfsServer`].
#[napi]
pub struct Nfs4Session {
    inner: TransportNfs4Session,
}

#[napi]
impl Nfs4Session {
    /// Handle one unframed NFSv4 RPC record. Malformed records return `null`;
    /// decoded calls return one encoded RPC reply.
    #[napi]
    pub async fn handle_call(&self, bytes: Buffer) -> Option<Buffer> {
        self.inner
            .handle_call(bytes.as_ref(), TransportNfsRequestContext::default())
            .await
            .map(Buffer::from)
    }

    /// Sweep expired NFSv4 client leases and release their process-local state.
    #[napi]
    pub async fn sweep_expired(&self) -> f64 {
        self.inner.sweep_expired().await as f64
    }

    #[napi(getter)]
    pub fn stats(&self) -> NfsSessionStats {
        let stats = self.inner.stats();
        NfsSessionStats {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            dropped: stats.dropped as f64,
            procedures: stats
                .procedures
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
        }
    }

    #[napi(getter)]
    pub fn handles(&self) -> Vec<NfsHandleEntry> {
        nfs_handle_entries(&self.inner.handles)
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.inner.destroyed()
    }

    /// Destroy the NFSv4.1 session and release its process-local state.
    #[napi]
    pub async fn destroy(&self) {
        self.inner.destroy().await;
    }
}

/// Read-only N-API view of one accepted NFS client connection.
#[napi]
pub struct NfsConnection {
    inner: TransportNfsConnection,
}

#[napi]
impl NfsConnection {
    #[napi(getter)]
    pub fn session(&self) -> NfsSession {
        NfsSession {
            inner: self.inner.session.clone(),
            v4_inner: self.inner.v4_session.clone(),
        }
    }

    #[napi(getter)]
    pub fn id(&self) -> f64 {
        self.inner.id() as f64
    }

    #[napi(getter)]
    pub fn peer(&self) -> Option<String> {
        self.inner.peer.clone()
    }

    #[napi(getter)]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.inner
            .close()
            .await
            .map_err(|error| transport_error("NFS connection close", error))
    }

    #[napi]
    pub async fn wait_closed(&self) -> napi::Result<()> {
        self.inner.wait_closed().await;
        Ok(())
    }
}

fn is_nfs_v4(bytes: &[u8]) -> bool {
    if bytes.len() < 20 {
        return false;
    }
    let program = u32::from_be_bytes(bytes[12..16].try_into().expect("NFS program bytes"));
    let version = u32::from_be_bytes(bytes[16..20].try_into().expect("NFS version bytes"));
    program == NFS4_PROGRAM && version == NFS_V4
}

#[napi]
pub struct NfsServer {
    inner: Arc<TransportNfsServer>,
    host: String,
    requested_port: u16,
    closed: AtomicBool,
    transport_error: Option<Arc<TransportErrorCallback>>,
    session_error: Option<Arc<NfsErrorCallback>>,
    callback_keepalive: NfsCallbackKeepalive,
}

#[napi]
impl NfsServer {
    #[napi(getter)]
    pub fn session(&self) -> NfsSession {
        NfsSession {
            inner: self.inner.session().clone(),
            v4_inner: self.inner.v4_session().clone(),
        }
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        self.inner.port().unwrap_or(self.requested_port) as u32
    }

    #[napi(getter)]
    pub fn connections(&self) -> u32 {
        self.inner.connections() as u32
    }

    #[napi]
    pub fn clients(&self) -> napi::Result<Vec<NfsConnection>> {
        self.inner
            .clients()
            .map(|clients| {
                clients
                    .into_iter()
                    .map(|inner| NfsConnection { inner })
                    .collect()
            })
            .map_err(|error| transport_error("NFS clients", error))
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("NFS listen", "server is closed"));
        }
        self.inner
            .listen()
            .await
            .map(|_| ())
            .map_err(|error| transport_error("NFS listen", error))
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closed.store(true, Ordering::Release);
        if let Some(callback) = &self.transport_error {
            callback.release();
        }
        if let Some(callback) = &self.session_error {
            callback.release();
        }
        self.callback_keepalive.release();
        self.inner
            .close()
            .await
            .map_err(|error| transport_error("NFS close", error))
    }
}

#[napi]
pub fn create_nfs_server(
    driver: &Filesystem,
    options: Option<NfsServerOptions>,
) -> napi::Result<NfsServer> {
    let (host, requested_port, options, on_transport_error, on_error, callback_keepalive) =
        nfs_options(options)?;
    let transport_error = on_transport_error
        .map(TransportErrorCallback::new)
        .transpose()?;
    let session_error = on_error.map(NfsErrorCallback::new).transpose()?;
    let inner = TransportNfsServer::new_with_hooks(
        MountDriver(Arc::clone(&driver.driver)),
        options,
        nfs_hooks(transport_error.as_ref(), session_error.as_ref()),
    );
    Ok(NfsServer {
        inner: Arc::new(inner),
        host,
        requested_port,
        closed: AtomicBool::new(false),
        transport_error,
        session_error,
        callback_keepalive,
    })
}

fn p9_u64(value: BigInt, name: &str) -> napi::Result<u64> {
    let (negative, magnitude, _) = value.get_u128();
    if negative || magnitude > u128::from(u64::MAX) {
        return Err(config_error(format!(
            "{name} must be a non-negative integer that fits in uint64"
        )));
    }
    Ok(magnitude as u64)
}

#[napi(object)]
pub struct P9LockTableOptions {
    pub max_locks_per_file: Option<f64>,
}

#[napi(object)]
pub struct P9LockRequest {
    pub path: String,
    pub fid: u32,
    #[napi(js_name = "type")]
    pub type_: u8,
    pub start: BigInt,
    pub length: BigInt,
    pub proc_id: u32,
    pub client_id: String,
}

fn p9_lock_request(value: P9LockRequest) -> napi::Result<TransportP9LockRequest> {
    Ok(TransportP9LockRequest {
        path: value.path,
        fid: value.fid,
        type_: value.type_,
        start: p9_u64(value.start, "start")?,
        length: p9_u64(value.length, "length")?,
        proc_id: value.proc_id,
        client_id: value.client_id,
    })
}

#[napi(object)]
pub struct P9LockHolder {
    #[napi(js_name = "type")]
    pub type_: u8,
    pub start: BigInt,
    pub length: BigInt,
    pub proc_id: u32,
    pub client_id: String,
}

fn p9_lock_holder(value: TransportP9LockHolder) -> P9LockHolder {
    P9LockHolder {
        type_: value.type_,
        start: BigInt::from(value.start),
        length: BigInt::from(value.length),
        proc_id: value.proc_id,
        client_id: value.client_id,
    }
}

#[napi(object)]
pub struct P9Lock {
    #[napi(js_name = "type")]
    pub type_: u8,
    pub start: BigInt,
    pub length: BigInt,
    pub proc_id: u32,
    pub client_id: String,
    pub holder: f64,
    pub fid: u32,
}

fn p9_lock(value: TransportP9Lock) -> P9Lock {
    P9Lock {
        type_: value.type_,
        start: BigInt::from(value.start),
        length: BigInt::from(value.length),
        proc_id: value.proc_id,
        client_id: value.client_id,
        holder: value.holder as f64,
        fid: value.fid,
    }
}

#[derive(Clone)]
#[napi]
pub struct P9LockTable {
    inner: TransportP9LockTable,
}

impl FromNapiValue for P9LockTable {
    unsafe fn from_napi_value(
        env: napi::sys::napi_env,
        value: napi::sys::napi_value,
    ) -> napi::Result<Self> {
        let reference = unsafe { Reference::<Self>::from_napi_value(env, value)? };
        Ok((*reference).clone())
    }
}

#[napi]
impl P9LockTable {
    #[napi(constructor)]
    pub fn new(options: Option<P9LockTableOptions>) -> napi::Result<Self> {
        let max_locks_per_file = positive_number(
            "maxLocksPerFile",
            options.and_then(|options| options.max_locks_per_file),
            TransportP9LockTableOptions::default().max_locks_per_file,
        )?;
        Ok(Self {
            inner: TransportP9LockTable::new(TransportP9LockTableOptions { max_locks_per_file }),
        })
    }

    #[napi(getter)]
    pub fn files(&self) -> f64 {
        self.inner.files() as f64
    }

    #[napi(getter)]
    pub fn size(&self) -> f64 {
        self.inner.size() as f64
    }

    #[napi]
    pub fn at(&self, path: String) -> Vec<P9Lock> {
        self.inner.at(&path).into_iter().map(p9_lock).collect()
    }

    #[napi]
    pub fn getlock(&self, request: P9LockRequest) -> napi::Result<Option<P9LockHolder>> {
        self.inner
            .getlock(&p9_lock_request(request)?)
            .map(|holder| holder.map(p9_lock_holder))
            .map_err(super::to_js_error)
    }

    #[napi]
    pub fn remap(&self, from: String, to: String) {
        self.inner.remap(&from, &to);
    }

    #[napi]
    pub fn release(&self, path: String) {
        self.inner.release(&path);
    }

    #[napi]
    pub fn client(&self) -> P9LockClient {
        P9LockClient {
            inner: self.inner.client(),
        }
    }
}

#[napi]
pub struct P9LockClient {
    inner: TransportP9LockClient,
}

#[napi]
impl P9LockClient {
    #[napi(getter)]
    pub fn table(&self) -> P9LockTable {
        P9LockTable {
            inner: self.inner.table.clone(),
        }
    }

    #[napi(getter)]
    pub fn id(&self) -> f64 {
        self.inner.id as f64
    }

    #[napi(getter)]
    pub fn held(&self) -> f64 {
        self.inner.held() as f64
    }

    #[napi]
    pub fn lock(&self, request: P9LockRequest) -> napi::Result<u8> {
        self.inner
            .lock(&p9_lock_request(request)?)
            .map_err(super::to_js_error)
    }

    #[napi]
    pub fn getlock(&self, request: P9LockRequest) -> napi::Result<Option<P9LockHolder>> {
        self.inner
            .getlock(&p9_lock_request(request)?)
            .map(|holder| holder.map(p9_lock_holder))
            .map_err(super::to_js_error)
    }

    #[napi]
    pub fn release_fid(&self, fid: u32) {
        self.inner.release_fid(fid);
    }

    #[napi]
    pub fn release_all(&self) {
        self.inner.release_all();
    }

    #[napi]
    pub fn renamed(&self, from: String, to: String) {
        self.inner.renamed(&from, &to);
    }

    #[napi]
    pub fn released(&self, path: String) {
        self.inner.released(&path);
    }
}

#[napi(object)]
pub struct P9FidTableOptions {
    pub use_driver_ino: Option<bool>,
}

#[napi(object)]
pub struct P9FidOffset {
    pub offset: BigInt,
    pub index: f64,
}

#[napi(object)]
pub struct P9FidCursor {
    pub entries: Vec<String>,
    pub offsets: Vec<P9FidOffset>,
}

#[napi]
#[derive(Clone)]
pub struct P9FidOpenState {
    flags: f64,
    handle: Option<FileHandle>,
    directory: bool,
    qid: Option<NativeP9Qid>,
}

#[napi]
impl P9FidOpenState {
    #[napi(constructor)]
    pub fn new(
        flags: Option<f64>,
        handle: Option<&FileHandle>,
        directory: Option<bool>,
        qid: Option<NativeP9Qid>,
    ) -> napi::Result<Self> {
        let flags = u32_number("flags", flags, 0)? as f64;
        Ok(Self {
            flags,
            handle: handle.cloned(),
            directory: directory.unwrap_or(false),
            qid,
        })
    }

    #[napi(getter)]
    pub fn flags(&self) -> f64 {
        self.flags
    }

    #[napi(getter)]
    pub fn handle(&self) -> Option<FileHandle> {
        self.handle.clone()
    }

    #[napi(getter)]
    pub fn directory(&self) -> bool {
        self.directory
    }

    #[napi(getter)]
    pub fn qid(&self) -> Option<NativeP9Qid> {
        self.qid.clone()
    }
}

#[napi(object)]
pub struct P9DirResume {
    pub entries: Vec<String>,
    pub index: f64,
}

#[napi(object)]
pub struct P9StatsLike {
    pub dev: f64,
    pub ino: f64,
    pub mode: u32,
    pub mtime_ms: f64,
}

#[derive(Clone)]
enum P9FidTableTarget {
    Standalone(Arc<Mutex<TransportP9FidTable>>),
    Session(mount_rs_9p::P9Session),
}

fn p9_fid_error(fid: u32) -> Error {
    super::to_js_error(FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid}")))
}

fn p9_qid(value: mount_rs_9p::P9Qid) -> NativeP9Qid {
    NativeP9Qid {
        type_: value.type_,
        version: value.version,
        path: BigInt::from(value.path),
    }
}

fn p9_open_state(value: TransportP9FidOpenView) -> P9FidOpenState {
    P9FidOpenState {
        flags: value.flags as f64,
        handle: value.handle.map(|handle| JsFileHandle { inner: handle }),
        directory: value.directory,
        qid: value.qid.map(p9_qid),
    }
}

fn p9_transport_qid(value: NativeP9Qid) -> napi::Result<mount_rs_9p::P9Qid> {
    Ok(mount_rs_9p::P9Qid {
        type_: value.type_,
        version: value.version,
        path: p9_u64(value.path, "qid.path")?,
    })
}

fn p9_transport_open_state(value: P9FidOpenState) -> napi::Result<TransportP9FidOpenState> {
    let wire_flags = u32_number("flags", Some(value.flags), 0)?;
    Ok(TransportP9FidOpenState {
        flags: OpenFlags::from_bits(u64::from(wire_flags)),
        wire_flags,
        handle: value.handle.map(|handle| handle.inner),
        directory: value.directory,
        qid: value.qid.map(p9_transport_qid).transpose()?,
    })
}

fn p9_transport_cursor(value: P9FidCursor) -> napi::Result<TransportP9DirCursor> {
    let mut offsets = HashMap::with_capacity(value.offsets.len());
    for offset in value.offsets {
        let index = number("index", Some(offset.index), 0)?;
        offsets.insert(p9_u64(offset.offset, "offset")?, index);
    }
    Ok(TransportP9DirCursor {
        entries: value.entries,
        offsets,
    })
}

fn p9_cursor(value: TransportP9FidCursorView) -> P9FidCursor {
    P9FidCursor {
        entries: value.entries,
        offsets: value
            .offsets
            .into_iter()
            .map(|(offset, index)| P9FidOffset {
                offset: BigInt::from(offset),
                index: index as f64,
            })
            .collect(),
    }
}

fn p9_cursor_view(value: TransportP9DirCursor) -> TransportP9FidCursorView {
    TransportP9FidCursorView {
        entries: value.entries,
        offsets: value.offsets.into_iter().collect(),
    }
}

#[derive(Clone)]
#[napi]
pub struct P9Fid {
    table: P9FidTableTarget,
    fid: u32,
    detached: Option<TransportP9FidView>,
}

impl P9Fid {
    fn live(table: P9FidTableTarget, fid: u32) -> Self {
        Self {
            table,
            fid,
            detached: None,
        }
    }

    fn detached(table: P9FidTableTarget, value: TransportP9FidView) -> Self {
        Self {
            table,
            fid: value.fid,
            detached: Some(value),
        }
    }

    fn view(&self) -> napi::Result<TransportP9FidView> {
        if let Some(value) = &self.detached {
            return Ok(value.clone());
        }
        p9_target_view(&self.table, self.fid)
    }
}

#[napi]
impl P9Fid {
    #[napi(getter)]
    pub fn fid(&self) -> u32 {
        self.fid
    }

    #[napi(getter)]
    pub fn path(&self) -> napi::Result<String> {
        Ok(self.view()?.path)
    }

    #[napi(setter)]
    pub fn set_path(&mut self, path: String) -> napi::Result<()> {
        if let Some(value) = self.detached.as_mut() {
            let normalized = mount_rs_core::path::normalize_path(&path);
            if normalized != value.path {
                value.path = normalized;
                value.cursor = None;
            }
            return Ok(());
        }
        p9_target_set_path(&self.table, self.fid, &path).map_err(super::to_js_error)
    }

    #[napi(getter)]
    pub fn open(&self) -> napi::Result<Option<P9FidOpenState>> {
        Ok(self.view()?.open.map(p9_open_state))
    }

    #[napi(setter)]
    pub fn set_open(&mut self, open: Option<&P9FidOpenState>) -> napi::Result<()> {
        let open = open.cloned().map(p9_transport_open_state).transpose()?;
        if let Some(value) = self.detached.as_mut() {
            value.open = open.map(|open| TransportP9FidOpenView {
                flags: open.wire_flags,
                handle: open.handle,
                directory: open.directory,
                qid: open.qid,
            });
            return Ok(());
        }
        p9_target_set_open(&self.table, self.fid, open).map_err(super::to_js_error)
    }

    #[napi(getter)]
    pub fn iounit(&self) -> napi::Result<u32> {
        Ok(self.view()?.iounit)
    }

    #[napi(setter)]
    pub fn set_iounit(&mut self, iounit: u32) -> napi::Result<()> {
        if let Some(value) = self.detached.as_mut() {
            value.iounit = iounit;
            return Ok(());
        }
        p9_target_set_iounit(&self.table, self.fid, iounit).map_err(super::to_js_error)
    }

    #[napi(getter)]
    pub fn cursor(&self) -> napi::Result<Option<P9FidCursor>> {
        Ok(self.view()?.cursor.map(p9_cursor))
    }

    #[napi(setter)]
    pub fn set_cursor(&mut self, cursor: Option<P9FidCursor>) -> napi::Result<()> {
        let cursor = cursor.map(p9_transport_cursor).transpose()?;
        if let Some(value) = self.detached.as_mut() {
            value.cursor = cursor.map(p9_cursor_view);
            return Ok(());
        }
        p9_target_set_cursor(&self.table, self.fid, cursor).map_err(super::to_js_error)
    }
}

fn p9_target_size(target: &P9FidTableTarget) -> usize {
    match target {
        P9FidTableTarget::Standalone(table) => {
            table.lock().expect("9P fid table mutex poisoned").len()
        }
        P9FidTableTarget::Session(session) => session.fid_size(),
    }
}

fn p9_target_qid_path_count(target: &P9FidTableTarget) -> usize {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .qid_path_count(),
        P9FidTableTarget::Session(session) => session.fid_qid_path_count(),
    }
}

fn p9_target_exists(target: &P9FidTableTarget, fid: u32) -> bool {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .get(fid)
            .is_some(),
        P9FidTableTarget::Session(session) => session.fid_exists(fid),
    }
}

fn p9_target_view(target: &P9FidTableTarget, fid: u32) -> napi::Result<TransportP9FidView> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .view(fid)
            .ok_or_else(|| p9_fid_error(fid)),
        P9FidTableTarget::Session(session) => session.fid_view(fid).map_err(super::to_js_error),
    }
}

fn p9_target_ids(target: &P9FidTableTarget) -> Vec<u32> {
    match target {
        P9FidTableTarget::Standalone(table) => {
            table.lock().expect("9P fid table mutex poisoned").fids()
        }
        P9FidTableTarget::Session(session) => session.fid_ids(),
    }
}

fn p9_target_create(target: &P9FidTableTarget, fid: u32, path: &str) -> Result<(), FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .create(fid, path)
            .map(|_| ()),
        P9FidTableTarget::Session(session) => session.fid_create(fid, path),
    }
}

fn p9_target_clone(target: &P9FidTableTarget, from: u32, to: u32) -> Result<(), FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .clone_fid(from, to)
            .map(|_| ()),
        P9FidTableTarget::Session(session) => session.fid_clone(from, to),
    }
}

fn p9_target_clunk(target: &P9FidTableTarget, fid: u32) -> Result<TransportP9FidView, FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .clunk(fid)
            .map(|entry| entry.view()),
        P9FidTableTarget::Session(session) => session.fid_clunk(fid),
    }
}

fn p9_target_set_path(target: &P9FidTableTarget, fid: u32, path: &str) -> Result<(), FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => {
            let mut table = table.lock().expect("9P fid table mutex poisoned");
            let entry = table.get_mut(fid).ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid}"))
            })?;
            entry.set_path(path);
            Ok(())
        }
        P9FidTableTarget::Session(session) => session.fid_set_path(fid, path),
    }
}

fn p9_target_set_open(
    target: &P9FidTableTarget,
    fid: u32,
    open: Option<TransportP9FidOpenState>,
) -> Result<(), FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .set_open(fid, open),
        P9FidTableTarget::Session(session) => session.fid_set_open(fid, open),
    }
}

fn p9_target_set_iounit(target: &P9FidTableTarget, fid: u32, iounit: u32) -> Result<(), FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .set_iounit(fid, iounit),
        P9FidTableTarget::Session(session) => session.fid_set_iounit(fid, iounit),
    }
}

fn p9_target_set_cursor(
    target: &P9FidTableTarget,
    fid: u32,
    cursor: Option<TransportP9DirCursor>,
) -> Result<(), FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .set_cursor(fid, cursor),
        P9FidTableTarget::Session(session) => session.fid_set_cursor(fid, cursor),
    }
}

fn p9_target_resume(
    target: &P9FidTableTarget,
    fid: u32,
    offset: u64,
) -> Result<Option<(Vec<String>, usize)>, FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .resume(fid, offset),
        P9FidTableTarget::Session(session) => session.fid_resume(fid, offset),
    }
}

fn p9_target_snapshot(
    target: &P9FidTableTarget,
    fid: u32,
    entries: Vec<String>,
) -> Result<(Vec<String>, usize), FsError> {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .snapshot(fid, entries),
        P9FidTableTarget::Session(session) => session.fid_snapshot_entries(fid, entries),
    }
}

fn p9_target_note_offset(target: &P9FidTableTarget, fid: u32, offset: u64, index: usize) {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .note_offset(fid, offset, index),
        P9FidTableTarget::Session(session) => session.fid_note_offset(fid, offset, index),
    }
}

fn p9_target_qid_for(target: &P9FidTableTarget, stats: &Stats, path: &str) -> mount_rs_9p::P9Qid {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .qid_for(stats, path),
        P9FidTableTarget::Session(session) => session.fid_qid_for(stats, path),
    }
}

fn p9_target_qid_path_for(target: &P9FidTableTarget, stats: &Stats, path: &str) -> u64 {
    p9_target_qid_for(target, stats, path).path
}

fn p9_target_release(target: &P9FidTableTarget, path: &str) {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .release(path),
        P9FidTableTarget::Session(session) => session.fid_release(path),
    }
}

fn p9_target_remap(target: &P9FidTableTarget, from: &str, to: &str) {
    match target {
        P9FidTableTarget::Standalone(table) => table
            .lock()
            .expect("9P fid table mutex poisoned")
            .remap(from, to),
        P9FidTableTarget::Session(session) => session.fid_remap(from, to),
    }
}

fn p9_target_clear(target: &P9FidTableTarget) {
    match target {
        P9FidTableTarget::Standalone(table) => {
            table.lock().expect("9P fid table mutex poisoned").clear()
        }
        P9FidTableTarget::Session(session) => session.fid_clear(),
    }
}

fn p9_stats(value: P9StatsLike) -> napi::Result<Stats> {
    let dev = p9_u64_number(value.dev, "dev")?;
    let ino = p9_u64_number(value.ino, "ino")?;
    let mtime_ms = if value.mtime_ms.is_finite() {
        value
            .mtime_ms
            .trunc()
            .clamp(i64::MIN as f64, i64::MAX as f64) as i64
    } else {
        0
    };
    Ok(Stats {
        dev,
        ino,
        mode: value.mode,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: 0,
        blksize: 0,
        blocks: 0,
        atime_ms: 0,
        mtime_ms,
        ctime_ms: 0,
        birthtime_ms: 0,
    })
}

fn p9_u64_number(value: f64, name: &str) -> napi::Result<u64> {
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=u64::MAX as f64).contains(&value) {
        return Err(config_error(format!(
            "{name} must be a non-negative integer that fits uint64"
        )));
    }
    Ok(value as u64)
}

#[napi]
pub struct P9OpenHandle {
    fid: P9Fid,
    handle: FileHandle,
}

#[napi]
impl P9OpenHandle {
    #[napi(getter)]
    pub fn fid(&self) -> P9Fid {
        self.fid.clone()
    }

    #[napi(getter)]
    pub fn handle(&self) -> FileHandle {
        self.handle.clone()
    }
}

#[napi]
pub struct P9FidTable {
    inner: P9FidTableTarget,
}

#[napi]
impl P9FidTable {
    #[napi(constructor)]
    pub fn new(options: Option<P9FidTableOptions>) -> Self {
        Self {
            inner: P9FidTableTarget::Standalone(Arc::new(Mutex::new(TransportP9FidTable::new(
                options
                    .and_then(|options| options.use_driver_ino)
                    .unwrap_or(true),
            )))),
        }
    }

    #[napi(getter)]
    pub fn size(&self) -> f64 {
        p9_target_size(&self.inner) as f64
    }

    #[napi(getter)]
    pub fn qid_path_count(&self) -> f64 {
        p9_target_qid_path_count(&self.inner) as f64
    }

    #[napi]
    pub fn get(&self, fid: u32) -> Option<P9Fid> {
        p9_target_exists(&self.inner, fid).then(|| P9Fid::live(self.inner.clone(), fid))
    }

    #[napi]
    pub fn require(&self, fid: u32) -> napi::Result<P9Fid> {
        if !p9_target_exists(&self.inner, fid) {
            return Err(p9_fid_error(fid));
        }
        Ok(P9Fid::live(self.inner.clone(), fid))
    }

    #[napi]
    pub fn create(&self, fid: u32, path: String) -> napi::Result<P9Fid> {
        p9_target_create(&self.inner, fid, &path).map_err(super::to_js_error)?;
        Ok(P9Fid::live(self.inner.clone(), fid))
    }

    #[napi(js_name = "clone")]
    pub fn clone_fid(&self, from: u32, to: u32) -> napi::Result<P9Fid> {
        p9_target_clone(&self.inner, from, to).map_err(super::to_js_error)?;
        Ok(P9Fid::live(self.inner.clone(), to))
    }

    #[napi]
    pub fn clunk(&self, fid: u32) -> napi::Result<P9Fid> {
        let value = p9_target_clunk(&self.inner, fid).map_err(super::to_js_error)?;
        Ok(P9Fid::detached(self.inner.clone(), value))
    }

    #[napi]
    pub fn resume(&self, entry: &P9Fid, offset: BigInt) -> napi::Result<Option<P9DirResume>> {
        let offset = p9_u64(offset, "offset")?;
        p9_target_resume(&self.inner, entry.fid, offset)
            .map(|value| {
                value.map(|(entries, index)| P9DirResume {
                    entries,
                    index: index as f64,
                })
            })
            .map_err(super::to_js_error)
    }

    #[napi]
    pub fn snapshot(&self, entry: &P9Fid, entries: Vec<String>) -> napi::Result<P9DirResume> {
        p9_target_snapshot(&self.inner, entry.fid, entries)
            .map(|(entries, index)| P9DirResume {
                entries,
                index: index as f64,
            })
            .map_err(super::to_js_error)
    }

    #[napi]
    pub fn note_offset(&self, entry: &P9Fid, offset: BigInt, index: f64) -> napi::Result<()> {
        let offset = p9_u64(offset, "offset")?;
        let index = number("index", Some(index), 0)?;
        p9_target_note_offset(&self.inner, entry.fid, offset, index);
        Ok(())
    }

    #[napi]
    pub fn qid_for(&self, stats: P9StatsLike, path: String) -> napi::Result<NativeP9Qid> {
        Ok(p9_qid(p9_target_qid_for(
            &self.inner,
            &p9_stats(stats)?,
            &path,
        )))
    }

    #[napi]
    pub fn qid_path_for(&self, stats: P9StatsLike, path: String) -> napi::Result<BigInt> {
        Ok(BigInt::from(p9_target_qid_path_for(
            &self.inner,
            &p9_stats(stats)?,
            &path,
        )))
    }

    #[napi]
    pub fn release(&self, path: String) {
        p9_target_release(&self.inner, &path);
    }

    #[napi]
    pub fn remap(&self, from: String, to: String) {
        p9_target_remap(&self.inner, &from, &to);
    }

    #[napi]
    pub fn fids(&self) -> Vec<u32> {
        p9_target_ids(&self.inner)
    }

    #[napi]
    pub fn entries(&self) -> Vec<P9Fid> {
        p9_target_ids(&self.inner)
            .into_iter()
            .map(|fid| P9Fid::live(self.inner.clone(), fid))
            .collect()
    }

    #[napi]
    pub fn open_handles(&self) -> napi::Result<Vec<P9OpenHandle>> {
        let mut handles = Vec::new();
        for fid in p9_target_ids(&self.inner) {
            let value = p9_target_view(&self.inner, fid)?;
            if let Some(open) = value.open
                && let Some(handle) = open.handle
            {
                handles.push(P9OpenHandle {
                    fid: P9Fid::live(self.inner.clone(), fid),
                    handle: JsFileHandle { inner: handle },
                });
            }
        }
        Ok(handles)
    }

    #[napi]
    pub fn clear(&self) {
        p9_target_clear(&self.inner);
    }
}

#[napi(object)]
pub struct P9ServerOptions {
    pub port: Option<f64>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub allow_remote: Option<bool>,
    pub socket_mode: Option<f64>,
    pub allow_shared_directory: Option<bool>,
    pub max_frame: Option<f64>,
    pub max_in_flight: Option<f64>,
    pub msize: Option<f64>,
    pub use_driver_ino: Option<bool>,
    pub read_only: Option<bool>,
    pub claim_ownership: Option<bool>,
    pub debug: Option<bool>,
    pub locks: Option<P9LockTable>,
    #[napi(ts_type = "(error: unknown, peer: string | undefined) => void")]
    pub on_transport_error: Option<JsTransportErrorCallback>,
    #[napi(ts_type = "(error: unknown, header: NativeP9Header | undefined) => void")]
    pub on_error: Option<JsP9SessionErrorCallback>,
    #[napi(ts_type = "(message: string) => void")]
    pub on_assertion: Option<JsP9AssertionCallback>,
}

/// Read-only session policy exposed to Node callers. The transport's driver
/// remains owned by the server; the shared lock table can be injected through
/// the server options and is exposed here as a live inspection handle.
#[napi(object)]
pub struct P9SessionOptions {
    pub msize: Option<f64>,
    pub use_driver_ino: bool,
    pub read_only: bool,
    pub claim_ownership: bool,
    pub debug: bool,
    pub locks: Option<P9LockTable>,
}

type P9OptionValues = (
    String,
    u16,
    TransportP9ServerOptions,
    Option<JsTransportErrorCallback>,
    Option<JsP9SessionErrorCallback>,
    Option<JsP9AssertionCallback>,
);

fn p9_options(options: Option<P9ServerOptions>) -> Result<P9OptionValues, Error> {
    let options = options.unwrap_or(P9ServerOptions {
        port: None,
        host: None,
        path: None,
        allow_remote: None,
        socket_mode: None,
        allow_shared_directory: None,
        max_frame: None,
        max_in_flight: None,
        msize: None,
        use_driver_ino: None,
        read_only: None,
        claim_ownership: None,
        debug: None,
        locks: None,
        on_transport_error: None,
        on_error: None,
        on_assertion: None,
    });
    let on_transport_error = options.on_transport_error;
    let on_error = options.on_error;
    let on_assertion = options.on_assertion;
    if options.path.is_some() && (options.host.is_some() || options.port.is_some()) {
        return Err(config_error(
            "a 9P server uses either path or host/port, not both",
        ));
    }
    let host = options.host.unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = u16_number("port", options.port, 0)?;
    let mut output = TransportP9ServerOptions {
        host: host.clone(),
        port,
        path: options.path.map(Into::into),
        ..TransportP9ServerOptions::default()
    };
    output.locks = options.locks.map(|locks| locks.inner);
    output.allow_remote = options.allow_remote.unwrap_or(false);
    output.socket_mode = u32_number("socketMode", options.socket_mode, output.socket_mode)?;
    output.allow_shared_directory = options.allow_shared_directory.unwrap_or(false);
    output.max_frame = positive_number("maxFrame", options.max_frame, output.max_frame)?;
    output.max_in_flight =
        positive_number("maxInFlight", options.max_in_flight, output.max_in_flight)?;
    output.msize = options
        .msize
        .map(|value| u32_number("msize", Some(value), 0))
        .transpose()?;
    output.use_driver_ino = options.use_driver_ino.unwrap_or(true);
    output.read_only = options.read_only.unwrap_or(false);
    output.claim_ownership = options.claim_ownership.unwrap_or(true);
    output.debug = options.debug.unwrap_or(output.debug);
    Ok((
        host,
        port,
        output,
        on_transport_error,
        on_error,
        on_assertion,
    ))
}

type P9ServeFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

struct P9State {
    server: Option<Arc<TransportP9Server>>,
    serve_task: Option<P9ServeFuture>,
}

#[napi]
pub struct P9Session {
    inner: mount_rs_9p::P9Session,
    options: P9SessionOptions,
}

fn p9_lock_client(inner: TransportP9LockClient) -> P9LockClient {
    P9LockClient { inner }
}

fn p9_session_options(options: &TransportP9ServerOptions) -> P9SessionOptions {
    P9SessionOptions {
        msize: options.msize.map(f64::from),
        use_driver_ino: options.use_driver_ino,
        read_only: options.read_only,
        claim_ownership: options.claim_ownership,
        debug: options.debug,
        locks: options.locks.as_ref().map(|inner| P9LockTable {
            inner: inner.clone(),
        }),
    }
}

#[napi(object)]
pub struct P9User {
    pub uname: String,
    pub uid: Option<f64>,
    pub aname: String,
}

fn p9_user(user: mount_rs_9p::P9User) -> P9User {
    P9User {
        uname: user.uname,
        uid: user.uid.map(f64::from),
        aname: user.aname,
    }
}

#[napi(object)]
pub struct P9SessionStats {
    pub requests: f64,
    pub replies: f64,
    pub errors: f64,
    pub dropped: f64,
    pub flushed: f64,
    pub assertions: f64,
    pub messages: HashMap<String, f64>,
}

impl From<mount_rs_9p::P9SessionStats> for P9SessionStats {
    fn from(stats: mount_rs_9p::P9SessionStats) -> Self {
        Self {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            dropped: stats.dropped as f64,
            flushed: stats.flushed as f64,
            assertions: stats.assertions as f64,
            messages: stats
                .messages
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
        }
    }
}

#[napi]
impl P9Session {
    /// Handle one complete 9P frame without a socket. Malformed framing
    /// returns `null`; protocol and driver failures remain encoded as an
    /// `Rlerror`, matching the transport session contract.
    #[napi]
    pub async fn handle_call(&self, bytes: Buffer) -> Option<Buffer> {
        self.inner
            .handle_call(bytes.as_ref())
            .await
            .map(Buffer::from)
    }

    /// Tear down the session and release every driver handle it owns.
    #[napi]
    pub async fn destroy(&self) {
        self.inner.destroy().await;
    }

    /// Read-only N-API wrapper for the session-owned filesystem driver. The
    /// server retains the authoritative driver lifetime.
    #[napi(getter)]
    pub fn driver(&self) -> Filesystem {
        Filesystem::from_driver(self.inner.driver(), None, None)
    }

    /// The scalar policy used when this session was created.
    #[napi(getter)]
    pub fn options(&self) -> P9SessionOptions {
        P9SessionOptions {
            msize: self.options.msize,
            use_driver_ino: self.options.use_driver_ino,
            read_only: self.options.read_only,
            claim_ownership: self.options.claim_ownership,
            debug: self.options.debug,
            locks: self.options.locks.clone(),
        }
    }

    /// The attach identity recorded for a live fid, if any.
    #[napi]
    pub fn user_for(&self, fid: u32) -> Option<P9User> {
        self.inner.user_for(fid).map(p9_user)
    }

    /// The live byte-range lock handle owned by this session. Its client id is
    /// stable across getter calls and teardown releases the same ranges.
    #[napi(getter)]
    pub fn locks(&self) -> P9LockClient {
        p9_lock_client(self.inner.lock_client())
    }

    /// The live per-connection fid table. The table is backed by the same
    /// transport state used by protocol dispatch.
    #[napi(getter)]
    pub fn fids(&self) -> P9FidTable {
        P9FidTable {
            inner: P9FidTableTarget::Session(self.inner.clone()),
        }
    }

    #[napi(getter)]
    pub fn msize(&self) -> Option<u32> {
        self.inner.msize()
    }

    #[napi(getter)]
    pub fn version(&self) -> Option<String> {
        self.inner.version().map(str::to_owned)
    }

    #[napi(getter)]
    pub fn generation(&self) -> f64 {
        self.inner.generation() as f64
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.inner.destroyed()
    }

    #[napi(getter)]
    pub fn inflight(&self) -> f64 {
        self.inner.inflight() as f64
    }

    #[napi(getter)]
    pub fn stats(&self) -> P9SessionStats {
        self.inner.stats().into()
    }

    #[napi(getter)]
    pub fn assertions(&self) -> Vec<String> {
        self.inner.assertions()
    }
}

#[napi]
pub struct P9Connection {
    inner: mount_rs_9p::P9Connection,
    options: P9SessionOptions,
}

impl P9Connection {
    pub(crate) fn from_transport(
        inner: mount_rs_9p::P9Connection,
        options: &TransportP9ServerOptions,
    ) -> Self {
        Self {
            inner,
            options: p9_session_options(options),
        }
    }
}

#[napi]
impl P9Connection {
    #[napi(getter)]
    pub fn session(&self) -> P9Session {
        P9Session {
            inner: self.inner.session.clone(),
            options: P9SessionOptions {
                msize: self.options.msize,
                use_driver_ino: self.options.use_driver_ino,
                read_only: self.options.read_only,
                claim_ownership: self.options.claim_ownership,
                debug: self.options.debug,
                locks: self.options.locks.clone(),
            },
        }
    }

    #[napi(getter)]
    pub fn id(&self) -> f64 {
        self.inner.id() as f64
    }

    #[napi(getter)]
    pub fn peer(&self) -> Option<String> {
        self.inner.peer.clone()
    }

    #[napi(getter)]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.inner
            .close()
            .await
            .map_err(|error| transport_error("9P connection close", error))
    }

    #[napi]
    pub async fn wait_closed(&self) -> napi::Result<()> {
        self.inner.wait_closed().await;
        Ok(())
    }
}

#[napi]
pub struct P9Server {
    driver: Arc<dyn FsDriver>,
    options: TransportP9ServerOptions,
    host: String,
    requested_port: u16,
    state: Mutex<P9State>,
    binding: AtomicBool,
    closed: AtomicBool,
    transport_error: Option<Arc<TransportErrorCallback>>,
    session_error: Option<Arc<P9SessionErrorCallback>>,
    assertion: Option<Arc<P9AssertionCallback>>,
}

impl P9Server {
    /// Wrap the transport server retained by a native 9P mount. The wrapper
    /// intentionally does not duplicate callbacks or the serving task: both
    /// remain owned by the mount-created server, while lifecycle methods and
    /// live client/session views operate on the same Arc-backed transport.
    pub(crate) fn from_transport(server: Arc<TransportP9Server>) -> Self {
        let options = server.options().clone();
        Self {
            driver: server.driver(),
            host: options.host.clone(),
            requested_port: options.port,
            options,
            state: Mutex::new(P9State {
                server: Some(server),
                serve_task: None,
            }),
            binding: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            transport_error: None,
            session_error: None,
            assertion: None,
        }
    }
}

#[napi]
impl P9Server {
    /// Create a session for the JavaScript duplex-stream adapter. The method
    /// is intentionally internal: the public Node contract is `attach`, while
    /// this native seam supplies the same shared driver/lock state without
    /// making N-API own a JavaScript stream from a Tokio task.
    #[napi(js_name = "_createAttachedSession")]
    pub fn create_attached_session(&self) -> P9Session {
        P9Session {
            inner: mount_rs_9p::P9Session::with_options_and_hooks(
                Arc::clone(&self.driver),
                self.options.session_options(),
                p9_session_hooks(self.session_error.as_ref(), self.assertion.as_ref()),
            ),
            options: p9_session_options(&self.options),
        }
    }

    /// The configured server policy, including the session settings applied to
    /// both native-listener and attached-stream connections. The callback is
    /// intentionally not reflected because its native lifetime is owned by the
    /// transport hook rather than exposed as a reusable N-API function.
    #[napi(getter)]
    pub fn options(&self) -> P9ServerOptions {
        P9ServerOptions {
            port: Some(f64::from(self.options.port)),
            host: Some(self.host.clone()),
            path: self
                .options
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            allow_remote: Some(self.options.allow_remote),
            socket_mode: Some(f64::from(self.options.socket_mode)),
            allow_shared_directory: Some(self.options.allow_shared_directory),
            max_frame: Some(self.options.max_frame as f64),
            max_in_flight: Some(self.options.max_in_flight as f64),
            msize: self.options.msize.map(f64::from),
            use_driver_ino: Some(self.options.use_driver_ino),
            read_only: Some(self.options.read_only),
            claim_ownership: Some(self.options.claim_ownership),
            debug: Some(self.options.debug),
            on_transport_error: None,
            on_error: None,
            on_assertion: None,
            locks: self.options.locks.as_ref().map(|inner| P9LockTable {
                inner: inner.clone(),
            }),
        }
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn path(&self) -> Option<String> {
        self.options
            .path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        let state = self.state.lock().expect("9P state lock");
        state
            .server
            .as_ref()
            .and_then(|server| server.local_addr().ok())
            .map_or(self.requested_port, |address| address.port()) as u32
    }

    #[napi(getter)]
    pub fn connections(&self) -> u32 {
        let state = self.state.lock().expect("9P state lock");
        state
            .server
            .as_ref()
            .and_then(|server| server.connection_count().ok())
            .unwrap_or(0) as u32
    }

    #[napi]
    pub fn address(&self) -> Option<String> {
        let state = self.state.lock().expect("9P state lock");
        let Some(server) = state.server.as_ref() else {
            return self.path();
        };
        if let Some(path) = server.unix_path() {
            return Some(path.to_string_lossy().into_owned());
        }
        server.local_addr().ok().map(|address| address.to_string())
    }

    #[napi(getter)]
    pub fn clients(&self) -> napi::Result<Vec<P9Connection>> {
        let state = self.state.lock().expect("9P state lock");
        state.server.as_ref().map_or(Ok(Vec::new()), |server| {
            server
                .clients()
                .map(|clients| {
                    clients
                        .into_iter()
                        .map(|inner| P9Connection {
                            inner,
                            options: p9_session_options(&self.options),
                        })
                        .collect()
                })
                .map_err(|error| transport_error("9P clients", error))
        })
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("9P listen", "server is closed"));
        }
        {
            let state = self.state.lock().expect("9P state lock");
            if state.server.is_some() {
                return Ok(());
            }
        }
        if self
            .binding
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(transport_error("9P listen", "server is already starting"));
        }
        let result = TransportP9Server::bind_arc_with_hooks_and_session_hooks(
            Arc::clone(&self.driver),
            self.options.clone(),
            p9_hooks(self.transport_error.as_ref()),
            p9_session_hooks(self.session_error.as_ref(), self.assertion.as_ref()),
        )
        .await
        .map(Arc::new)
        .map_err(|error| transport_error("9P listen", error));
        let server = match result {
            Ok(server) => server,
            Err(error) => {
                self.binding.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let task = match server.start() {
            Ok(task) => task,
            Err(error) => {
                let close_result = server.close().await;
                self.binding.store(false, Ordering::Release);
                if let Err(close_error) = close_result {
                    return Err(transport_error("9P close", close_error));
                }
                return Err(transport_error("9P listen", error));
            }
        };
        let serve_task: P9ServeFuture = Box::pin(async move {
            let _ = task.await;
        });
        if self.closed.load(Ordering::Acquire) {
            server
                .close()
                .await
                .map_err(|error| transport_error("9P close", error))?;
            serve_task.await;
            self.binding.store(false, Ordering::Release);
            return Err(transport_error(
                "9P listen",
                "server was closed while binding",
            ));
        }
        {
            let mut state = self.state.lock().expect("9P state lock");
            state.server = Some(server);
            state.serve_task = Some(serve_task);
        }
        self.binding.store(false, Ordering::Release);
        Ok(())
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closed.store(true, Ordering::Release);
        if let Some(callback) = &self.transport_error {
            callback.release();
        }
        if let Some(callback) = &self.session_error {
            callback.release();
        }
        if let Some(callback) = &self.assertion {
            callback.release();
        }
        let (server, task) = {
            let mut state = self.state.lock().expect("9P state lock");
            (state.server.take(), state.serve_task.take())
        };
        if let Some(server) = server {
            server
                .close()
                .await
                .map_err(|error| transport_error("9P close", error))?;
        }
        if let Some(task) = task {
            task.await;
        }
        Ok(())
    }
}

#[napi]
pub fn create_p9_server(
    driver: &Filesystem,
    options: Option<P9ServerOptions>,
) -> napi::Result<P9Server> {
    let (host, requested_port, mut options, on_transport_error, on_error, on_assertion) =
        p9_options(options)?;
    // The native listener and the JavaScript attach seam must share byte-range
    // lock ownership when they are used on the same public server object.
    if options.locks.is_none() {
        options.locks = Some(TransportP9LockTable::new(Default::default()));
    }
    let transport_error = on_transport_error
        .map(TransportErrorCallback::new)
        .transpose()?;
    let session_error = on_error.map(P9SessionErrorCallback::new).transpose()?;
    let assertion = on_assertion.map(P9AssertionCallback::new).transpose()?;
    Ok(P9Server {
        driver: Arc::clone(&driver.driver),
        options,
        host,
        requested_port,
        state: Mutex::new(P9State {
            server: None,
            serve_task: None,
        }),
        binding: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        transport_error,
        session_error,
        assertion,
    })
}

#[napi(object)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[napi(object)]
pub struct S3ServerOptions {
    pub bucket: Option<String>,
    pub host: Option<String>,
    pub port: Option<f64>,
    pub credentials: Option<S3Credentials>,
    pub region: Option<String>,
    pub max_body_bytes: Option<f64>,
    pub max_xml_bytes: Option<f64>,
    pub read_chunk_bytes: Option<f64>,
    pub drain_timeout: Option<f64>,
    pub debug: Option<bool>,
    #[napi(ts_type = "(error: unknown, peer: string | undefined) => void")]
    pub on_transport_error: Option<JsTransportErrorCallback>,
}

type S3FactoryOptions = (
    String,
    u16,
    Option<String>,
    TransportS3ServerOptions,
    S3SessionOptions,
    Option<JsTransportErrorCallback>,
);

fn s3_options(options: Option<S3ServerOptions>) -> Result<S3FactoryOptions, Error> {
    let options = options.unwrap_or(S3ServerOptions {
        bucket: None,
        host: None,
        port: None,
        credentials: None,
        region: None,
        max_body_bytes: None,
        max_xml_bytes: None,
        read_chunk_bytes: None,
        drain_timeout: None,
        debug: None,
        on_transport_error: None,
    });
    let on_transport_error = options.on_transport_error;
    let (host, address) = ip_host(options.host, "127.0.0.1")?;
    let port = u16_number("port", options.port, 0)?;
    let credentials = options.credentials.map(|credentials| {
        TransportS3Credentials::new(credentials.access_key_id, credentials.secret_access_key)
    });
    if credentials.is_none() && !address.is_loopback() {
        return Err(config_error(
            "unauthenticated S3 servers must bind a loopback address",
        ));
    }
    let mut session = S3SessionOptions::default();
    session.credentials = credentials;
    session.region = options.region;
    session.max_body_bytes = positive_number(
        "maxBodyBytes",
        options.max_body_bytes,
        session.max_body_bytes,
    )?;
    session.max_xml_bytes =
        positive_number("maxXmlBytes", options.max_xml_bytes, session.max_xml_bytes)?;
    session.read_chunk_bytes = positive_number(
        "readChunkBytes",
        options.read_chunk_bytes,
        session.read_chunk_bytes,
    )?;
    session.debug = options.debug.unwrap_or(session.debug);
    let mut server_options = TransportS3ServerOptions {
        host: address,
        port,
        ..TransportS3ServerOptions::default()
    };
    server_options.drain_timeout = duration_ms(
        "drainTimeout",
        options.drain_timeout,
        server_options.drain_timeout,
    )?;
    Ok((
        host,
        port,
        options.bucket,
        server_options,
        session,
        on_transport_error,
    ))
}

type S3BucketEntries = Vec<(String, Arc<dyn FsDriver>)>;

fn s3_buckets(
    source: Either<&Filesystem, Object<'_>>,
    bucket: Option<String>,
) -> Result<S3BucketEntries, Error> {
    match source {
        Either::A(driver) => {
            let bucket = bucket.unwrap_or_else(|| "mountx".to_owned());
            if !valid_s3_bucket_name(&bucket) {
                return Err(config_error(format!("invalid S3 bucket name {bucket:?}")));
            }
            Ok(vec![(bucket, Arc::clone(&driver.driver))])
        }
        Either::B(source) => {
            let buckets = source
                .get_named_property::<Object<'_>>("buckets")
                .map_err(|_| config_error("S3 source must contain a buckets object"))?;
            let mut entries = Vec::new();
            for name in Object::keys(&buckets)
                .map_err(|error| config_error(format!("could not enumerate S3 buckets: {error}")))?
            {
                if !valid_s3_bucket_name(&name) {
                    return Err(config_error(format!("invalid S3 bucket name {name:?}")));
                }
                let driver = buckets
                    .get_named_property_unchecked::<Reference<Filesystem>>(&name)
                    .map_err(|_| {
                        config_error(format!("S3 bucket {name:?} must contain a Filesystem"))
                    })?;
                entries.push((name, Arc::clone(&driver.driver)));
            }
            Ok(entries)
        }
    }
}

#[napi(object)]
pub struct S3SessionStats {
    pub requests: f64,
    pub replies: f64,
    pub errors: f64,
    pub operations: HashMap<String, f64>,
    pub assertions: f64,
    pub duration_ms_total: f64,
    pub duration_ms_max: f64,
    pub request_bytes: f64,
    pub response_bytes: f64,
    pub error_classes: HashMap<String, f64>,
}

#[napi(object)]
pub struct S3SessionOptionsView {
    /// Whether SigV4 credentials were configured. Secret material is never
    /// returned through the inspection surface.
    pub credentials_configured: bool,
    pub region: Option<String>,
    pub max_body_bytes: f64,
    pub max_xml_bytes: f64,
    pub read_chunk_bytes: f64,
    pub multipart_staging_ttl_ms: f64,
    pub multipart_staging_max_bytes: f64,
    pub debug: bool,
}

#[derive(Clone)]
#[napi(object)]
pub struct S3Header {
    pub name: String,
    pub value: String,
}

#[napi(object)]
pub struct S3RequestHead {
    pub method: String,
    pub target: String,
    pub headers: Vec<S3Header>,
}

#[napi(object)]
pub struct S3Response {
    pub status: u32,
    pub headers: Vec<S3Header>,
    #[napi(ts_type = "Buffer")]
    pub body: Buffer,
}

impl From<TransportS3Response> for S3Response {
    fn from(response: TransportS3Response) -> Self {
        Self {
            status: response.status as u32,
            headers: response
                .headers
                .into_iter()
                .map(|(name, value)| S3Header { name, value })
                .collect(),
            body: Buffer::from(response.body),
        }
    }
}

/// Adapt a JavaScript Web ReadableStream to the transport's incremental S3
/// request-body contract. `Reader` performs one JS read at a time, preserving
/// the transport's existing backpressure and avoiding a second body buffer at
/// the N-API boundary.
struct NapiS3RequestBody {
    reader: Reader<Buffer>,
}

impl FuturesStream for NapiS3RequestBody {
    type Item = std::result::Result<Vec<u8>, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.reader).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(buffer))) => Poll::Ready(Some(Ok(buffer.to_vec()))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error.to_string()))),
        }
    }
}

enum S3ResponseBodyState {
    Bytes(Option<Vec<u8>>),
    Stream(Option<TransportS3ResponseBodyStream>),
    Closed,
}

/// A pull-based S3 response body retained by the JavaScript facade. Buffered
/// protocol responses and file-backed object streams share the same async
/// iterator shape, while stream variants remain owned by the transport until
/// JavaScript asks for each next chunk.
#[derive(Clone)]
#[napi]
pub struct S3BodyStream {
    inner: Arc<tokio::sync::Mutex<S3ResponseBodyState>>,
}

impl S3BodyStream {
    fn new(body: TransportS3StreamBody) -> Self {
        let state = match body {
            TransportS3StreamBody::Bytes(bytes) => S3ResponseBodyState::Bytes(Some(bytes)),
            TransportS3StreamBody::Stream(stream) => S3ResponseBodyState::Stream(Some(stream)),
        };
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(state)),
        }
    }
}

#[napi]
impl S3BodyStream {
    /// Read one response chunk, or `null` after EOF. A single mutex prevents
    /// concurrent JavaScript pulls from advancing the transport stream twice.
    #[napi]
    pub async fn read_chunk(&self) -> napi::Result<Option<Buffer>> {
        let mut state = self.inner.lock().await;
        let current = std::mem::replace(&mut *state, S3ResponseBodyState::Closed);
        match current {
            S3ResponseBodyState::Bytes(Some(bytes)) => Ok(Some(Buffer::from(bytes))),
            S3ResponseBodyState::Bytes(None)
            | S3ResponseBodyState::Stream(None)
            | S3ResponseBodyState::Closed => Ok(None),
            S3ResponseBodyState::Stream(Some(mut stream)) => {
                let next = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
                match next {
                    None => Ok(None),
                    Some(Ok(bytes)) => {
                        *state = S3ResponseBodyState::Stream(Some(stream));
                        Ok(Some(Buffer::from(bytes)))
                    }
                    Some(Err(error)) => Err(transport_error("S3 response body", error)),
                }
            }
        }
    }

    /// Close an unread or partially-read response body. The JavaScript
    /// async-iterator facade calls this from `return()` on cancellation;
    /// dropping the transport stream also signals file-reader cancellation.
    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        let mut state = self.inner.lock().await;
        let _ = std::mem::replace(&mut *state, S3ResponseBodyState::Closed);
        Ok(())
    }
}

#[napi]
pub struct S3StreamResponse {
    status: u32,
    headers: Vec<S3Header>,
    body: Option<S3BodyStream>,
}

#[napi]
impl S3StreamResponse {
    #[napi(getter)]
    pub fn status(&self) -> u32 {
        self.status
    }

    #[napi(getter)]
    pub fn headers(&self) -> Vec<S3Header> {
        self.headers.clone()
    }

    #[napi(getter, ts_return_type = "AsyncIterable<Uint8Array> | null")]
    pub fn body(&self) -> Option<S3BodyStream> {
        self.body.clone()
    }
}

fn s3_stream_response(response: TransportS3StreamResponse) -> S3StreamResponse {
    S3StreamResponse {
        status: response.status as u32,
        headers: response
            .headers
            .into_iter()
            .map(|(name, value)| S3Header { name, value })
            .collect(),
        body: response.body.map(S3BodyStream::new),
    }
}

struct S3StreamRequestState {
    session: Arc<TransportS3Session>,
    head: TransportS3RequestHead,
    body: TransportS3RequestBody,
}

/// The synchronous N-API entrypoint safely captures the JavaScript stream
/// reader. The transport future runs through `run` after that non-Send JS
/// wrapper has left the N-API call frame.
#[napi]
pub struct S3StreamRequest {
    inner: tokio::sync::Mutex<Option<S3StreamRequestState>>,
}

#[napi]
impl S3StreamRequest {
    #[napi]
    pub async fn run(&self) -> napi::Result<S3StreamResponse> {
        let request = self
            .inner
            .lock()
            .await
            .take()
            .ok_or_else(|| transport_error("S3 request stream", "request already run"))?;
        let response = request
            .session
            .handle_request_stream(request.head, request.body)
            .await;
        Ok(s3_stream_response(response))
    }
}

fn transport_s3_head(head: S3RequestHead) -> TransportS3RequestHead {
    TransportS3RequestHead {
        method: head.method,
        target: head.target,
        headers: head
            .headers
            .into_iter()
            .map(|header| TransportS3HeaderEntry::new(header.name, header.value))
            .collect(),
    }
}

/// Read-only N-API view of the in-process S3 session shared by a server.
/// Request dispatch remains owned by the Rust session; this view exposes the
/// streaming boundary without taking ownership of the underlying drivers.
#[napi]
pub struct S3Session {
    inner: Arc<TransportS3Session>,
}

#[napi]
impl S3Session {
    /// Handle one in-process, buffered S3 request without an HTTP socket.
    #[napi]
    pub async fn handle_request(&self, head: S3RequestHead, body: Option<Buffer>) -> S3Response {
        self.inner
            .handle_request(
                transport_s3_head(head),
                body.map(|value| value.to_vec()).unwrap_or_default(),
            )
            .await
            .into()
    }

    /// Capture one in-process S3 request body stream without entering the
    /// async N-API future while it still owns a JavaScript stream wrapper.
    #[napi]
    pub fn handle_request_stream(
        &self,
        head: S3RequestHead,
        body: ReadableStream<'_, Buffer>,
    ) -> napi::Result<S3StreamRequest> {
        let reader = body
            .read()
            .map_err(|error| transport_error("S3 request body", error))?;
        Ok(S3StreamRequest {
            inner: tokio::sync::Mutex::new(Some(S3StreamRequestState {
                session: Arc::clone(&self.inner),
                head: transport_s3_head(head),
                body: Box::pin(NapiS3RequestBody { reader }),
            })),
        })
    }

    #[napi(getter)]
    pub fn bucket_names(&self) -> Vec<String> {
        self.inner.bucket_names()
    }

    /// Read-only N-API wrappers for the session-owned bucket drivers. Each
    /// wrapper shares the transport's driver state; shutting down the view
    /// does not tear down the server-owned session driver.
    #[napi(getter, ts_return_type = "Record<string, Filesystem>")]
    pub fn buckets(&self, env: Env) -> napi::Result<Object<'_>> {
        let mut buckets = Object::new(&env)?;
        for (name, driver) in self.inner.buckets.iter() {
            buckets.set(
                name,
                Filesystem::from_driver(Arc::clone(driver), None, None),
            )?;
        }
        Ok(buckets)
    }

    #[napi(getter)]
    pub fn options(&self) -> S3SessionOptionsView {
        let options = &self.inner.options;
        S3SessionOptionsView {
            credentials_configured: options.credentials.is_some(),
            region: options.region.clone(),
            max_body_bytes: options.max_body_bytes as f64,
            max_xml_bytes: options.max_xml_bytes as f64,
            read_chunk_bytes: options.read_chunk_bytes as f64,
            multipart_staging_ttl_ms: options.multipart_staging_ttl_ms as f64,
            multipart_staging_max_bytes: options.multipart_staging_max_bytes as f64,
            debug: options.debug,
        }
    }

    #[napi(getter)]
    pub fn assertions(&self) -> Vec<String> {
        self.inner.assertions()
    }

    /// Read a coherent snapshot of the transport-owned session metrics.
    #[napi]
    pub async fn stats(&self) -> S3SessionStats {
        let stats = self.inner.stats().await;
        S3SessionStats {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            operations: stats
                .operations
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
            assertions: stats.assertions as f64,
            duration_ms_total: stats.duration_ms_total as f64,
            duration_ms_max: stats.duration_ms_max as f64,
            request_bytes: stats.request_bytes as f64,
            response_bytes: stats.response_bytes as f64,
            error_classes: stats
                .error_classes
                .into_iter()
                .map(|(class, count)| (format!("{class:?}"), count as f64))
                .collect(),
        }
    }
}

#[napi]
pub struct S3Server {
    session: Arc<TransportS3Session>,
    options: TransportS3ServerOptions,
    host: String,
    requested_port: u16,
    buckets: Vec<String>,
    running: Mutex<Option<Arc<TransportS3Server>>>,
    binding: AtomicBool,
    closed: AtomicBool,
    transport_error: Option<Arc<TransportErrorCallback>>,
}

#[napi]
impl S3Server {
    #[napi(getter)]
    pub fn session(&self) -> S3Session {
        S3Session {
            inner: Arc::clone(&self.session),
        }
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        let running = self.running.lock().expect("S3 server lock");
        running
            .as_ref()
            .map_or(self.requested_port, |server| server.address().port()) as u32
    }

    #[napi(getter)]
    pub fn connections(&self) -> u32 {
        self.running
            .lock()
            .expect("S3 server lock")
            .as_ref()
            .map_or(0, |server| server.connections()) as u32
    }

    #[napi(getter)]
    pub fn url(&self) -> String {
        format!("http://{}:{}", bracketed_host(&self.host), self.port())
    }

    #[napi(getter)]
    pub fn buckets(&self) -> Vec<String> {
        self.buckets.clone()
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("S3 listen", "server is closed"));
        }
        {
            let running = self.running.lock().expect("S3 server lock");
            if running.is_some() {
                return Ok(());
            }
        }
        if self
            .binding
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(transport_error("S3 listen", "server is already starting"));
        }
        let result = TransportS3Server::start_with_hooks(
            Arc::clone(&self.session),
            self.options.clone(),
            s3_hooks(self.transport_error.as_ref()),
        )
        .await
        .map(Arc::new)
        .map_err(|error| transport_error("S3 listen", error));
        match result {
            Ok(server) => {
                if self.closed.load(Ordering::Acquire) {
                    server
                        .close()
                        .await
                        .map_err(|error| transport_error("S3 close", error))?;
                    self.binding.store(false, Ordering::Release);
                    return Err(transport_error(
                        "S3 listen",
                        "server was closed while binding",
                    ));
                }
                *self.running.lock().expect("S3 server lock") = Some(server);
                self.binding.store(false, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                self.binding.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closed.store(true, Ordering::Release);
        if let Some(callback) = &self.transport_error {
            callback.release();
        }
        let running = self.running.lock().expect("S3 server lock").take();
        if let Some(server) = running {
            server
                .close()
                .await
                .map_err(|error| transport_error("S3 close", error))?;
        } else {
            self.session
                .close()
                .await
                .map_err(|error| transport_error("S3 session close", error))?;
        }
        Ok(())
    }
}

#[napi]
pub fn create_s3_server(
    #[napi(ts_arg_type = "Filesystem | { buckets: Record<string, Filesystem> }")] source: Either<
        &Filesystem,
        Object<'_>,
    >,
    options: Option<S3ServerOptions>,
) -> napi::Result<S3Server> {
    let (host, requested_port, bucket, server_options, session_options, on_transport_error) =
        s3_options(options)?;
    let buckets = s3_buckets(source, bucket)?;
    let session = Arc::new(TransportS3Session::from_buckets_with_options(
        buckets,
        session_options,
    ));
    let buckets = session.bucket_names();
    let transport_error = on_transport_error
        .map(TransportErrorCallback::new)
        .transpose()?;
    Ok(S3Server {
        session,
        options: server_options,
        host,
        requested_port,
        buckets,
        running: Mutex::new(None),
        binding: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        transport_error,
    })
}

#[napi(object)]
pub struct WebdavCredentials {
    pub username: String,
    pub password: String,
}

#[napi(object)]
pub struct WebdavLockOptions {
    pub default_timeout_seconds: Option<f64>,
    pub max_timeout_seconds: Option<f64>,
    pub max_locks: Option<f64>,
}

#[napi(object)]
pub struct WebdavServerOptions {
    pub host: Option<String>,
    pub port: Option<f64>,
    pub credentials: Option<WebdavCredentials>,
    pub realm: Option<String>,
    pub read_chunk_bytes: Option<f64>,
    pub max_xml_bytes: Option<f64>,
    pub max_body_bytes: Option<f64>,
    pub locks: Option<WebdavLockOptions>,
    pub drain_timeout: Option<f64>,
    pub debug: Option<bool>,
    #[napi(ts_type = "(error: unknown, peer: string | undefined) => void")]
    pub on_transport_error: Option<JsTransportErrorCallback>,
    #[napi(ts_type = "(error: unknown, head: WebdavRequestHead | undefined) => void")]
    pub on_error: Option<JsWebdavErrorCallback>,
}

#[napi(object)]
pub struct WebdavLockOptionsView {
    pub default_timeout_seconds: f64,
    pub max_timeout_seconds: f64,
    pub max_locks: f64,
}

#[napi(object)]
pub struct WebdavSessionOptionsView {
    pub credentials: Option<WebdavCredentials>,
    pub realm: String,
    pub read_chunk_bytes: f64,
    pub max_xml_bytes: f64,
    pub max_body_bytes: Option<f64>,
    pub locks: WebdavLockOptionsView,
    pub debug: bool,
}

type ParsedWebdavOptions = (
    String,
    u16,
    TransportWebdavServerOptions,
    Option<JsTransportErrorCallback>,
    Option<JsWebdavErrorCallback>,
);

fn webdav_options(options: Option<WebdavServerOptions>) -> Result<ParsedWebdavOptions, Error> {
    let options = options.unwrap_or(WebdavServerOptions {
        host: None,
        port: None,
        credentials: None,
        realm: None,
        read_chunk_bytes: None,
        max_xml_bytes: None,
        max_body_bytes: None,
        locks: None,
        drain_timeout: None,
        debug: None,
        on_transport_error: None,
        on_error: None,
    });
    let on_transport_error = options.on_transport_error;
    let on_error = options.on_error;
    let host = options.host.unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = u16_number("port", options.port, 0)?;
    let mut output = TransportWebdavServerOptions {
        host: host.clone(),
        port,
        ..TransportWebdavServerOptions::default()
    };
    if let Some(credentials) = options.credentials {
        output = output.with_credentials(credentials.username, credentials.password);
    }
    if let Some(realm) = options.realm {
        output.session.realm = realm;
    }
    output.session.read_chunk_bytes = positive_number(
        "readChunkBytes",
        options.read_chunk_bytes,
        output.session.read_chunk_bytes,
    )?;
    output.session.max_xml_bytes = positive_number(
        "maxXmlBytes",
        options.max_xml_bytes,
        output.session.max_xml_bytes,
    )?;
    if let Some(max_body_bytes) = options.max_body_bytes {
        let max_body_bytes = positive_number(
            "maxBodyBytes",
            Some(max_body_bytes),
            output.max_request_bytes,
        )?;
        output.session.max_body_bytes = Some(max_body_bytes);
        output.max_request_bytes = max_body_bytes;
    }
    if let Some(locks) = options.locks {
        output.session.locks.default_timeout_seconds = positive_number(
            "defaultTimeoutSeconds",
            locks.default_timeout_seconds,
            output.session.locks.default_timeout_seconds as usize,
        )? as u64;
        output.session.locks.max_timeout_seconds = positive_number(
            "maxTimeoutSeconds",
            locks.max_timeout_seconds,
            output.session.locks.max_timeout_seconds as usize,
        )? as u64;
        output.session.locks.max_locks =
            positive_number("maxLocks", locks.max_locks, output.session.locks.max_locks)?;
    }
    output.drain_timeout =
        duration_ms("drainTimeout", options.drain_timeout, output.drain_timeout)?;
    output.session.debug = options.debug.unwrap_or(output.session.debug);
    Ok((host, port, output, on_transport_error, on_error))
}

#[napi(object)]
pub struct WebdavSessionStats {
    pub requests: f64,
    pub replies: f64,
    pub errors: f64,
    pub methods: HashMap<String, f64>,
    pub assertions: f64,
}

#[napi(object)]
pub struct WebdavXmlNode {
    pub name: String,
    pub ns: String,
    pub text: String,
    pub children: Vec<WebdavXmlNode>,
}

fn webdav_xml_node(node: TransportWebdavXmlNode) -> WebdavXmlNode {
    WebdavXmlNode {
        name: node.name,
        ns: node.ns,
        text: node.text,
        children: node.children.into_iter().map(webdav_xml_node).collect(),
    }
}

#[napi(object)]
pub struct WebdavLockView {
    pub token: String,
    pub path: String,
    pub collection: bool,
    pub depth: String,
    pub exclusive: bool,
    pub owner: Option<WebdavXmlNode>,
    pub timeout_seconds: f64,
    pub expires_at: f64,
}

#[derive(Clone)]
#[napi(object)]
pub struct WebdavHeader {
    pub name: String,
    pub value: String,
}

#[derive(Clone)]
#[napi(object)]
pub struct WebdavRequestHead {
    pub method: String,
    pub target: String,
    pub headers: Vec<WebdavHeader>,
}

#[napi(object)]
pub struct WebdavResponse {
    pub status: u32,
    pub headers: Vec<WebdavHeader>,
    #[napi(ts_type = "Buffer | null")]
    pub body: Option<Buffer>,
}

/// Adapt a JavaScript Web ReadableStream to the transport's pull-based body
/// contract. `Reader` performs one JS read at a time, so the Rust session can
/// apply its existing request backpressure without materializing the body.
struct NapiWebdavRequestBody {
    reader: Reader<Buffer>,
}

impl WebdavRequestBody for NapiWebdavRequestBody {
    fn poll_next_chunk(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Bytes, TransportWebdavBodyError>>> {
        match Pin::new(&mut self.reader).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(buffer))) => Poll::Ready(Some(Ok(Bytes::from(buffer.to_vec())))),
            Poll::Ready(Some(Err(error))) => {
                Poll::Ready(Some(Err(TransportWebdavBodyError::Body(error.to_string()))))
            }
        }
    }
}

enum WebdavResponseBodyState {
    Bytes(Option<Vec<u8>>),
    File {
        handle: Arc<dyn CoreFileHandle>,
        position: u64,
        end: u64,
        chunk_size: usize,
    },
    Closed,
}

/// A pull-based response body retained by the JavaScript WebDAV facade.
/// Files stay as positional transport handles until JavaScript asks for each
/// next chunk; only small protocol-generated byte bodies are already buffered
/// by the transport itself.
#[derive(Clone)]
#[napi]
pub struct WebdavBodyStream {
    inner: Arc<tokio::sync::Mutex<WebdavResponseBodyState>>,
}

impl WebdavBodyStream {
    fn new(body: TransportWebdavBody) -> Self {
        let state = match body {
            TransportWebdavBody::Bytes(bytes) => WebdavResponseBodyState::Bytes(Some(bytes)),
            TransportWebdavBody::File(file) => WebdavResponseBodyState::File {
                handle: file.handle,
                position: file.start,
                end: file.start.saturating_add(file.length),
                chunk_size: file.chunk_size.max(1),
            },
        };
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(state)),
        }
    }

    async fn close_handle(handle: Arc<dyn CoreFileHandle>) -> napi::Result<()> {
        handle
            .close()
            .await
            .map_err(|error| transport_error("WebDAV response body", error))
    }
}

impl Drop for WebdavBodyStream {
    fn drop(&mut self) {
        let Some(mutex) = Arc::get_mut(&mut self.inner) else {
            return;
        };
        let state = std::mem::replace(mutex.get_mut(), WebdavResponseBodyState::Closed);
        let WebdavResponseBodyState::File { handle, .. } = state else {
            return;
        };
        if let Ok(runtime) = napi::tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = handle.close().await;
            });
        }
    }
}

#[napi]
impl WebdavBodyStream {
    /// Read one response chunk, or `null` after EOF. A single mutex protects
    /// the positional file read and makes concurrent JavaScript pulls safe.
    #[napi]
    pub async fn read_chunk(&self) -> napi::Result<Option<Buffer>> {
        let mut state = self.inner.lock().await;
        let current = std::mem::replace(&mut *state, WebdavResponseBodyState::Closed);
        match current {
            WebdavResponseBodyState::Bytes(Some(bytes)) => Ok(Some(Buffer::from(bytes))),
            WebdavResponseBodyState::Bytes(None) | WebdavResponseBodyState::Closed => Ok(None),
            WebdavResponseBodyState::File {
                handle,
                position,
                end,
                chunk_size: _,
            } if position >= end => {
                Self::close_handle(handle).await?;
                Ok(None)
            }
            WebdavResponseBodyState::File {
                handle,
                position,
                end,
                chunk_size,
            } => {
                let wanted = (end - position).min(chunk_size as u64) as usize;
                let mut bytes = vec![0_u8; wanted];
                let count = match handle.read(&mut bytes, Some(position)).await {
                    Ok(count) => count,
                    Err(error) => {
                        let _ = handle.close().await;
                        return Err(transport_error("WebDAV response body", error));
                    }
                };
                if count == 0 || count > wanted {
                    let _ = handle.close().await;
                    return Err(transport_error(
                        "WebDAV response body",
                        FsError::new(ErrorCode::Eio)
                            .with_syscall("read")
                            .with_message("invalid WebDAV response body read length"),
                    ));
                }
                *state = WebdavResponseBodyState::File {
                    handle,
                    position: position.saturating_add(count as u64),
                    end,
                    chunk_size,
                };
                bytes.truncate(count);
                Ok(Some(Buffer::from(bytes)))
            }
        }
    }

    /// Close an unread or partially-read response body. The JavaScript
    /// async-iterator facade calls this from `return()` on cancellation.
    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        let mut state = self.inner.lock().await;
        let current = std::mem::replace(&mut *state, WebdavResponseBodyState::Closed);
        if let WebdavResponseBodyState::File { handle, .. } = current {
            Self::close_handle(handle).await?;
        }
        Ok(())
    }
}

#[napi]
pub struct WebdavStreamResponse {
    status: u32,
    headers: Vec<WebdavHeader>,
    body: Option<WebdavBodyStream>,
}

#[napi]
impl WebdavStreamResponse {
    #[napi(getter)]
    pub fn status(&self) -> u32 {
        self.status
    }

    #[napi(getter)]
    pub fn headers(&self) -> Vec<WebdavHeader> {
        self.headers.clone()
    }

    #[napi(getter, ts_return_type = "AsyncIterable<Uint8Array> | null")]
    pub fn body(&self) -> Option<WebdavBodyStream> {
        self.body.clone()
    }
}

fn webdav_stream_response(response: TransportWebdavResponse) -> WebdavStreamResponse {
    WebdavStreamResponse {
        status: response.status as u32,
        headers: response
            .headers
            .into_iter()
            .map(|(name, value)| WebdavHeader { name, value })
            .collect(),
        body: response.body.map(WebdavBodyStream::new),
    }
}

struct WebdavStreamRequestState {
    session: Arc<TransportWebdavSession>,
    head: TransportWebdavRequestHead,
    body: NapiWebdavRequestBody,
}

/// The synchronous N-API entrypoint can safely take ownership of the
/// JavaScript ReadableStream reader. The actual transport future runs through
/// this class's async `run` method, after the non-Send JS wrapper is gone.
#[napi]
pub struct WebdavStreamRequest {
    inner: tokio::sync::Mutex<Option<WebdavStreamRequestState>>,
}

#[napi]
impl WebdavStreamRequest {
    #[napi]
    pub async fn run(&self) -> napi::Result<WebdavStreamResponse> {
        let request = self
            .inner
            .lock()
            .await
            .take()
            .ok_or_else(|| transport_error("WebDAV request stream", "request already run"))?;
        let response = request
            .session
            .handle_request_stream(request.head, request.body)
            .await;
        Ok(webdav_stream_response(response))
    }
}

async fn webdav_response(response: TransportWebdavResponse) -> napi::Result<WebdavResponse> {
    let body = match response.body {
        Some(body) => {
            Some(Buffer::from(body.into_bytes().await.map_err(|error| {
                transport_error("WebDAV response body", error)
            })?))
        }
        None => None,
    };
    Ok(WebdavResponse {
        status: response.status as u32,
        headers: response
            .headers
            .into_iter()
            .map(|(name, value)| WebdavHeader { name, value })
            .collect(),
        body,
    })
}

fn transport_webdav_head(head: WebdavRequestHead) -> TransportWebdavRequestHead {
    TransportWebdavRequestHead {
        method: head.method,
        target: head.target,
        headers: head
            .headers
            .into_iter()
            .map(|header| (header.name.to_ascii_lowercase(), header.value))
            .collect(),
    }
}

fn webdav_request_head(head: TransportWebdavRequestHead) -> WebdavRequestHead {
    WebdavRequestHead {
        method: head.method,
        target: head.target,
        headers: head
            .headers
            .into_iter()
            .map(|(name, value)| WebdavHeader { name, value })
            .collect(),
    }
}

/// Read-only N-API view of the in-process WebDAV session shared by a server.
#[napi]
pub struct WebdavSession {
    inner: Arc<TransportWebdavSession>,
}

#[napi]
impl WebdavSession {
    /// Read-only N-API wrapper for the session-owned filesystem driver. The
    /// server retains the authoritative driver lifetime.
    #[napi(getter)]
    pub fn driver(&self) -> Filesystem {
        Filesystem::from_driver(Arc::clone(&self.inner.driver), None, None)
    }

    /// Handle one in-process, buffered WebDAV request without an HTTP socket.
    /// The response body is materialized for the N-API boundary.
    #[napi]
    pub async fn handle_request(
        &self,
        head: WebdavRequestHead,
        body: Option<Buffer>,
    ) -> napi::Result<WebdavResponse> {
        let response = self
            .inner
            .handle_request(
                transport_webdav_head(head),
                body.map(|value| value.to_vec()).unwrap_or_default(),
            )
            .await;
        webdav_response(response).await
    }

    /// Handle one in-process WebDAV request with a pull-based body stream.
    /// Request chunks are consumed by the transport as they arrive and file
    /// responses remain positional until the returned async iterator asks for
    /// each chunk.
    #[napi]
    pub fn handle_request_stream(
        &self,
        head: WebdavRequestHead,
        body: ReadableStream<'_, Buffer>,
    ) -> napi::Result<WebdavStreamRequest> {
        let reader = body
            .read()
            .map_err(|error| transport_error("WebDAV request body", error))?;
        Ok(WebdavStreamRequest {
            inner: tokio::sync::Mutex::new(Some(WebdavStreamRequestState {
                session: Arc::clone(&self.inner),
                head: transport_webdav_head(head),
                body: NapiWebdavRequestBody { reader },
            })),
        })
    }

    #[napi(getter)]
    pub fn stats(&self) -> WebdavSessionStats {
        let stats = self.inner.stats();
        WebdavSessionStats {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            methods: stats
                .methods
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
            assertions: stats.assertions as f64,
        }
    }

    #[napi(getter)]
    pub fn assertions(&self) -> Vec<String> {
        self.inner.assertions()
    }

    #[napi(getter)]
    pub fn options(&self) -> WebdavSessionOptionsView {
        let options = &self.inner.options;
        WebdavSessionOptionsView {
            credentials: options
                .credentials
                .as_ref()
                .map(|credentials| WebdavCredentials {
                    username: credentials.username.clone(),
                    password: credentials.password.clone(),
                }),
            realm: options.realm.clone(),
            read_chunk_bytes: options.read_chunk_bytes as f64,
            max_xml_bytes: options.max_xml_bytes as f64,
            max_body_bytes: options.max_body_bytes.map(|bytes| bytes as f64),
            locks: WebdavLockOptionsView {
                default_timeout_seconds: options.locks.default_timeout_seconds as f64,
                max_timeout_seconds: options.locks.max_timeout_seconds as f64,
                max_locks: options.locks.max_locks as f64,
            },
            debug: options.debug,
        }
    }

    #[napi(getter)]
    pub fn lock_count(&self) -> f64 {
        self.inner.lock_count() as f64
    }

    #[napi(getter)]
    pub fn locks(&self) -> Vec<WebdavLockView> {
        self.inner
            .lock_records()
            .into_iter()
            .map(|lock| WebdavLockView {
                token: lock.token,
                path: lock.path,
                collection: lock.collection,
                depth: lock.depth.to_string(),
                exclusive: lock.exclusive,
                owner: lock.owner.map(webdav_xml_node),
                timeout_seconds: lock.timeout_seconds as f64,
                expires_at: lock.expires_at as f64,
            })
            .collect()
    }
}

#[napi]
pub struct WebdavServer {
    inner: Arc<TransportWebdavServer>,
    host: String,
    closed: AtomicBool,
    lifecycle: tokio::sync::Mutex<()>,
    transport_error: Option<Arc<TransportErrorCallback>>,
    session_error: Option<Arc<WebdavErrorCallback>>,
}

#[napi]
impl WebdavServer {
    #[napi(getter)]
    pub fn session(&self) -> WebdavSession {
        WebdavSession {
            inner: Arc::clone(&self.inner.session),
        }
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        self.inner.port() as u32
    }

    #[napi(getter)]
    pub fn url(&self) -> String {
        format!("http://{}:{}", bracketed_host(&self.host), self.port())
    }

    #[napi(getter)]
    pub fn connections(&self) -> u32 {
        self.inner.connections() as u32
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("WebDAV listen", "server is closed"));
        }
        self.inner
            .listen()
            .await
            .map_err(|error| transport_error("WebDAV listen", error))
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.closed.store(true, Ordering::Release);
        if let Some(callback) = &self.transport_error {
            callback.release();
        }
        if let Some(callback) = &self.session_error {
            callback.release();
        }
        self.inner
            .close()
            .await
            .map_err(|error| transport_error("WebDAV close", error))
    }
}

#[napi]
pub fn create_webdav_server(
    driver: &Filesystem,
    options: Option<WebdavServerOptions>,
) -> napi::Result<WebdavServer> {
    let (host, _requested_port, options, on_transport_error, on_error) = webdav_options(options)?;
    let transport_error_callback = on_transport_error
        .map(TransportErrorCallback::new)
        .transpose()?;
    let session_error_callback = on_error.map(WebdavErrorCallback::new).transpose()?;
    let inner = mount_rs_webdav::create_webdav_server_with_session_hooks(
        Arc::clone(&driver.driver),
        options,
        webdav_hooks(transport_error_callback.as_ref()),
        webdav_session_hooks(session_error_callback.as_ref()),
    )
    .map_err(|error| transport_error("WebDAV create", error))?;
    Ok(WebdavServer {
        inner: Arc::new(inner),
        host,
        closed: AtomicBool::new(false),
        lifecycle: tokio::sync::Mutex::new(()),
        transport_error: transport_error_callback,
        session_error: session_error_callback,
    })
}
