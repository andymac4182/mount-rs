//! N-API transport for the Rust key-value filesystem.
//!
//! The filesystem semantics live in `mount-rs-kv`.  This module only keeps
//! references to the JavaScript store callbacks and turns their asynchronous
//! results into the `KeyValueStore` trait used by that crate.

use std::fmt;
use std::future::Future;
use std::os::raw::c_void;
use std::pin::Pin;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use mount_rs_kv::{KeyValueMetadata, KeyValueStore, UnstorageOptions};
use napi::bindgen_prelude::{
    Either, FnArgs, FromNapiValue, Function, JsObjectValue, JsValuesTupleIntoVec, Object, Promise,
    TypeName, Unknown, ValidateNapiValue, check_pending_exception, check_status,
};
use napi::threadsafe_function::ThreadsafeFunction;
use napi::{Error, JsDate, Status, ValueType, sys};
use napi_derive::napi;

// Keep the TSFN referenced while a Filesystem exists so an in-flight Rust
// filesystem Promise cannot be stranded when Node has no other active handle.
// `CallbackSet::release` explicitly drops these strong references at shutdown.
type JsCallback<T, R> = ThreadsafeFunction<T, Either<Promise<R>, R>, T, Status, false, false>;

type KeyCall = FnArgs<(String,)>;
type SetCall = FnArgs<(String, napi::bindgen_prelude::Uint8Array)>;

type HasItemCallback = JsCallback<KeyCall, bool>;
type GetItemRawCallback = JsCallback<KeyCall, Option<JsRawBytes>>;
type SetItemRawCallback = JsCallback<SetCall, ()>;
type RemoveItemCallback = JsCallback<KeyCall, ()>;
type GetKeysCallback = JsCallback<KeyCall, Vec<String>>;
type GetMetaCallback = JsCallback<KeyCall, Option<JsMetadata>>;

#[derive(Debug)]
struct JsStoreError {
    operation: &'static str,
    reason: String,
}

impl JsStoreError {
    fn napi(operation: &'static str, error: Error) -> Self {
        let reason = if error.reason.is_empty() {
            format!("callback failed with {}", error.status)
        } else {
            error.reason
        };
        Self { operation, reason }
    }

    fn closed() -> Self {
        Self {
            operation: "store",
            reason: "callback adapter has been shut down".to_owned(),
        }
    }
}

impl fmt::Display for JsStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} callback failed: {}",
            self.operation, self.reason
        )
    }
}

/// A raw value copied while the callback is running on Node's JS thread.
///
/// Keeping only owned bytes here is important: `KeyValueStore` futures are
/// `Send`, while N-API typed-array views are backed by JS memory and must not
/// be retained by a Rust worker after the callback returns.
#[derive(Debug, Clone)]
struct JsRawBytes(Vec<u8>);

impl TypeName for JsRawBytes {
    fn type_name() -> &'static str {
        "raw byte value"
    }

    fn value_type() -> ValueType {
        ValueType::Unknown
    }
}

impl ValidateNapiValue for JsRawBytes {
    unsafe fn validate(
        _env: sys::napi_env,
        _napi_value: sys::napi_value,
    ) -> napi::Result<sys::napi_value> {
        // `getItemRaw` is typed as `unknown` by unstorage.  Validation is
        // deliberately deferred to `from_napi_value`, which accepts the
        // byte-array forms plus the string/JSON forms handled by upstream's
        // `toBytes` helper.
        Ok(ptr::null_mut())
    }
}

fn copy_raw_bytes(data: *mut c_void, length: usize, kind: &str) -> napi::Result<Vec<u8>> {
    if length == 0 {
        return Ok(Vec::new());
    }
    if data.is_null() {
        return Err(Error::new(
            Status::InvalidArg,
            format!("{kind} reported a null data pointer for {length} bytes"),
        ));
    }
    // SAFETY: each caller obtained this pointer and byte length from the
    // corresponding N-API info function during this conversion.
    Ok(unsafe { slice::from_raw_parts(data.cast::<u8>(), length) }.to_vec())
}

