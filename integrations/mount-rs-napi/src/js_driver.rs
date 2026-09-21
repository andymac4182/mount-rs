// Structural JavaScript `FsDriver` adapter.
//
// This module deliberately contains no filesystem policy.  JavaScript owns
// the driver and its handles; the adapter only marshals calls across N-API,
// copies byte values while they are on the JavaScript thread, and translates
// the result into the core `FsDriver` contract.
use std::fmt;
use std::future::Future;
use std::os::raw::c_void;
use std::pin::Pin;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle as CoreFileHandle, FileType, FsDriver, FsError,
    MkdirOptions, OpenFlags, Result as CoreResult, Stats, StatsFs,
};
use napi::bindgen_prelude::{
    BigInt, Either, FnArgs, FromNapiValue, Function, JsObjectValue, JsValue, JsValuesTupleIntoVec,
    Object, PromiseRaw, Uint8Array, Unknown,
};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, Error, Status, ValueType, sys};
use napi_derive::napi;

type JsReturn = Either<PromiseRaw<'static, Unknown<'static>>, Unknown<'static>>;
type JsCallback<T> = ThreadsafeFunction<T, JsReturn, T, Status, false, false>;

type PathArgs = FnArgs<(String,)>;
type ReaddirArgs = FnArgs<(String, super::JsReaddirOptions)>;
type OpenArgs = FnArgs<(String, Either<String, f64>, u32)>;
type MkdirArgs = FnArgs<(String, super::JsMkdirOptions)>;
type TwoPathArgs = FnArgs<(String, String)>;
type ChownArgs = FnArgs<(String, u32, u32)>;
type ChmodArgs = FnArgs<(String, u32)>;
type TruncateArgs = FnArgs<(String, f64)>;
type UtimeArgs = FnArgs<(String, f64, f64)>;
type UtimeNsArgs = FnArgs<(String, BigInt, BigInt, Option<JsUtimensOptions>)>;
type MknodArgs = FnArgs<(String, u32, f64)>;

type HandleReadArgs = FnArgs<(Uint8Array, f64, f64, Option<f64>)>;
type HandleWriteArgs = FnArgs<(Uint8Array, f64, f64, Option<f64>)>;
type HandleLengthArgs = FnArgs<(f64,)>;
type NoArgs = ();
type OpenFuture<'a> =
    Pin<Box<dyn Future<Output = CoreResult<Arc<dyn CoreFileHandle>>> + Send + 'a>>;

#[napi(object)]
pub struct JsUtimensOptions {
    pub follow_symlinks: Option<bool>,
}

#[derive(Debug, Clone)]
struct JsDriverError {
    operation: String,
    details: Box<JsDriverErrorDetails>,
}

#[derive(Debug, Clone, Default)]
struct JsDriverErrorDetails {
    code: Option<String>,
    errno: Option<i32>,
    syscall: Option<String>,
    path: Option<String>,
    dest: Option<String>,
    message: Option<String>,
}

impl JsDriverError {
    fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            details: Box::new(JsDriverErrorDetails::default()),
        }
    }

    fn native(operation: &str, error: impl fmt::Display) -> Self {
        let mut output = Self::new(operation);
        output.details.code = Some("EIO".to_owned());
        output.details.message = Some(error.to_string());
        output
    }

    fn closed() -> Self {
        let mut output = Self::new("driver");
        output.details.code = Some("EBADF".to_owned());
        output.details.message = Some("JavaScript driver callback adapter is closed".to_owned());
        output
    }

    fn with_code(mut self, code: impl Into<String>) -> Self {
        self.details.code = Some(code.into());
        self
    }

    fn into_fs_error(self, _operation: &str, path: Option<&str>, dest: Option<&str>) -> FsError {
        let code = self
            .details
            .code
            .as_deref()
            .and_then(error_code)
            .or_else(|| self.details.errno.and_then(error_code_from_errno))
            .unwrap_or(ErrorCode::Eio);
        let syscall = self.details.syscall.unwrap_or(self.operation);
        let mut output = FsError::new(code).with_syscall(syscall);
        if let Some(value) = self.details.path.or_else(|| path.map(str::to_owned)) {
            output = output.with_path(value);
        }
        if let Some(value) = self.details.dest.or_else(|| dest.map(str::to_owned)) {
            output = output.with_dest(value);
        }
        if let Some(message) = self.details.message {
            output = output.with_message(message);
        }
        output
    }
}

impl fmt::Display for JsDriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(message) = &self.details.message {
            return formatter.write_str(message);
        }
        write!(formatter, "{} callback failed", self.operation)
    }
}

impl std::error::Error for JsDriverError {}

fn error_code(value: &str) -> Option<ErrorCode> {
    Some(match value.to_ascii_uppercase().as_str() {
        "EPERM" => ErrorCode::Eperm,
        "ENOENT" => ErrorCode::Enoent,
        "EINTR" => ErrorCode::Eintr,
        "EIO" => ErrorCode::Eio,
        "ENXIO" => ErrorCode::Enxio,
        "EBADF" => ErrorCode::Ebadf,
        "EAGAIN" | "EWOULDBLOCK" => ErrorCode::Eagain,
        "ENOMEM" => ErrorCode::Enomem,
        "EACCES" => ErrorCode::Eacces,
        "EBUSY" => ErrorCode::Ebusy,
        "EEXIST" => ErrorCode::Eexist,
        "EXDEV" => ErrorCode::Exdev,
        "ENODEV" => ErrorCode::Enodev,
        "ENOTDIR" => ErrorCode::Enotdir,
        "EISDIR" => ErrorCode::Eisdir,
        "EINVAL" => ErrorCode::Einval,
        "ENFILE" => ErrorCode::Enfile,
        "EMFILE" => ErrorCode::Emfile,
        "EFBIG" => ErrorCode::Efbig,
        "ENOSPC" => ErrorCode::Enospc,
        "ESPIPE" => ErrorCode::Espipe,
        "EROFS" => ErrorCode::Erofs,
        "EMLINK" => ErrorCode::Emlink,
        "ERANGE" => ErrorCode::Erange,
        "ENAMETOOLONG" => ErrorCode::Enametoolong,
        "ENOSYS" => ErrorCode::Enosys,
        "ENOTEMPTY" => ErrorCode::Enotempty,
        "ELOOP" => ErrorCode::Eloop,
        "ENODATA" => ErrorCode::Enodata,
        "EPROTO" => ErrorCode::Eproto,
        "EOVERFLOW" => ErrorCode::Eoverflow,
        "ENOTSUP" | "EOPNOTSUPP" => ErrorCode::Enotsup,
        "ESTALE" => ErrorCode::Estale,
        "EDQUOT" => ErrorCode::Edquot,
        _ => return None,
    })
}

fn error_code_from_errno(value: i32) -> Option<ErrorCode> {
    let value = value.unsigned_abs();
    Some(match value {
        1 => ErrorCode::Eperm,
        2 => ErrorCode::Enoent,
        4 => ErrorCode::Eintr,
        5 => ErrorCode::Eio,
        6 => ErrorCode::Enxio,
        9 => ErrorCode::Ebadf,
        11 => ErrorCode::Eagain,
        12 => ErrorCode::Enomem,
        13 => ErrorCode::Eacces,
        16 => ErrorCode::Ebusy,
        17 => ErrorCode::Eexist,
        18 => ErrorCode::Exdev,
        19 => ErrorCode::Enodev,
        20 => ErrorCode::Enotdir,
        21 => ErrorCode::Eisdir,
        22 => ErrorCode::Einval,
        23 => ErrorCode::Enfile,
        24 => ErrorCode::Emfile,
        27 => ErrorCode::Efbig,
        28 => ErrorCode::Enospc,
        29 => ErrorCode::Espipe,
        30 => ErrorCode::Erofs,
        31 => ErrorCode::Emlink,
        34 => ErrorCode::Erange,
        36 => ErrorCode::Enametoolong,
        38 => ErrorCode::Enosys,
        39 => ErrorCode::Enotempty,
        40 => ErrorCode::Eloop,
        61 => ErrorCode::Enodata,
        71 => ErrorCode::Eproto,
        75 => ErrorCode::Eoverflow,
        95 => ErrorCode::Enotsup,
        116 => ErrorCode::Estale,
        122 => ErrorCode::Edquot,
        _ => return None,
    })
}