fn stringify_json(env: sys::napi_env, value: sys::napi_value) -> napi::Result<String> {
    let mut global = ptr::null_mut();
    check_status!(unsafe { sys::napi_get_global(env, &mut global) })?;

    let mut json = ptr::null_mut();
    check_status!(unsafe {
        sys::napi_get_named_property(env, global, c"JSON".as_ptr(), &mut json)
    })?;

    let mut stringify = ptr::null_mut();
    check_status!(unsafe {
        sys::napi_get_named_property(env, json, c"stringify".as_ptr(), &mut stringify)
    })?;

    let mut output = ptr::null_mut();
    check_pending_exception!(
        env,
        unsafe { sys::napi_call_function(env, json, stringify, 1, [value].as_ptr(), &mut output) },
        "JSON.stringify failed"
    )?;

    unsafe { String::from_napi_value(env, output) }
}

impl FromNapiValue for JsRawBytes {
    unsafe fn from_napi_value(
        env: sys::napi_env,
        napi_value: sys::napi_value,
    ) -> napi::Result<Self> {
        let mut is_buffer = false;
        check_status!(unsafe { sys::napi_is_buffer(env, napi_value, &mut is_buffer) })?;
        if is_buffer {
            let mut data = ptr::null_mut();
            let mut length = 0;
            check_status!(unsafe {
                sys::napi_get_buffer_info(env, napi_value, &mut data, &mut length)
            })?;
            return Ok(Self(copy_raw_bytes(data, length, "Buffer")?));
        }

        let mut is_typed_array = false;
        check_status!(unsafe { sys::napi_is_typedarray(env, napi_value, &mut is_typed_array) })?;
        if is_typed_array {
            let mut array_type = 0;
            let mut length = 0;
            let mut data = ptr::null_mut();
            let mut array_buffer = ptr::null_mut();
            let mut byte_offset = 0;
            check_status!(unsafe {
                sys::napi_get_typedarray_info(
                    env,
                    napi_value,
                    &mut array_type,
                    &mut length,
                    &mut data,
                    &mut array_buffer,
                    &mut byte_offset,
                )
            })?;

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
                napi::bindgen_prelude::TypedArrayType::BigInt64
                | napi::bindgen_prelude::TypedArrayType::BigUint64 => 8,
                napi::bindgen_prelude::TypedArrayType::Unknown => {
                    return Err(Error::new(
                        Status::InvalidArg,
                        "unsupported JavaScript typed-array kind",
                    ));
                }
                _ => {
                    return Err(Error::new(
                        Status::InvalidArg,
                        "unsupported JavaScript typed-array kind",
                    ));
                }
            };
            let byte_length = length.checked_mul(bytes_per_element).ok_or_else(|| {
                Error::new(Status::InvalidArg, "typed-array byte length overflow")
            })?;
            return Ok(Self(copy_raw_bytes(data, byte_length, "typed array")?));
        }

        let mut is_data_view = false;
        check_status!(unsafe { sys::napi_is_dataview(env, napi_value, &mut is_data_view) })?;
        if is_data_view {
            let mut byte_length = 0;
            let mut data = ptr::null_mut();
            let mut array_buffer = ptr::null_mut();
            let mut byte_offset = 0;
            check_status!(unsafe {
                sys::napi_get_dataview_info(
                    env,
                    napi_value,
                    &mut byte_length,
                    &mut data,
                    &mut array_buffer,
                    &mut byte_offset,
                )
            })?;
            return Ok(Self(copy_raw_bytes(data, byte_length, "DataView")?));
        }

        let mut is_array_buffer = false;
        check_status!(unsafe { sys::napi_is_arraybuffer(env, napi_value, &mut is_array_buffer) })?;
        if is_array_buffer {
            let mut data = ptr::null_mut();
            let mut byte_length = 0;
            check_status!(unsafe {
                sys::napi_get_arraybuffer_info(env, napi_value, &mut data, &mut byte_length)
            })?;
            return Ok(Self(copy_raw_bytes(data, byte_length, "ArrayBuffer")?));
        }

        if let Ok(value) = unsafe { String::from_napi_value(env, napi_value) } {
            return Ok(Self(value.into_bytes()));
        }