fn read_error_string(object: &Object<'_>, name: &str) -> Option<String> {
    let value: Unknown = object.get_named_property_unchecked(name).ok()?;
    if value.get_type().ok()? != ValueType::String {
        return None;
    }
    // SAFETY: the value type was checked above.
    unsafe { String::from_napi_value(value.value().env, value.raw()) }.ok()
}

fn read_error_errno(object: &Object<'_>) -> Option<i32> {
    let value: Unknown = object.get_named_property_unchecked("errno").ok()?;
    if value.get_type().ok()? != ValueType::Number {
        return None;
    }
    // SAFETY: the value type was checked above.
    let value = unsafe { f64::from_napi_value(value.value().env, value.raw()) }.ok()?;
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i32::MIN as f64
        || value > i32::MAX as f64
    {
        return None;
    }
    Some(value as i32)
}

fn capture_js_error(value: Unknown<'static>, operation: &str) -> JsDriverError {
    let mut output = JsDriverError::new(operation);
    if value.get_type().ok() == Some(ValueType::Object) {
        // SAFETY: the value type was checked above.
        if let Ok(object) = unsafe { Object::from_napi_value(value.value().env, value.raw()) } {
            output.details.code = read_error_string(&object, "code");
            output.details.errno = read_error_errno(&object);
            output.details.syscall = read_error_string(&object, "syscall");
            output.details.path = read_error_string(&object, "path");
            output.details.dest = read_error_string(&object, "dest");
            output.details.message = read_error_string(&object, "message");
        }
    }
    if output.details.message.is_none() {
        output.details.message = value
            .coerce_to_string()
            .ok()
            // SAFETY: `coerce_to_string` returns a JavaScript string.
            .and_then(|value| unsafe {
                String::from_napi_value(value.value().env, value.raw()).ok()
            });
    }
    output
}

fn capture_napi_error(error: Error, env: Env, operation: &str) -> JsDriverError {
    // `JsError::from(error)` preserves the original thrown value, unlike
    // formatting the N-API status.  This is where Node's code/errno metadata
    // must be captured, while the value is still on the JavaScript thread.
    let value = napi::JsError::from(error).into_unknown(env);
    capture_js_error(value, operation)
}

struct CompletionState<T> {
    result: Option<Result<T, JsDriverError>>,
    waker: Option<Waker>,
}

struct Completion<T> {
    state: Mutex<CompletionState<T>>,
}

impl<T> Completion<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new(CompletionState {
                result: None,
                waker: None,
            }),
        }
    }

    fn finish(&self, result: Result<T, JsDriverError>) {
        let waker = match self.state.lock() {
            Ok(mut state) => {
                if state.result.is_some() {
                    return;
                }
                state.result = Some(result);
                state.waker.take()
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                if state.result.is_some() {
                    return;
                }
                state.result = Some(result);
                state.waker.take()
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct CompletionFuture<T>(Arc<Completion<T>>);

impl<T> Future for CompletionFuture<T>
where
    T: Send + 'static,
{
    type Output = Result<T, JsDriverError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = match self.0.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(result) = state.result.take() {
            Poll::Ready(result)
        } else {
            state.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

trait CancelWaiter: Send + Sync {
    fn cancel(&self);
}

impl<T> CancelWaiter for Completion<T>
where
    T: Send + 'static,
{
    fn cancel(&self) {
        self.finish(Err(JsDriverError::closed()));
    }
}

struct WaiterRegistry {
    waiters: Mutex<Vec<Weak<dyn CancelWaiter>>>,
}

impl WaiterRegistry {
    fn new() -> Self {
        Self {
            waiters: Mutex::new(Vec::new()),
        }
    }

    fn add<T>(&self, waiter: &Arc<Completion<T>>)
    where
        T: Send + 'static,
    {
        let erased: Arc<dyn CancelWaiter> = waiter.clone();
        let weak = Arc::downgrade(&erased);
        let mut waiters = match self.waiters.lock() {
            Ok(waiters) => waiters,
            Err(poisoned) => poisoned.into_inner(),
        };
        waiters.push(weak);
    }

    fn cancel_all(&self) {
        let waiters = match self.waiters.lock() {
            Ok(mut waiters) => std::mem::take(&mut *waiters),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for waiter in waiters {
            if let Some(waiter) = waiter.upgrade() {
                waiter.cancel();
            }
        }
    }
}

struct Lifecycle {
    closed: AtomicBool,
    callbacks: Mutex<Vec<Weak<dyn CallbackControl>>>,
    waiters: WaiterRegistry,
}

impl Lifecycle {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            closed: AtomicBool::new(false),
            callbacks: Mutex::new(Vec::new()),
            waiters: WaiterRegistry::new(),
        })
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn register<T>(self: &Arc<Self>, callback: &Arc<CallbackSlot<T>>)
    where
        T: 'static + JsValuesTupleIntoVec,
    {
        let erased: Arc<dyn CallbackControl> = callback.clone();
        let weak = Arc::downgrade(&erased);
        let mut callbacks = match self.callbacks.lock() {
            Ok(callbacks) => callbacks,
            Err(poisoned) => poisoned.into_inner(),
        };
        callbacks.push(weak);
    }

    fn shutdown(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.waiters.cancel_all();
        let callbacks = match self.callbacks.lock() {
            Ok(mut callbacks) => std::mem::take(&mut *callbacks),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for callback in callbacks {
            if let Some(callback) = callback.upgrade() {
                callback.abort();
            }
        }
    }
}

trait CallbackControl: Send + Sync {
    fn abort(&self);
}

struct CallbackSlot<T: 'static + JsValuesTupleIntoVec> {
    callback: Arc<JsCallback<T>>,
}

impl<T: 'static + JsValuesTupleIntoVec> CallbackControl for CallbackSlot<T> {
    fn abort(&self) {
        self.callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the TSFN raw handle is owned by `callback.handle`;
                // the write guard serializes this one abort with its Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        self.callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

fn has_function(object: &Object<'_>, name: &str) -> napi::Result<bool> {
    if !object.has_named_property(name)? {
        return Ok(false);
    }
    let value: Unknown = object.get_named_property_unchecked(name)?;
    Ok(value.get_type()? == ValueType::Function)
}

fn build_callback<'env, T>(
    object: Object<'env>,
    name: &str,
    lifecycle: &Arc<Lifecycle>,
    required: bool,
) -> napi::Result<Option<Arc<CallbackSlot<T>>>>
where
    T: 'static + JsValuesTupleIntoVec,
{
    if !has_function(&object, name)? {
        if required {
            return Err(Error::new(
                Status::InvalidArg,
                format!("FsDriver method '{name}' must be a function"),
            ));
        }
        return Ok(None);
    }
    let function: Function<'static, Unknown<'static>, JsReturn> =
        object.get_named_property(name)?;
    let function = function.bind(object)?;
    let callback = function
        .build_threadsafe_function::<T>()
        .weak::<false>()
        .callee_handled::<false>()
        .build_callback(|context| Ok(context.value))?;
    let callback = Arc::new(CallbackSlot {
        callback: Arc::new(callback),
    });
    lifecycle.register(&callback);
    Ok(Some(callback))
}

async fn invoke<T, R>(
    callback: Arc<CallbackSlot<T>>,
    lifecycle: Arc<Lifecycle>,
    owner: &WaiterRegistry,
    value: T,
    operation: &'static str,
    parse: Arc<dyn Fn(Env, Unknown<'static>) -> Result<R, JsDriverError> + Send + Sync>,
) -> Result<R, JsDriverError>
where
    T: 'static + JsValuesTupleIntoVec,
    R: Send + 'static,
{
    if lifecycle.is_closed() {
        return Err(JsDriverError::closed());
    }
    let completion = Arc::new(Completion::new());
    owner.add(&completion);
    let completion_for_callback = Arc::clone(&completion);
    let parse_for_callback = Arc::clone(&parse);
    let status = callback.callback.call_with_return_value(
        value,
        ThreadsafeFunctionCallMode::NonBlocking,
        move |result, env| {
            match result {
                Ok(Either::B(value)) => {
                    completion_for_callback.finish(parse_for_callback(env, value));
                }
                Ok(Either::A(promise)) => {
                    let completion_for_then = Arc::clone(&completion_for_callback);
                    let parse_for_then = Arc::clone(&parse_for_callback);
                    let chain = promise.then(move |context| {
                        completion_for_then.finish(parse_for_then(context.env, context.value));
                        Ok(())
                    });
                    match chain {
                        Ok(chain) => {
                            let completion_for_catch = Arc::clone(&completion_for_callback);
                            let chain_result = chain.catch(move |context| {
                                completion_for_catch
                                    .finish(Err(capture_js_error(context.value, operation)));
                                Ok(())
                            });
                            if let Err(error) = chain_result {
                                completion_for_callback
                                    .finish(Err(JsDriverError::native(operation, error)));
                            }
                        }
                        Err(error) => {
                            completion_for_callback
                                .finish(Err(JsDriverError::native(operation, error)));
                        }
                    }
                }
                Err(error) => {
                    completion_for_callback.finish(Err(capture_napi_error(error, env, operation)));
                }
            }
            Ok(())
        },
    );
    if status != Status::Ok {
        completion.finish(Err(JsDriverError::new(operation).with_code("EBADF")));
    }
    CompletionFuture(completion).await
}

fn value_object<'env>(
    value: Unknown<'env>,
    operation: &str,
) -> Result<Object<'env>, JsDriverError> {
    if value
        .get_type()
        .map_err(|error| JsDriverError::native(operation, error))?
        != ValueType::Object
    {
        return Err(JsDriverError::native(
            operation,
            "callback returned a non-object",
        ));
    }
    // SAFETY: the value type was checked above.
    unsafe { Object::from_napi_value(value.value().env, value.raw()) }
        .map_err(|error| JsDriverError::native(operation, error))
}

fn property<'env>(
    object: &Object<'env>,
    name: &str,
    operation: &str,
) -> Result<Option<Unknown<'env>>, JsDriverError> {
    let value: Unknown = object
        .get_named_property_unchecked(name)
        .map_err(|error| JsDriverError::native(operation, error))?;
    match value
        .get_type()
        .map_err(|error| JsDriverError::native(operation, error))?
    {
        ValueType::Null | ValueType::Undefined => Ok(None),
        _ => Ok(Some(value)),
    }
}

fn required_property<'env>(
    object: &Object<'env>,
    name: &str,
    operation: &str,
) -> Result<Unknown<'env>, JsDriverError> {
    property(object, name, operation)?.ok_or_else(|| {
        JsDriverError::native(operation, format!("callback result is missing '{name}'"))
    })
}

fn number_property(object: &Object<'_>, name: &str, operation: &str) -> Result<f64, JsDriverError> {
    let value = required_property(object, name, operation)?;
    if value
        .get_type()
        .map_err(|error| JsDriverError::native(operation, error))?
        != ValueType::Number
    {
        return Err(JsDriverError::native(
            operation,
            format!("'{name}' must be a number"),
        ));
    }
    // SAFETY: the value type was checked above.
    unsafe { f64::from_napi_value(value.value().env, value.raw()) }
        .map_err(|error| JsDriverError::native(operation, error))
}

fn optional_number_property(
    object: &Object<'_>,
    name: &str,
    operation: &str,
) -> Result<Option<f64>, JsDriverError> {
    let Some(value) = property(object, name, operation)? else {
        return Ok(None);
    };
    if value
        .get_type()
        .map_err(|error| JsDriverError::native(operation, error))?
        != ValueType::Number
    {
        return Err(JsDriverError::native(
            operation,
            format!("'{name}' must be a number"),
        ));
    }
    // SAFETY: the value type was checked above.
    unsafe { f64::from_napi_value(value.value().env, value.raw()) }
        .map(Some)
        .map_err(|error| JsDriverError::native(operation, error))
}

fn finite_u64(value: f64, name: &str, operation: &str) -> Result<u64, JsDriverError> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value >= u64::MAX as f64 {
        return Err(JsDriverError::native(
            operation,
            format!("'{name}' must be a non-negative integer"),
        ));
    }
    Ok(value as u64)
}

fn finite_u32(value: f64, name: &str, operation: &str) -> Result<u32, JsDriverError> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > u32::MAX as f64 {
        return Err(JsDriverError::native(
            operation,
            format!("'{name}' must be a u32"),
        ));
    }
    Ok(value as u32)
}

fn finite_i64(value: f64, name: &str, operation: &str) -> Result<i64, JsDriverError> {
    if !value.is_finite() || value < i64::MIN as f64 || value >= i64::MAX as f64 {
        return Err(JsDriverError::native(
            operation,
            format!("'{name}' is outside i64 range"),
        ));
    }
    Ok(value as i64)
}

fn check_napi_status(status: sys::napi_status, operation: &str) -> Result<(), JsDriverError> {
    if status == sys::Status::napi_ok {
        Ok(())
    } else {
        Err(JsDriverError::native(operation, Status::from(status)))
    }
}

fn copy_bytes(value: Unknown<'_>, operation: &str) -> Result<Vec<u8>, JsDriverError> {
    let env = value.value().env;
    let raw = value.raw();
    let mut is_buffer = false;
    // SAFETY: `env` and `raw` are the callback's live N-API value.
    check_napi_status(
        unsafe { sys::napi_is_buffer(env, raw, &mut is_buffer) },
        operation,
    )?;
    if is_buffer {
        let mut data = ptr::null_mut();
        let mut length = 0;
        // SAFETY: N-API fills the buffer pointer/length for this value.
        check_napi_status(
            unsafe { sys::napi_get_buffer_info(env, raw, &mut data, &mut length) },
            operation,
        )?;
        return copy_raw(data, length, operation);
    }

    let mut is_typed_array = false;
    // SAFETY: `env` and `raw` are the callback's live N-API value.
    check_napi_status(
        unsafe { sys::napi_is_typedarray(env, raw, &mut is_typed_array) },
        operation,
    )?;
    if is_typed_array {
        let mut array_type = 0;
        let mut length = 0;
        let mut data = ptr::null_mut();
        let mut _array_buffer = ptr::null_mut();
        let mut _byte_offset = 0;
        // SAFETY: N-API fills the typed-array descriptor for this value.
        check_napi_status(
            unsafe {
                sys::napi_get_typedarray_info(
                    env,
                    raw,
                    &mut array_type,
                    &mut length,
                    &mut data,
                    &mut _array_buffer,
                    &mut _byte_offset,
                )
            },
            operation,
        )?;
        let bytes_per_element = match napi::bindgen_prelude::TypedArrayType::from(array_type) {
            napi::bindgen_prelude::TypedArrayType::Int8
            | napi::bindgen_prelude::TypedArrayType::Uint8
            | napi::bindgen_prelude::TypedArrayType::Uint8Clamped => 1,
            napi::bindgen_prelude::TypedArrayType::Int16
            | napi::bindgen_prelude::TypedArrayType::Uint16 => 2,
            napi::bindgen_prelude::TypedArrayType::Int32
            | napi::bindgen_prelude::TypedArrayType::Uint32
            | napi::bindgen_prelude::TypedArrayType::Float32 => 4,
            napi::bindgen_prelude::TypedArrayType::Float64 => 8,
            _ => {
                return Err(JsDriverError::native(
                    operation,
                    "unsupported JavaScript typed-array kind",
                ));
            }
        };
        let byte_length = length
            .checked_mul(bytes_per_element)
            .ok_or_else(|| JsDriverError::native(operation, "typed-array byte length overflow"))?;
        return copy_raw(data, byte_length, operation);
    }

    let mut is_data_view = false;
    // SAFETY: `env` and `raw` are the callback's live N-API value.
    check_napi_status(
        unsafe { sys::napi_is_dataview(env, raw, &mut is_data_view) },
        operation,
    )?;
    if is_data_view {
        let mut byte_length = 0;
        let mut data = ptr::null_mut();
        let mut _array_buffer = ptr::null_mut();
        let mut _byte_offset = 0;
        // SAFETY: N-API fills the DataView descriptor for this value.
        check_napi_status(
            unsafe {
                sys::napi_get_dataview_info(
                    env,
                    raw,
                    &mut byte_length,
                    &mut data,
                    &mut _array_buffer,
                    &mut _byte_offset,
                )
            },
            operation,
        )?;
        return copy_raw(data, byte_length, operation);
    }

    let mut is_array_buffer = false;
    // SAFETY: `env` and `raw` are the callback's live N-API value.
    check_napi_status(
        unsafe { sys::napi_is_arraybuffer(env, raw, &mut is_array_buffer) },
        operation,
    )?;
    if is_array_buffer {
        let mut data = ptr::null_mut();
        let mut byte_length = 0;
        // SAFETY: N-API fills the ArrayBuffer descriptor for this value.
        check_napi_status(
            unsafe { sys::napi_get_arraybuffer_info(env, raw, &mut data, &mut byte_length) },
            operation,
        )?;
        return copy_raw(data, byte_length, operation);
    }
    Err(JsDriverError::native(
        operation,
        "expected a Buffer, typed array, DataView, or ArrayBuffer",
    ))
}