        Ok(Self(stringify_json(env, napi_value)?.into_bytes()))
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct JsMetadata {
    size: Option<u64>,
    atime_ms: Option<i64>,
    mtime_ms: Option<i64>,
    ctime_ms: Option<i64>,
    birthtime_ms: Option<i64>,
}

impl TypeName for JsMetadata {
    fn type_name() -> &'static str {
        "StorageMeta"
    }

    fn value_type() -> ValueType {
        ValueType::Object
    }
}

impl ValidateNapiValue for JsMetadata {}

fn named_value<'env>(object: &Object<'env>, name: &str) -> napi::Result<Option<Unknown<'env>>> {
    let value: Unknown = object.get_named_property_unchecked(name)?;
    match value.get_type()? {
        ValueType::Null | ValueType::Undefined => Ok(None),
        _ => Ok(Some(value)),
    }
}

fn integer_millis(value: f64, name: &str) -> napi::Result<i64> {
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i64::MIN as f64
        || value >= i64::MAX as f64
    {
        return Err(Error::new(
            Status::InvalidArg,
            format!("metadata field '{name}' must be an i64 millisecond timestamp"),
        ));
    }
    Ok(value as i64)
}

fn metadata_millis(value: Unknown<'_>, name: &str) -> napi::Result<i64> {
    match value.get_type()? {
        ValueType::Number => {
            // SAFETY: the value type was checked above.
            integer_millis(unsafe { value.cast::<f64>()? }, name)
        }
        ValueType::Object => {
            // `value_of` performs the N-API Date check.  This also preserves
            // normal N-API errors for an object that is not a Date.
            let date: JsDate = unsafe { value.cast()? };
            integer_millis(date.value_of()?, name)
        }
        received => Err(Error::new(
            Status::InvalidArg,
            format!("metadata field '{name}' must be a Date or number, got {received}"),
        )),
    }
}

fn metadata_size(value: Unknown<'_>) -> napi::Result<u64> {
    if value.get_type()? != ValueType::Number {
        return Err(Error::new(
            Status::InvalidArg,
            "metadata field 'size' must be a number",
        ));
    }
    // SAFETY: the value type was checked above.
    let size = unsafe { value.cast::<f64>()? };
    if !size.is_finite() || size.fract() != 0.0 || size < 0.0 || size >= u64::MAX as f64 {
        return Err(Error::new(
            Status::InvalidArg,
            "metadata field 'size' must be a non-negative u64",
        ));
    }
    Ok(size as u64)
}

impl FromNapiValue for JsMetadata {
    unsafe fn from_napi_value(
        env: sys::napi_env,
        napi_value: sys::napi_value,
    ) -> napi::Result<Self> {
        let object: Object<'_> = unsafe { Object::from_napi_value(env, napi_value)? };
        Ok(Self {
            size: named_value(&object, "size")?
                .map(metadata_size)
                .transpose()?,
            atime_ms: named_value(&object, "atime")?
                .map(|value| metadata_millis(value, "atime"))
                .transpose()?,
            mtime_ms: named_value(&object, "mtime")?
                .map(|value| metadata_millis(value, "mtime"))
                .transpose()?,
            ctime_ms: named_value(&object, "ctime")?
                .map(|value| metadata_millis(value, "ctime"))
                .transpose()?,
            birthtime_ms: named_value(&object, "birthtime")?
                .map(|value| metadata_millis(value, "birthtime"))
                .transpose()?,
        })
    }
}

impl From<JsMetadata> for KeyValueMetadata {
    fn from(value: JsMetadata) -> Self {
        Self {
            size: value.size,
            atime_ms: value.atime_ms,
            mtime_ms: value.mtime_ms,
            ctime_ms: value.ctime_ms,
            birthtime_ms: value.birthtime_ms,
        }
    }
}