fn copy_raw(data: *mut c_void, length: usize, operation: &str) -> Result<Vec<u8>, JsDriverError> {
    if length == 0 {
        return Ok(Vec::new());
    }
    if data.is_null() {
        return Err(JsDriverError::native(
            operation,
            "JavaScript byte value has a null data pointer",
        ));
    }
    // SAFETY: N-API supplied this pointer and byte length for a live value;
    // the copy completes before the JS callback returns.
    Ok(unsafe { slice::from_raw_parts(data.cast::<u8>(), length) }.to_vec())
}

fn parse_string(value: Unknown<'_>, name: &str, operation: &str) -> Result<String, JsDriverError> {
    if value
        .get_type()
        .map_err(|error| JsDriverError::native(operation, error))?
        != ValueType::String
    {
        return Err(JsDriverError::native(
            operation,
            format!("'{name}' must be a string"),
        ));
    }
    // SAFETY: the value type was checked above.
    unsafe { String::from_napi_value(value.value().env, value.raw()) }
        .map_err(|error| JsDriverError::native(operation, error))
}

fn parse_optional_string(
    _env: Env,
    value: Unknown<'static>,
) -> Result<Option<String>, JsDriverError> {
    match value
        .get_type()
        .map_err(|error| JsDriverError::native("mkdir", error))?
    {
        ValueType::Undefined | ValueType::Null => Ok(None),
        _ => parse_string(value, "mkdir result", "mkdir").map(Some),
    }
}

fn parse_stats(_env: Env, value: Unknown<'static>) -> Result<Stats, JsDriverError> {
    let operation = "stat";
    let object = value_object(value, operation)?;
    Ok(Stats {
        dev: finite_u64(
            number_property(&object, "dev", operation)?,
            "dev",
            operation,
        )?,
        ino: finite_u64(
            number_property(&object, "ino", operation)?,
            "ino",
            operation,
        )?,
        mode: finite_u32(
            number_property(&object, "mode", operation)?,
            "mode",
            operation,
        )?,
        nlink: finite_u64(
            number_property(&object, "nlink", operation)?,
            "nlink",
            operation,
        )?,
        uid: finite_u32(
            number_property(&object, "uid", operation)?,
            "uid",
            operation,
        )?,
        gid: finite_u32(
            number_property(&object, "gid", operation)?,
            "gid",
            operation,
        )?,
        rdev: finite_u64(
            number_property(&object, "rdev", operation)?,
            "rdev",
            operation,
        )?,
        size: finite_u64(
            number_property(&object, "size", operation)?,
            "size",
            operation,
        )?,
        blksize: finite_u64(
            number_property(&object, "blksize", operation)?,
            "blksize",
            operation,
        )?,
        blocks: finite_u64(
            number_property(&object, "blocks", operation)?,
            "blocks",
            operation,
        )?,
        atime_ms: finite_i64(
            number_property(&object, "atimeMs", operation)?,
            "atimeMs",
            operation,
        )?,
        mtime_ms: finite_i64(
            number_property(&object, "mtimeMs", operation)?,
            "mtimeMs",
            operation,
        )?,
        ctime_ms: finite_i64(
            number_property(&object, "ctimeMs", operation)?,
            "ctimeMs",
            operation,
        )?,
        birthtime_ms: finite_i64(
            number_property(&object, "birthtimeMs", operation)?,
            "birthtimeMs",
            operation,
        )?,
    })
}

fn parse_statsfs(_env: Env, value: Unknown<'static>) -> Result<StatsFs, JsDriverError> {
    let operation = "statfs";
    let object = value_object(value, operation)?;
    Ok(StatsFs {
        filesystem_type: finite_u64(
            number_property(&object, "type", operation)?,
            "type",
            operation,
        )?,
        block_size: finite_u64(
            number_property(&object, "bsize", operation)?,
            "bsize",
            operation,
        )?,
        blocks: finite_u64(
            number_property(&object, "blocks", operation)?,
            "blocks",
            operation,
        )?,
        blocks_free: finite_u64(
            number_property(&object, "bfree", operation)?,
            "bfree",
            operation,
        )?,
        blocks_available: finite_u64(
            number_property(&object, "bavail", operation)?,
            "bavail",
            operation,
        )?,
        files: finite_u64(
            number_property(&object, "files", operation)?,
            "files",
            operation,
        )?,
        files_free: finite_u64(
            number_property(&object, "ffree", operation)?,
            "ffree",
            operation,
        )?,
    })
}

fn call_dirent_flag(object: &Object<'_>, name: &str) -> Result<bool, JsDriverError> {
    let function: Function<'static, (), bool> = object
        .get_named_property(name)
        .map_err(|error| JsDriverError::native("readdir", error))?;
    let function = function
        .bind(*object)
        .map_err(|error| JsDriverError::native("readdir", error))?;
    function
        .call(())
        .map_err(|error| JsDriverError::native("readdir", error))
}

fn parse_dirents(
    _env: Env,
    value: Unknown<'static>,
    parent_path: &str,
) -> Result<Vec<DirEntry>, JsDriverError> {
    let operation = "readdir";
    let array: Vec<Unknown<'static>> =
        unsafe { Vec::from_napi_value(value.value().env, value.raw()) }
            .map_err(|error| JsDriverError::native(operation, error))?;
    let mut output = Vec::with_capacity(array.len());
    for value in array {
        let object = value_object(value, operation)?;
        let name = parse_string(
            required_property(&object, "name", operation)?,
            "name",
            operation,
        )?;
        let file_type = if call_dirent_flag(&object, "isFile")? {
            FileType::File
        } else if call_dirent_flag(&object, "isDirectory")? {
            FileType::Directory
        } else if call_dirent_flag(&object, "isSymbolicLink")? {
            FileType::Symlink
        } else if call_dirent_flag(&object, "isBlockDevice")? {
            FileType::BlockDevice
        } else if call_dirent_flag(&object, "isCharacterDevice")? {
            FileType::CharacterDevice
        } else if call_dirent_flag(&object, "isFIFO")? {
            FileType::Fifo
        } else if call_dirent_flag(&object, "isSocket")? {
            FileType::Socket
        } else {
            return Err(JsDriverError::native(
                operation,
                "DirentLike did not identify a file type",
            ));
        };
        output.push(DirEntry {
            name,
            parent_path: parent_path.to_owned(),
            file_type,
        });
    }
    Ok(output)
}