fn build_callback<'env, T, R>(store: Object<'env>, name: &str) -> napi::Result<JsCallback<T, R>>
where
    T: 'static + JsValuesTupleIntoVec,
    R: 'static + FromNapiValue + TypeName + ValidateNapiValue + Send,
{
    // The N-API function handle is retained by the TSFN.  Its lifetime
    // parameter is only a Rust scope marker; choosing `'static` here keeps
    // the callback argument marker out of the returned worker-safe handle.
    let function: Function<'static, Unknown<'static>, Either<Promise<R>, R>> =
        store.get_named_property(name)?;
    let function = function.bind(store)?;
    function
        .build_threadsafe_function::<T>()
        .weak::<false>()
        .callee_handled::<false>()
        .build_callback(|context| Ok(context.value))
}

struct Callbacks {
    has_item: Arc<HasItemCallback>,
    get_item_raw: Arc<GetItemRawCallback>,
    set_item_raw: Arc<SetItemRawCallback>,
    remove_item: Arc<RemoveItemCallback>,
    get_keys: Arc<GetKeysCallback>,
    get_meta: Option<Arc<GetMetaCallback>>,
}

struct CallbackSet {
    callbacks: Mutex<Option<Callbacks>>,
    closed: AtomicBool,
}

impl CallbackSet {
    fn from_store<'env>(store: Object<'env>) -> napi::Result<Self> {
        let get_meta = if store.has_named_property("getMeta")? {
            Some(Arc::new(build_callback::<KeyCall, Option<JsMetadata>>(
                store, "getMeta",
            )?))
        } else {
            None
        };
        Ok(Self {
            callbacks: Mutex::new(Some(Callbacks {
                has_item: Arc::new(build_callback(store, "hasItem")?),
                get_item_raw: Arc::new(build_callback(store, "getItemRaw")?),
                set_item_raw: Arc::new(build_callback(store, "setItemRaw")?),
                remove_item: Arc::new(build_callback(store, "removeItem")?),
                get_keys: Arc::new(build_callback(store, "getKeys")?),
                get_meta,
            })),
            closed: AtomicBool::new(false),
        })
    }

    fn release(&self) {
        self.closed.store(true, Ordering::SeqCst);
        match self.callbacks.lock() {
            Ok(mut callbacks) => drop(callbacks.take()),
            Err(poisoned) => drop(poisoned.into_inner().take()),
        }
    }

    fn callback<T>(&self, get: impl FnOnce(&Callbacks) -> T) -> Result<T, JsStoreError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(JsStoreError::closed());
        }
        let callbacks = self.callbacks.lock().map_err(|_| JsStoreError {
            operation: "store",
            reason: "callback mutex is poisoned".to_owned(),
        })?;
        callbacks.as_ref().map(get).ok_or_else(JsStoreError::closed)
    }
}

#[derive(Clone)]
struct JsKeyValueStore {
    callbacks: Arc<CallbackSet>,
}

async fn invoke<T, R>(
    callback: &JsCallback<T, R>,
    value: T,
    operation: &'static str,
) -> Result<R, JsStoreError>
where
    T: 'static + JsValuesTupleIntoVec,
    R: 'static + FromNapiValue + TypeName + ValidateNapiValue + Send,
{
    let result = callback
        .call_async_catch(value)
        .await
        .map_err(|error| JsStoreError::napi(operation, error))?;
    match result {
        Either::A(promise) => promise
            .await
            .map_err(|error| JsStoreError::napi(operation, error)),
        Either::B(value) => Ok(value),
    }
}

impl KeyValueStore for JsKeyValueStore {
    type Error = JsStoreError;