fn parse_read_result(
    _env: Env,
    value: Unknown<'static>,
) -> Result<(usize, Vec<u8>), JsDriverError> {
    let operation = "read";
    let object = value_object(value, operation)?;
    let bytes_read = finite_u64(
        number_property(&object, "bytesRead", operation)?,
        "bytesRead",
        operation,
    )?;
    let buffer = copy_bytes(required_property(&object, "buffer", operation)?, operation)?;
    let bytes_read =
        usize::try_from(bytes_read).map_err(|error| JsDriverError::native(operation, error))?;
    if bytes_read > buffer.len() {
        return Err(JsDriverError::native(
            operation,
            "bytesRead exceeds the returned buffer length",
        ));
    }
    Ok((bytes_read, buffer))
}

fn parse_write_result(_env: Env, value: Unknown<'static>) -> Result<usize, JsDriverError> {
    let operation = "write";
    let object = value_object(value, operation)?;
    let bytes_written = finite_u64(
        number_property(&object, "bytesWritten", operation)?,
        "bytesWritten",
        operation,
    )?;
    usize::try_from(bytes_written).map_err(|error| JsDriverError::native(operation, error))
}

fn parse_unit(_env: Env, _value: Unknown<'static>) -> Result<(), JsDriverError> {
    Ok(())
}

fn parse_handle(
    _env: Env,
    value: Unknown<'static>,
    lifecycle: Arc<Lifecycle>,
) -> Result<Arc<JsFileHandle>, JsDriverError> {
    let object = value_object(value, "open")?;
    let fd = optional_number_property(&object, "fd", "open")?
        .map(|value| finite_u64(value, "fd", "open"))
        .transpose()?;
    let state = Arc::new(HandleState {
        closed: AtomicBool::new(false),
        waiters: WaiterRegistry::new(),
    });
    let read = build_callback(object, "read", &lifecycle, true)
        .map_err(|error| JsDriverError::native("open", error))?
        .expect("required callback checked by build_callback");
    let write = build_callback(object, "write", &lifecycle, true)
        .map_err(|error| JsDriverError::native("open", error))?
        .expect("required callback checked by build_callback");
    let stat = build_callback(object, "stat", &lifecycle, true)
        .map_err(|error| JsDriverError::native("open", error))?
        .expect("required callback checked by build_callback");
    let truncate = build_callback(object, "truncate", &lifecycle, true)
        .map_err(|error| JsDriverError::native("open", error))?
        .expect("required callback checked by build_callback");
    let close = build_callback(object, "close", &lifecycle, true)
        .map_err(|error| JsDriverError::native("open", error))?
        .expect("required callback checked by build_callback");
    let sync = build_callback(object, "sync", &lifecycle, false)
        .map_err(|error| JsDriverError::native("open", error))?;
    let datasync = build_callback(object, "datasync", &lifecycle, false)
        .map_err(|error| JsDriverError::native("open", error))?;
    Ok(Arc::new(JsFileHandle {
        fd,
        lifecycle,
        state,
        callbacks: HandleCallbacks {
            read,
            write,
            stat,
            truncate,
            sync,
            datasync,
            close,
        },
    }))
}

struct HandleCallbacks {
    read: Arc<CallbackSlot<HandleReadArgs>>,
    write: Arc<CallbackSlot<HandleWriteArgs>>,
    stat: Arc<CallbackSlot<()>>,
    truncate: Arc<CallbackSlot<HandleLengthArgs>>,
    sync: Option<Arc<CallbackSlot<NoArgs>>>,
    datasync: Option<Arc<CallbackSlot<NoArgs>>>,
    close: Arc<CallbackSlot<()>>,
}

impl HandleCallbacks {
    fn abort(&self) {
        self.read.abort();
        self.write.abort();
        self.stat.abort();
        self.truncate.abort();
        if let Some(callback) = &self.sync {
            callback.abort();
        }
        if let Some(callback) = &self.datasync {
            callback.abort();
        }
        self.close.abort();
    }
}

struct HandleState {
    closed: AtomicBool,
    waiters: WaiterRegistry,
}

struct JsFileHandle {
    fd: Option<u64>,
    lifecycle: Arc<Lifecycle>,
    state: Arc<HandleState>,
    callbacks: HandleCallbacks,
}

impl JsFileHandle {
    fn ensure_open(&self) -> CoreResult<()> {
        if self.lifecycle.is_closed() || self.state.closed.load(Ordering::Acquire) {
            Err(FsError::new(ErrorCode::Ebadf).with_syscall("file handle"))
        } else {
            Ok(())
        }
    }

    fn closed_error(operation: &str) -> FsError {
        FsError::new(ErrorCode::Ebadf).with_syscall(operation)
    }
}

impl CoreFileHandle for JsFileHandle {
    fn fd(&self) -> Option<u64> {
        self.fd
    }

    fn read<'a, 'b, 'async_trait>(
        &'a self,
        buffer: &'b mut [u8],
        position: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = CoreResult<usize>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        if let Err(error) = self.ensure_open() {
            return Box::pin(async move { Err(error) });
        }
        let callback = self.callbacks.read.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let owner = &self.state.waiters;
        let target = buffer.len();
        let parse = Arc::new(parse_read_result);
        let value = FnArgs::from((
            Uint8Array::from(vec![0_u8; target]),
            0.0,
            target as f64,
            position.map(|value| value as f64),
        ));
        Box::pin(async move {
            let (count, bytes) = invoke(callback, lifecycle, owner, value, "read", parse)
                .await
                .map_err(|error| error.into_fs_error("read", None, None))?;
            if count > buffer.len() {
                return Err(FsError::new(ErrorCode::Eio).with_syscall("read"));
            }
            buffer[..count].copy_from_slice(&bytes[..count]);
            Ok(count)
        })
    }

    fn write<'a, 'b, 'async_trait>(
        &'a self,
        buffer: &'b [u8],
        position: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = CoreResult<usize>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        if let Err(error) = self.ensure_open() {
            return Box::pin(async move { Err(error) });
        }
        let callback = self.callbacks.write.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let owner = &self.state.waiters;
        let length = buffer.len();
        let input = buffer.to_vec();
        let parse = Arc::new(parse_write_result);
        let value = FnArgs::from((
            Uint8Array::from(input),
            0.0,
            length as f64,
            position.map(|value| value as f64),
        ));
        Box::pin(async move {
            let count = invoke(callback, lifecycle, owner, value, "write", parse)
                .await
                .map_err(|error| error.into_fs_error("write", None, None))?;
            if count > length {
                return Err(FsError::new(ErrorCode::Eio).with_syscall("write"));
            }
            Ok(count)
        })
    }

    fn stat<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Stats>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        if let Err(error) = self.ensure_open() {
            return Box::pin(async move { Err(error) });
        }
        let callback = self.callbacks.stat.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let owner = &self.state.waiters;
        let parse = Arc::new(parse_stats);
        Box::pin(async move {
            invoke(callback, lifecycle, owner, (), "stat", parse)
                .await
                .map_err(|error| error.into_fs_error("stat", None, None))
        })
    }

    fn truncate<'a, 'async_trait>(
        &'a self,
        length: u64,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        if let Err(error) = self.ensure_open() {
            return Box::pin(async move { Err(error) });
        }
        if length > 9_007_199_254_740_991 {
            return Box::pin(async {
                Err(FsError::new(ErrorCode::Eoverflow).with_syscall("truncate"))
            });
        }
        let callback = self.callbacks.truncate.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let owner = &self.state.waiters;
        let parse = Arc::new(parse_unit);
        let value = FnArgs::from((length as f64,));
        Box::pin(async move {
            invoke(callback, lifecycle, owner, value, "truncate", parse)
                .await
                .map_err(|error| error.into_fs_error("truncate", None, None))
        })
    }

    fn sync<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        self.simple_optional("sync", self.callbacks.sync.clone())
    }

    fn datasync<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        self.simple_optional("datasync", self.callbacks.datasync.clone())
    }

    fn close<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        if self.lifecycle.is_closed() || self.state.closed.swap(true, Ordering::AcqRel) {
            return Box::pin(async { Err(Self::closed_error("close")) });
        }
        self.state.waiters.cancel_all();
        let callback = self.callbacks.close.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let owner = &self.lifecycle.waiters;
        let callbacks = &self.callbacks;
        let parse = Arc::new(parse_unit);
        Box::pin(async move {
            let result = invoke(callback, lifecycle, owner, (), "close", parse)
                .await
                .map_err(|error| error.into_fs_error("close", None, None));
            callbacks.abort();
            result
        })
    }
}

impl JsFileHandle {
    fn simple_optional(
        &self,
        operation: &'static str,
        callback: Option<Arc<CallbackSlot<NoArgs>>>,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + '_>> {
        let Some(callback) = callback else {
            return Box::pin(async { Ok(()) });
        };
        if let Err(error) = self.ensure_open() {
            return Box::pin(async move { Err(error) });
        }
        let lifecycle = Arc::clone(&self.lifecycle);
        let owner = &self.state.waiters;
        let parse = Arc::new(parse_unit);
        Box::pin(async move {
            invoke(callback, lifecycle, owner, (), operation, parse)
                .await
                .map_err(|error| error.into_fs_error(operation, None, None))
        })
    }
}

struct DriverCallbacks {
    stat: Arc<CallbackSlot<PathArgs>>,
    readdir: Arc<CallbackSlot<ReaddirArgs>>,
    open: Arc<CallbackSlot<OpenArgs>>,
    lstat: Option<Arc<CallbackSlot<PathArgs>>>,
    statfs: Option<Arc<CallbackSlot<PathArgs>>>,
    mkdir: Option<Arc<CallbackSlot<MkdirArgs>>>,
    rmdir: Option<Arc<CallbackSlot<PathArgs>>>,
    unlink: Option<Arc<CallbackSlot<PathArgs>>>,
    rename: Option<Arc<CallbackSlot<TwoPathArgs>>>,
    link: Option<Arc<CallbackSlot<TwoPathArgs>>>,
    symlink: Option<Arc<CallbackSlot<TwoPathArgs>>>,
    readlink: Option<Arc<CallbackSlot<PathArgs>>>,
    chmod: Option<Arc<CallbackSlot<ChmodArgs>>>,
    chown: Option<Arc<CallbackSlot<ChownArgs>>>,
    lchown: Option<Arc<CallbackSlot<ChownArgs>>>,
    truncate: Option<Arc<CallbackSlot<TruncateArgs>>>,
    utimes: Option<Arc<CallbackSlot<UtimeArgs>>>,
    lutimes: Option<Arc<CallbackSlot<UtimeArgs>>>,
    utimens: Option<Arc<CallbackSlot<UtimeNsArgs>>>,
    mknod: Option<Arc<CallbackSlot<MknodArgs>>>,
}

struct JsDriver {
    lifecycle: Arc<Lifecycle>,
    callbacks: DriverCallbacks,
    capabilities: Capabilities,
}

fn optional_bool(object: &Object<'_>, name: &str) -> napi::Result<Option<bool>> {
    if !object.has_named_property(name)? {
        return Ok(None);
    }
    let value: Unknown = object.get_named_property_unchecked(name)?;
    match value.get_type()? {
        ValueType::Null | ValueType::Undefined => Ok(None),
        ValueType::Boolean => {
            // SAFETY: the value type was checked above.
            unsafe { bool::from_napi_value(value.value().env, value.raw()) }.map(Some)
        }
        _ => Err(Error::new(
            Status::InvalidArg,
            format!("driver capability '{name}' must be a boolean"),
        )),
    }
}

fn optional_extensions(object: &Object<'_>) -> napi::Result<Option<Vec<String>>> {
    if !object.has_named_property("extensions")? {
        return Ok(None);
    }
    let value: Unknown = object.get_named_property_unchecked("extensions")?;
    match value.get_type()? {
        ValueType::Null | ValueType::Undefined => Ok(None),
        ValueType::Object => {
            // SAFETY: `Vec<String>` validates and copies the JavaScript array.
            unsafe { Vec::<String>::from_napi_value(value.value().env, value.raw()) }.map(Some)
        }
        _ => Err(Error::new(
            Status::InvalidArg,
            "driver capability 'extensions' must be an array",
        )),
    }
}

fn read_capabilities(
    driver: &Object<'_>,
    method: impl Fn(&str) -> bool,
    has_mknod: bool,
) -> napi::Result<Capabilities> {
    let declared = if !driver.has_named_property("capabilities")? {
        None
    } else {
        let value: Unknown = driver.get_named_property_unchecked("capabilities")?;
        match value.get_type()? {
            ValueType::Null | ValueType::Undefined => None,
            ValueType::Object => {
                // SAFETY: the value type was checked above.
                Some(unsafe { Object::from_napi_value(value.value().env, value.raw()) }?)
            }
            _ => {
                return Err(Error::new(
                    Status::InvalidArg,
                    "driver capabilities must be an object",
                ));
            }
        }
    };
    let get = |name: &str, inferred: bool| -> napi::Result<bool> {
        Ok(declared
            .as_ref()
            .map(|value| optional_bool(value, name))
            .transpose()?
            .flatten()
            .unwrap_or(inferred))
    };
    let extensions = declared
        .as_ref()
        .map(optional_extensions)
        .transpose()?
        .flatten();
    Ok(Capabilities {
        handles: get("handles", false)?,
        hardlinks: get("hardlinks", method("link"))?,
        symlinks: get(
            "symlinks",
            method("symlink") && method("readlink") && method("lstat"),
        )?,
        permissions: get("permissions", method("chmod"))?,
        times: get("times", method("utimes"))?,
        truncate: get("truncate", method("truncate"))?,
        atomic_rename: get("atomicRename", false)?,
        case_sensitive: get("caseSensitive", true)?,
        statfs: get("statfs", method("statfs"))?,
        read_only: get("readOnly", false)?,
        durable_writes: get("durableWrites", false)?,
        mknod: extensions
            .as_ref()
            .map(|extensions| extensions.iter().any(|value| value == "mknod"))
            .unwrap_or(has_mknod),
    })
}

fn method_callback<T>(
    driver: Object<'_>,
    lifecycle: &Arc<Lifecycle>,
    name: &str,
) -> napi::Result<Option<Arc<CallbackSlot<T>>>>
where
    T: 'static + JsValuesTupleIntoVec,
{
    build_callback(driver, name, lifecycle, false)
}

/// Encode the normalized flags used by the Rust driver contract into the
/// numeric namespace accepted by a structural Node FsDriver. Native
/// transports frequently need combinations which have no Node string alias,
/// such as write-only without create or truncate; passing the numeric form
/// preserves that decoded intent across the N-API boundary.
fn encode_open_flags(flags: OpenFlags, path: &str) -> CoreResult<f64> {
    let access = match (flags.read, flags.write) {
        (true, false) => 0_u64,
        (false, true) => 1_u64,
        (true, true) => 2_u64,
        (false, false) => {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("open")
                .with_path(path)
                .with_message("decoded open flags do not select read or write access"));
        }
    };

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (access, flags);
        return Err(FsError::enotsup("open").with_path(path));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let mut bits = access;

        #[cfg(target_os = "linux")]
        {
            const O_CREAT: u64 = 0o100;
            const O_EXCL: u64 = 0o200;
            const O_TRUNC: u64 = 0o1000;
            const O_APPEND: u64 = 0o2000;
            if flags.create {
                bits |= O_CREAT;
            }
            if flags.exclusive {
                bits |= O_EXCL;
            }
            if flags.truncate {
                bits |= O_TRUNC;
            }
            if flags.append {
                bits |= O_APPEND;
            }
        }

        #[cfg(target_os = "macos")]
        {
            const O_CREAT: u64 = 0x0200;
            const O_EXCL: u64 = 0x0800;
            const O_TRUNC: u64 = 0x0400;
            const O_APPEND: u64 = 0x0008;
            if flags.create {
                bits |= O_CREAT;
            }
            if flags.exclusive {
                bits |= O_EXCL;
            }
            if flags.truncate {
                bits |= O_TRUNC;
            }
            if flags.append {
                bits |= O_APPEND;
            }
        }

        // Node exposes the Microsoft CRT open flags on Windows. These are
        // distinct from Linux values for create, exclusive, and truncate.
        #[cfg(target_os = "windows")]
        {
            const O_CREAT: u64 = 0x0100;
            const O_EXCL: u64 = 0x0400;
            const O_TRUNC: u64 = 0x0200;
            const O_APPEND: u64 = 0x0008;
            if flags.create {
                bits |= O_CREAT;
            }
            if flags.exclusive {
                bits |= O_EXCL;
            }
            if flags.truncate {
                bits |= O_TRUNC;
            }
            if flags.append {
                bits |= O_APPEND;
            }
        }

        Ok(bits as f64)
    }
}