    fn has_item<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let callbacks = Arc::clone(&self.callbacks);
        let key = key.to_owned();
        Box::pin(async move {
            let callback = callbacks.callback(|callbacks| callbacks.has_item.clone())?;
            invoke(&callback, FnArgs::from((key,)), "hasItem").await
        })
    }

    fn get_item_raw<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let callbacks = Arc::clone(&self.callbacks);
        let key = key.to_owned();
        Box::pin(async move {
            let callback = callbacks.callback(|callbacks| callbacks.get_item_raw.clone())?;
            invoke(&callback, FnArgs::from((key,)), "getItemRaw")
                .await
                .map(|value| value.map(|bytes| bytes.0))
        })
    }

    fn set_item_raw<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
        value: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let callbacks = Arc::clone(&self.callbacks);
        let key = key.to_owned();
        let value = napi::bindgen_prelude::Uint8Array::from(value);
        Box::pin(async move {
            let callback = callbacks.callback(|callbacks| callbacks.set_item_raw.clone())?;
            invoke(&callback, FnArgs::from((key, value)), "setItemRaw").await
        })
    }

    fn remove_item<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let callbacks = Arc::clone(&self.callbacks);
        let key = key.to_owned();
        Box::pin(async move {
            let callback = callbacks.callback(|callbacks| callbacks.remove_item.clone())?;
            invoke(&callback, FnArgs::from((key,)), "removeItem").await
        })
    }

    fn get_keys<'a, 'b, 'async_trait>(
        &'a self,
        prefix: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let callbacks = Arc::clone(&self.callbacks);
        let prefix = prefix.to_owned();
        Box::pin(async move {
            let callback = callbacks.callback(|callbacks| callbacks.get_keys.clone())?;
            invoke(&callback, FnArgs::from((prefix,)), "getKeys").await
        })
    }

    fn get_meta<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<KeyValueMetadata, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let callbacks = Arc::clone(&self.callbacks);
        let key = key.to_owned();
        Box::pin(async move {
            let callback = callbacks.callback(|callbacks| callbacks.get_meta.clone());
            let Some(callback) = callback? else {
                return Ok(KeyValueMetadata::default());
            };
            invoke(&callback, FnArgs::from((key,)), "getMeta")
                .await
                .map(|value| value.map(Into::into).unwrap_or_default())
        })
    }
}

#[napi(object)]
pub struct JsUnstorageOptions {
    pub uid: Option<f64>,
    pub gid: Option<f64>,
    pub file_mode: Option<f64>,
    pub dir_mode: Option<f64>,
    pub read_only: Option<bool>,
}

fn kv_options(options: Option<JsUnstorageOptions>) -> napi::Result<UnstorageOptions> {
    let defaults = UnstorageOptions::default();
    let options = options.unwrap_or(JsUnstorageOptions {
        uid: None,
        gid: None,
        file_mode: None,
        dir_mode: None,
        read_only: None,
    });
    Ok(UnstorageOptions {
        uid: options
            .uid
            .map(|value| super::validate_u32("uid", value))
            .transpose()?
            .unwrap_or(defaults.uid),
        gid: options
            .gid
            .map(|value| super::validate_u32("gid", value))
            .transpose()?
            .unwrap_or(defaults.gid),
        file_mode: options
            .file_mode
            .map(|value| super::validate_u32("fileMode", value))
            .transpose()?
            .unwrap_or(defaults.file_mode),
        dir_mode: options
            .dir_mode
            .map(|value| super::validate_u32("dirMode", value))
            .transpose()?
            .unwrap_or(defaults.dir_mode),
        read_only: options.read_only.unwrap_or(defaults.read_only),
    })
}

/// Create a filesystem over an unstorage-compatible JavaScript store.
///
/// The callback names and arguments match the unstorage `Storage` surface;
/// each callback may return its value directly or a Promise.  The Rust KV
/// driver remains responsible for path mapping, directory discovery, handle
/// buffering, metadata overlays, read-only checks, and flush/close behavior.
#[napi]
pub fn create_unstorage_driver(
    store: Object<'_>,
    options: Option<JsUnstorageOptions>,
) -> napi::Result<super::Filesystem> {
    let callbacks = Arc::new(CallbackSet::from_store(store)?);
    let store = JsKeyValueStore {
        callbacks: Arc::clone(&callbacks),
    };
    let filesystem = mount_rs_kv::create_unstorage_driver(store, kv_options(options)?);
    let shutdown_callbacks = Arc::clone(&callbacks);
    let shutdown: Arc<super::ShutdownCallback> = Arc::new(move || {
        shutdown_callbacks.release();
        Box::pin(async { Ok::<(), mount_rs_core::FsError>(()) })
    });

    Ok(super::Filesystem {
        driver: super::instrument_driver(Arc::new(filesystem)),
        shutdown: Some(shutdown),
        reconcile: None,
    })
}