impl JsDriver {
    fn open_with_flags(&self, path: &str, flags: Either<String, f64>, mode: u32) -> OpenFuture<'_> {
        let lifecycle = Arc::clone(&self.lifecycle);
        let callback = self.callbacks.open.clone();
        let parse_lifecycle = Arc::clone(&self.lifecycle);
        let parse =
            Arc::new(move |env, value| parse_handle(env, value, Arc::clone(&parse_lifecycle)));
        let value = FnArgs::from((path.to_owned(), flags, mode));
        Box::pin(async move {
            let handle = invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                value,
                "open",
                parse,
            )
            .await
            .map_err(|error| error.into_fs_error("open", None, None))?;
            Ok(handle as Arc<dyn CoreFileHandle>)
        })
    }

    fn call_path<'a, R>(
        &'a self,
        callback: Arc<CallbackSlot<PathArgs>>,
        path: String,
        operation: &'static str,
        parse: Arc<dyn Fn(Env, Unknown<'static>) -> Result<R, JsDriverError> + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = CoreResult<R>> + Send + 'a>>
    where
        R: Send + 'static,
    {
        let lifecycle = Arc::clone(&self.lifecycle);
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                FnArgs::from((path,)),
                operation,
                parse,
            )
            .await
            .map_err(|error| error.into_fs_error(operation, None, None))
        })
    }

    fn optional_path<R>(
        &self,
        callback: &Option<Arc<CallbackSlot<PathArgs>>>,
        path: String,
        operation: &'static str,
        parse: Arc<dyn Fn(Env, Unknown<'static>) -> Result<R, JsDriverError> + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = CoreResult<R>> + Send + '_>>
    where
        R: Send + 'static,
    {
        match callback {
            Some(callback) => self.call_path(callback.clone(), path, operation, parse),
            None => Box::pin(async move { Err(FsError::enosys(operation)) }),
        }
    }
}

impl FsDriver for JsDriver {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn stat<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Stats>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.call_path(
            self.callbacks.stat.clone(),
            path.to_owned(),
            "stat",
            Arc::new(parse_stats),
        )
    }

    fn lstat<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Stats>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.optional_path(
            &self.callbacks.lstat,
            path.to_owned(),
            "lstat",
            Arc::new(parse_stats),
        )
    }

    fn statfs<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<StatsFs>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.optional_path(
            &self.callbacks.statfs,
            path.to_owned(),
            "statfs",
            Arc::new(parse_statsfs),
        )
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Vec<DirEntry>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let path = path.to_owned();
        let parent_path = path.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let callback = self.callbacks.readdir.clone();
        let parse = Arc::new(move |env, value| parse_dirents(env, value, &parent_path));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                FnArgs::from((
                    path,
                    super::JsReaddirOptions {
                        with_file_types: Some(true),
                    },
                )),
                "readdir",
                parse,
            )
            .await
            .map_err(|error| error.into_fs_error("readdir", None, None))
        })
    }

    fn open<'a, 'b, 'c, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: &'c str,
        mode: u32,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Arc<dyn CoreFileHandle>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        self.open_with_flags(path, Either::A(flags.to_owned()), mode)
    }

    fn open_flags<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: OpenFlags,
        mode: u32,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Arc<dyn CoreFileHandle>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let numeric = match encode_open_flags(flags, path) {
            Ok(value) => value,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.open_with_flags(path, Either::B(numeric), mode)
    }

    fn mkdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        options: MkdirOptions,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Option<String>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let Some(callback) = self.callbacks.mkdir.clone() else {
            return Box::pin(async { Err(FsError::enosys("mkdir")) });
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let path = path.to_owned();
        let parse = Arc::new(parse_optional_string);
        let args = FnArgs::from((
            path,
            super::JsMkdirOptions {
                recursive: Some(options.recursive),
                mode: options.mode.map(f64::from),
            },
        ));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                "mkdir",
                parse,
            )
            .await
            .map_err(|error| error.into_fs_error("mkdir", None, None))
        })
    }

    fn rmdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.optional_path(
            &self.callbacks.rmdir,
            path.to_owned(),
            "rmdir",
            Arc::new(parse_unit),
        )
    }

    fn unlink<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.optional_path(
            &self.callbacks.unlink,
            path.to_owned(),
            "unlink",
            Arc::new(parse_unit),
        )
    }

    fn rename<'a, 'b, 'c, 'async_trait>(
        &'a self,
        old_path: &'b str,
        new_path: &'c str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        self.two_path(&self.callbacks.rename, old_path, new_path, "rename")
    }

    fn link<'a, 'b, 'c, 'async_trait>(
        &'a self,
        existing_path: &'b str,
        new_path: &'c str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        self.two_path(&self.callbacks.link, existing_path, new_path, "link")
    }

    fn symlink<'a, 'b, 'c, 'async_trait>(
        &'a self,
        target: &'b str,
        path: &'c str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        self.two_path(&self.callbacks.symlink, target, path, "symlink")
    }

    fn readlink<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<String>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.optional_path(
            &self.callbacks.readlink,
            path.to_owned(),
            "readlink",
            Arc::new(|_env, value| parse_string(value, "readlink", "readlink")),
        )
    }

    fn chmod<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        mode: u32,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.numeric_path(&self.callbacks.chmod, path, mode, "chmod")
    }

    fn chown<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        uid: u32,
        gid: u32,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let Some(callback) = self.callbacks.chown.clone() else {
            return Box::pin(async { Err(FsError::enosys("chown")) });
        };
        self.chown_like(callback, path, uid, gid, "chown")
    }

    fn lchown<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        uid: u32,
        gid: u32,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let Some(callback) = self.callbacks.lchown.clone() else {
            return Box::pin(async { Err(FsError::enosys("lchown")) });
        };
        self.chown_like(callback, path, uid, gid, "lchown")
    }

    fn truncate<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        length: u64,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let Some(callback) = self.callbacks.truncate.clone() else {
            return Box::pin(async { Err(FsError::enosys("truncate")) });
        };
        if length > 9_007_199_254_740_991 {
            return Box::pin(async {
                Err(FsError::new(ErrorCode::Eoverflow).with_syscall("truncate"))
            });
        }
        let lifecycle = Arc::clone(&self.lifecycle);
        let args = FnArgs::from((path.to_owned(), length as f64));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                "truncate",
                Arc::new(parse_unit),
            )
            .await
            .map_err(|error| error.into_fs_error("truncate", None, None))
        })
    }

    fn utimes<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        atime_ms: i64,
        mtime_ms: i64,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.timestamp_path(&self.callbacks.utimes, path, atime_ms, mtime_ms, "utimes")
    }

    fn lutimes<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        atime_ms: i64,
        mtime_ms: i64,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.timestamp_path(&self.callbacks.lutimes, path, atime_ms, mtime_ms, "lutimes")
    }

    fn has_utimens(&self) -> bool {
        self.callbacks.utimens.is_some()
    }

    fn utimens<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        atime_ns: i128,
        mtime_ns: i128,
        follow_symlinks: bool,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let Some(callback) = self.callbacks.utimens.clone() else {
            return Box::pin(async { Err(FsError::enosys("utimens")) });
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let args = FnArgs::from((
            path.to_owned(),
            BigInt::from(atime_ns),
            BigInt::from(mtime_ns),
            Some(JsUtimensOptions {
                follow_symlinks: Some(follow_symlinks),
            }),
        ));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                "utimens",
                Arc::new(parse_unit),
            )
            .await
            .map_err(|error| error.into_fs_error("utimens", None, None))
        })
    }

    fn mknod<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        mode: u32,
        dev: u64,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let Some(callback) = self.callbacks.mknod.clone() else {
            return Box::pin(async { Err(FsError::enosys("mknod")) });
        };
        if dev > 9_007_199_254_740_991 {
            return Box::pin(async {
                Err(FsError::new(ErrorCode::Eoverflow).with_syscall("mknod"))
            });
        }
        let lifecycle = Arc::clone(&self.lifecycle);
        let args = FnArgs::from((path.to_owned(), mode, dev as f64));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                "mknod",
                Arc::new(parse_unit),
            )
            .await
            .map_err(|error| error.into_fs_error("mknod", None, None))
        })
    }
}

impl JsDriver {
    fn two_path(
        &self,
        callback: &Option<Arc<CallbackSlot<TwoPathArgs>>>,
        first: &str,
        second: &str,
        operation: &'static str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + '_>> {
        let Some(callback) = callback.clone() else {
            return Box::pin(async move { Err(FsError::enosys(operation)) });
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let args = FnArgs::from((first.to_owned(), second.to_owned()));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                operation,
                Arc::new(parse_unit),
            )
            .await
            .map_err(|error| error.into_fs_error(operation, None, None))
        })
    }

    fn numeric_path(
        &self,
        callback: &Option<Arc<CallbackSlot<ChmodArgs>>>,
        path: &str,
        value: u32,
        operation: &'static str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + '_>> {
        let Some(callback) = callback.clone() else {
            return Box::pin(async move { Err(FsError::enosys(operation)) });
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let args = FnArgs::from((path.to_owned(), value));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                operation,
                Arc::new(parse_unit),
            )
            .await
            .map_err(|error| error.into_fs_error(operation, None, None))
        })
    }

    fn chown_like(
        &self,
        callback: Arc<CallbackSlot<ChownArgs>>,
        path: &str,
        uid: u32,
        gid: u32,
        operation: &'static str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + '_>> {
        let lifecycle = Arc::clone(&self.lifecycle);
        let args = FnArgs::from((path.to_owned(), uid, gid));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                operation,
                Arc::new(parse_unit),
            )
            .await
            .map_err(|error| error.into_fs_error(operation, None, None))
        })
    }

    fn timestamp_path(
        &self,
        callback: &Option<Arc<CallbackSlot<UtimeArgs>>>,
        path: &str,
        atime_ms: i64,
        mtime_ms: i64,
        operation: &'static str,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + '_>> {
        let Some(callback) = callback.clone() else {
            return Box::pin(async move { Err(FsError::enosys(operation)) });
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let args = FnArgs::from((
            path.to_owned(),
            atime_ms as f64 / 1000.0,
            mtime_ms as f64 / 1000.0,
        ));
        Box::pin(async move {
            invoke(
                callback,
                lifecycle.clone(),
                &lifecycle.waiters,
                args,
                operation,
                Arc::new(parse_unit),
            )
            .await
            .map_err(|error| error.into_fs_error(operation, None, None))
        })
    }
}

fn object_method<T>(
    object: Object<'_>,
    name: &str,
    lifecycle: &Arc<Lifecycle>,
) -> napi::Result<Option<Arc<CallbackSlot<T>>>>
where
    T: 'static + JsValuesTupleIntoVec,
{
    method_callback(object, lifecycle, name)
}

/// Construct a Rust `Filesystem` backed by a structural JavaScript
/// `FsDriver` object.  `lib.rs` owns the root N-API export; this function is
/// kept here so the adapter remains isolated from the existing native drivers.
#[napi]
pub fn create_driver(driver: Object<'_>) -> napi::Result<super::Filesystem> {
    let lifecycle = Lifecycle::new();
    let mountx = if driver.has_named_property("mountx")? {
        let value: Unknown = driver.get_named_property_unchecked("mountx")?;
        match value.get_type()? {
            ValueType::Null | ValueType::Undefined => None,
            ValueType::Object => {
                // SAFETY: the value type was checked above.
                Some(unsafe { Object::from_napi_value(value.value().env, value.raw()) }?)
            }
            _ => {
                return Err(Error::new(
                    Status::InvalidArg,
                    "driver mountx must be an object",
                ));
            }
        }
    } else {
        None
    };
    let has = |name: &str| has_function(&driver, name).unwrap_or(false);
    let capabilities = read_capabilities(
        &driver,
        has,
        mountx
            .as_ref()
            .map(|mountx| has_function(mountx, "mknod"))
            .transpose()?
            .unwrap_or(false),
    )?;

    let stat = build_callback(driver, "stat", &lifecycle, true)?.expect("required callback");
    let readdir = build_callback(driver, "readdir", &lifecycle, true)?.expect("required callback");
    let open = build_callback(driver, "open", &lifecycle, true)?.expect("required callback");
    let callbacks = DriverCallbacks {
        stat,
        readdir,
        open,
        lstat: object_method(driver, "lstat", &lifecycle)?,
        statfs: object_method(driver, "statfs", &lifecycle)?,
        mkdir: object_method(driver, "mkdir", &lifecycle)?,
        rmdir: object_method(driver, "rmdir", &lifecycle)?,
        unlink: object_method(driver, "unlink", &lifecycle)?,
        rename: object_method(driver, "rename", &lifecycle)?,
        link: object_method(driver, "link", &lifecycle)?,
        symlink: object_method(driver, "symlink", &lifecycle)?,
        readlink: object_method(driver, "readlink", &lifecycle)?,
        chmod: object_method(driver, "chmod", &lifecycle)?,
        chown: object_method(driver, "chown", &lifecycle)?,
        lchown: object_method(driver, "lchown", &lifecycle)?,
        truncate: object_method(driver, "truncate", &lifecycle)?,
        utimes: object_method(driver, "utimes", &lifecycle)?,
        lutimes: object_method(driver, "lutimes", &lifecycle)?,
        utimens: mountx
            .as_ref()
            .map(|mountx| object_method(*mountx, "utimens", &lifecycle))
            .transpose()?
            .flatten(),
        mknod: mountx
            .as_ref()
            .map(|mountx| object_method(*mountx, "mknod", &lifecycle))
            .transpose()?
            .flatten(),
    };
    let lifecycle_for_shutdown = Arc::clone(&lifecycle);
    let shutdown: Arc<super::ShutdownCallback> = Arc::new(move || {
        lifecycle_for_shutdown.shutdown();
        Box::pin(async { Ok::<(), FsError>(()) })
    });
    Ok(super::Filesystem {
        driver: super::instrument_driver(Arc::new(JsDriver {
            lifecycle,
            callbacks,
            capabilities,
        })),
        shutdown: Some(shutdown),
        reconcile: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn numeric_open_flags_preserve_write_only_without_create_or_truncate() {
        let flags = OpenFlags {
            read: false,
            write: true,
            create: false,
            truncate: false,
            append: false,
            exclusive: false,
        };
        assert_eq!(encode_open_flags(flags, "/file").unwrap(), 1.0);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn numeric_open_flags_match_platform_node_bits() {
        let flags = OpenFlags::parse("ax+", "/file").unwrap();
        let encoded = encode_open_flags(flags, "/file").unwrap();
        #[cfg(target_os = "linux")]
        let expected = 2_u64 | 0o100 | 0o200 | 0o2000;
        #[cfg(target_os = "macos")]
        let expected = 2_u64 | 0x0200 | 0x0800 | 0x0008;
        #[cfg(target_os = "windows")]
        let expected = 2_u64 | 0x0100 | 0x0400 | 0x0008;
        assert_eq!(encoded, expected as f64);
    }

    #[test]
    fn numeric_open_flags_reject_missing_access_mode() {
        let flags = OpenFlags {
            read: false,
            write: false,
            create: false,
            truncate: false,
            append: false,
            exclusive: false,
        };
        let error = encode_open_flags(flags, "/file").unwrap_err();
        assert!(error.is(ErrorCode::Einval));
    }
}
