pub mod kv_binding;
pub mod memory_factory;
pub mod servers;
pub mod utilities;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use mount_rs_auto::{
    AutoMount, AutoMountError, AutoMountOptions, AutoTransport, Transport, TransportProbe,
};
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle as CoreFileHandle, FsDriver, FsError, MemoryFs,
    MkdirOptions, OpenFlags, Result as CoreResult, Stats, StatsFs,
};
use mount_rs_host::{HostFs, HostFsOptions};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_pglite::{
    PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions, connect_pglite_with_store,
};
use mount_rs_r2::{R2BlockStore, R2Config, open_r2};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore, open_sqlite};
use napi::bindgen_prelude::{Buffer, Either};
use napi::{Error, Status};
use napi_derive::napi;

const ERROR_MARKER: &str = "__mount_rs_error_v1__";
const RANGE_ERROR_MARKER: &str = "__mount_rs_range_error_v1__";
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn hex(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn format_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        value.to_string()
    }
}

fn range_error(name: &str, expected: &str, value: f64) -> Error {
    Error::new(
        Status::InvalidArg,
        format!(
            "{RANGE_ERROR_MARKER}|{}|{}|{}",
            hex(name),
            hex(expected),
            hex(&format_number(value))
        ),
    )
}

/// Keep the POSIX fields intact until the package's small JS postlude turns
/// this into a normal Node-style Error. N-API's `Error` type only carries a
/// status and reason, so encoding the fields here avoids dropping syscall,
/// path, destination, and errno information at the native boundary.
fn to_js_error(error: FsError) -> Error {
    let message = error.to_string();
    Error::new(
        Status::GenericFailure,
        format!(
            "{ERROR_MARKER}|{}|{}|{}|{}|{}|{}",
            error.code.as_str(),
            -error.code.errno(),
            error
                .syscall
                .as_deref()
                .map(hex)
                .unwrap_or_else(|| "-".to_owned()),
            error
                .path
                .as_deref()
                .map(hex)
                .unwrap_or_else(|| "-".to_owned()),
            error
                .dest
                .as_deref()
                .map(hex)
                .unwrap_or_else(|| "-".to_owned()),
            hex(&message)
        ),
    )
}

fn normalized_path(path: &str) -> String {
    mount_rs_core::path::normalize_path(path)
}

#[derive(Debug, Clone, Copy)]
struct IoRange {
    start: usize,
    count: usize,
}

/// Match mountx's `FileHandleLike` validation order. In particular, a zero
/// length read may use an offset beyond the buffer, while a write may not.
fn validate_range(
    buffer_len: usize,
    offset: Option<f64>,
    length: Option<f64>,
    write: bool,
) -> Result<IoRange, Error> {
    let start_value = offset.unwrap_or(0.0);
    if !start_value.is_finite() || start_value.fract() != 0.0 {
        return Err(range_error("offset", "an integer", start_value));
    }
    if !(0.0..=MAX_SAFE_INTEGER).contains(&start_value) {
        return Err(range_error(
            "offset",
            ">= 0 && <= 9007199254740991",
            start_value,
        ));
    }
    if write && start_value > buffer_len as f64 {
        return Err(range_error(
            "offset",
            &format!("<= {buffer_len}"),
            start_value,
        ));
    }

    let count_value = length.unwrap_or(buffer_len as f64 - start_value);
    if count_value < 0.0 {
        return Err(range_error("length", ">= 0", count_value));
    }
    if count_value > 0.0 && count_value > buffer_len as f64 - start_value {
        return Err(range_error(
            "length",
            &format!("<= {}", buffer_len as f64 - start_value),
            count_value,
        ));
    }
    if !count_value.is_finite() || count_value.fract() != 0.0 {
        return Err(range_error("length", "an integer", count_value));
    }

    let start = usize::try_from(start_value as u64)
        .map_err(|_| range_error("offset", "a platform-sized integer", start_value))?;
    let count = usize::try_from(count_value as u64)
        .map_err(|_| range_error("length", "a platform-sized integer", count_value))?;
    Ok(IoRange { start, count })
}

fn validate_position(position: Option<f64>) -> Result<Option<u64>, Error> {
    let Some(value) = position else {
        return Ok(None);
    };
    if value == -1.0 {
        return Ok(None);
    }
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(range_error("position", "an integer", value));
    }
    if !(-1.0..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(range_error(
            "position",
            ">= -1 && <= 9007199254740991",
            value,
        ));
    }
    Ok(Some(value as u64))
}

fn validate_u32(name: &str, value: f64) -> Result<u32, Error> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > u32::MAX as f64 {
        return Err(range_error(name, ">= 0 && <= 4294967295", value));
    }
    Ok(value as u32)
}

fn validate_owner_id(name: &str, value: f64) -> Result<u32, Error> {
    if value == -1.0 {
        return Ok(u32::MAX);
    }
    validate_u32(name, value)
}

fn validate_length(value: Option<f64>) -> Result<u64, Error> {
    let value = value.unwrap_or(0.0);
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(range_error("length", ">= 0 && <= 9007199254740991", value));
    }
    Ok(value as u64)
}

fn validate_dev(value: f64) -> Result<u64, Error> {
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(range_error("dev", ">= 0 && <= 9007199254740991", value));
    }
    Ok(value as u64)
}

fn timestamp_ms(name: &str, seconds: f64) -> Result<i64, Error> {
    if !seconds.is_finite() {
        return Err(range_error(name, "a finite number", seconds));
    }
    let millis = seconds * 1000.0;
    // `as i64` saturates on current Rust, which would silently turn an
    // out-of-range timestamp into i64::MAX/MIN. Reject it before conversion.
    if !millis.is_finite() || millis < i64::MIN as f64 || millis >= 9_223_372_036_854_775_808.0 {
        return Err(range_error(
            name,
            "a timestamp representable in milliseconds",
            seconds,
        ));
    }
    Ok(millis as i64)
}

/// Decode Node's numeric `fs.constants` namespace. O_* values are not shared
/// between Linux and Darwin (notably O_CREAT/O_EXCL/O_TRUNC/O_APPEND), so this
/// is deliberately cfg'd instead of using the common Linux table in shared
/// code.
fn decode_numeric_flags(bits: f64, _path: &str) -> Result<OpenFlags, Error> {
    // The upstream parser applies JavaScript's ToInt32 through `&`, rather
    // than validating the number first. Preserve that behavior for unusual
    // numeric inputs as well as the normal node:fs constants: NaN and
    // infinities become zero, fractions truncate toward zero, and values are
    // reduced modulo 2^32 before the platform flag bits are inspected.
    let bits = if !bits.is_finite() {
        0
    } else {
        bits.trunc().rem_euclid(4_294_967_296.0) as u32
    } as u64;

    #[cfg(target_os = "linux")]
    const O_CREAT: u64 = 0o100;
    #[cfg(target_os = "linux")]
    const O_EXCL: u64 = 0o200;
    #[cfg(target_os = "linux")]
    const O_TRUNC: u64 = 0o1000;
    #[cfg(target_os = "linux")]
    const O_APPEND: u64 = 0o2000;

    #[cfg(target_os = "macos")]
    const O_CREAT: u64 = 0x0200;
    #[cfg(target_os = "macos")]
    const O_EXCL: u64 = 0x0800;
    #[cfg(target_os = "macos")]
    const O_TRUNC: u64 = 0x0400;
    #[cfg(target_os = "macos")]
    const O_APPEND: u64 = 0x0008;

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(to_js_error(
        FsError::new(ErrorCode::Einval)
            .with_syscall("open")
            .with_path(_path)
            .with_message("numeric open flags are unsupported on this platform"),
    ));

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let access = bits & 0x3;
        Ok(OpenFlags {
            read: access == 0 || access == 2,
            write: access == 1 || access == 2,
            create: bits & O_CREAT != 0,
            truncate: bits & O_TRUNC != 0,
            append: bits & O_APPEND != 0,
            exclusive: bits & O_EXCL != 0,
        })
    }
}

fn parse_open_flags(flags: Either<String, f64>, path: &str) -> Result<OpenFlags, Error> {
    match flags {
        Either::A(value) => OpenFlags::parse(&value, path).map_err(to_js_error),
        Either::B(value) => decode_numeric_flags(value, path),
    }
}

#[napi(object)]
pub struct JsMkdirOptions {
    pub recursive: Option<bool>,
    pub mode: Option<f64>,
}

fn parse_mkdir_options(
    options: Option<Either<bool, JsMkdirOptions>>,
) -> Result<MkdirOptions, Error> {
    match options {
        None => Ok(MkdirOptions {
            recursive: false,
            mode: None,
        }),
        Some(Either::A(recursive)) => Ok(MkdirOptions {
            recursive,
            mode: None,
        }),
        Some(Either::B(options)) => Ok(MkdirOptions {
            recursive: options.recursive.unwrap_or(false),
            mode: options
                .mode
                .map(|mode| validate_u32("mode", mode))
                .transpose()?,
        }),
    }
}

#[napi(object)]
pub struct JsReaddirOptions {
    pub with_file_types: Option<bool>,
}

#[napi(object)]
pub struct JsCapabilities {
    pub handles: bool,
    pub hardlinks: bool,
    pub symlinks: bool,
    pub permissions: bool,
    pub times: bool,
    pub truncate: bool,
    pub atomic_rename: bool,
    pub case_sensitive: bool,
    pub statfs: bool,
    pub read_only: bool,
    pub durable_writes: bool,
    pub mknod: bool,
    #[napi(ts_type = "ReadonlyArray<string>")]
    pub extensions: Vec<String>,
}

impl From<Capabilities> for JsCapabilities {
    fn from(value: Capabilities) -> Self {
        Self {
            handles: value.handles,
            hardlinks: value.hardlinks,
            symlinks: value.symlinks,
            permissions: value.permissions,
            times: value.times,
            truncate: value.truncate,
            atomic_rename: value.atomic_rename,
            case_sensitive: value.case_sensitive,
            statfs: value.statfs,
            read_only: value.read_only,
            durable_writes: value.durable_writes,
            mknod: value.mknod,
            extensions: if value.mknod {
                vec!["mknod".to_owned()]
            } else {
                Vec::new()
            },
        }
    }
}

#[napi]
pub struct JsStats {
    inner: Stats,
}

impl From<Stats> for JsStats {
    fn from(value: Stats) -> Self {
        Self { inner: value }
    }
}

#[napi]
impl JsStats {
    #[napi(getter)]
    pub fn dev(&self) -> f64 {
        self.inner.dev as f64
    }
    #[napi(getter)]
    pub fn ino(&self) -> f64 {
        self.inner.ino as f64
    }
    #[napi(getter)]
    pub fn mode(&self) -> u32 {
        self.inner.mode
    }
    #[napi(getter)]
    pub fn nlink(&self) -> f64 {
        self.inner.nlink as f64
    }
    #[napi(getter)]
    pub fn uid(&self) -> u32 {
        self.inner.uid
    }
    #[napi(getter)]
    pub fn gid(&self) -> u32 {
        self.inner.gid
    }
    #[napi(getter)]
    pub fn rdev(&self) -> f64 {
        self.inner.rdev as f64
    }
    #[napi(getter)]
    pub fn size(&self) -> f64 {
        self.inner.size as f64
    }
    #[napi(getter)]
    pub fn blksize(&self) -> f64 {
        self.inner.blksize as f64
    }
    #[napi(getter)]
    pub fn blocks(&self) -> f64 {
        self.inner.blocks as f64
    }
    #[napi(getter)]
    pub fn atime_ms(&self) -> f64 {
        self.inner.atime_ms as f64
    }
    #[napi(getter)]
    pub fn mtime_ms(&self) -> f64 {
        self.inner.mtime_ms as f64
    }
    #[napi(getter)]
    pub fn ctime_ms(&self) -> f64 {
        self.inner.ctime_ms as f64
    }
    #[napi(getter)]
    pub fn birthtime_ms(&self) -> f64 {
        self.inner.birthtime_ms as f64
    }

    #[napi]
    pub fn is_file(&self) -> bool {
        self.inner.is_file()
    }
    #[napi]
    pub fn is_directory(&self) -> bool {
        self.inner.is_directory()
    }
    #[napi]
    pub fn is_symbolic_link(&self) -> bool {
        self.inner.is_symbolic_link()
    }
    #[napi]
    pub fn is_block_device(&self) -> bool {
        self.inner.is_block_device()
    }
    #[napi]
    pub fn is_character_device(&self) -> bool {
        self.inner.is_character_device()
    }
    #[napi(js_name = "isFIFO")]
    pub fn is_fifo(&self) -> bool {
        self.inner.is_fifo()
    }
    #[napi]
    pub fn is_socket(&self) -> bool {
        self.inner.is_socket()
    }
}

#[napi]
pub struct JsStatsFs {
    inner: StatsFs,
}

impl From<StatsFs> for JsStatsFs {
    fn from(value: StatsFs) -> Self {
        Self { inner: value }
    }
}

#[napi]
impl JsStatsFs {
    #[napi(getter, js_name = "type")]
    pub fn stat_type(&self) -> f64 {
        self.inner.filesystem_type as f64
    }
    #[napi(getter)]
    pub fn bsize(&self) -> f64 {
        self.inner.block_size as f64
    }
    #[napi(getter)]
    pub fn blocks(&self) -> f64 {
        self.inner.blocks as f64
    }
    #[napi(getter)]
    pub fn bfree(&self) -> f64 {
        self.inner.blocks_free as f64
    }
    #[napi(getter)]
    pub fn bavail(&self) -> f64 {
        self.inner.blocks_available as f64
    }
    #[napi(getter)]
    pub fn files(&self) -> f64 {
        self.inner.files as f64
    }
    #[napi(getter)]
    pub fn ffree(&self) -> f64 {
        self.inner.files_free as f64
    }

    #[napi(getter)]
    pub fn filesystem_type(&self) -> f64 {
        self.inner.filesystem_type as f64
    }
    #[napi(getter)]
    pub fn block_size(&self) -> f64 {
        self.inner.block_size as f64
    }
    #[napi(getter)]
    pub fn blocks_free(&self) -> f64 {
        self.inner.blocks_free as f64
    }
    #[napi(getter)]
    pub fn blocks_available(&self) -> f64 {
        self.inner.blocks_available as f64
    }
    #[napi(getter)]
    pub fn files_free(&self) -> f64 {
        self.inner.files_free as f64
    }
}

#[napi]
pub struct JsDirEntry {
    inner: DirEntry,
}

#[napi]
pub struct JsMountx {
    driver: Arc<dyn FsDriver>,
}

#[napi]
impl JsMountx {
    #[napi]
    pub async fn mknod(&self, path: String, mode: f64, dev: f64) -> napi::Result<()> {
        self.driver
            .mknod(
                &normalized_path(&path),
                validate_u32("mode", mode)?,
                validate_dev(dev)?,
            )
            .await
            .map_err(to_js_error)
    }
}

impl From<DirEntry> for JsDirEntry {
    fn from(value: DirEntry) -> Self {
        Self { inner: value }
    }
}

#[napi]
impl JsDirEntry {
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.name.clone()
    }
    #[napi(getter)]
    pub fn parent_path(&self) -> String {
        self.inner.parent_path.clone()
    }
    #[napi(getter)]
    pub fn file_type(&self) -> String {
        format!("{:?}", self.inner.file_type).to_lowercase()
    }

    #[napi]
    pub fn is_file(&self) -> bool {
        self.inner.is_file()
    }
    #[napi]
    pub fn is_directory(&self) -> bool {
        self.inner.is_directory()
    }
    #[napi]
    pub fn is_symbolic_link(&self) -> bool {
        self.inner.is_symbolic_link()
    }
    #[napi]
    pub fn is_block_device(&self) -> bool {
        self.inner.is_block_device()
    }
    #[napi]
    pub fn is_character_device(&self) -> bool {
        self.inner.is_character_device()
    }
    #[napi(js_name = "isFIFO")]
    pub fn is_fifo(&self) -> bool {
        self.inner.is_fifo()
    }
    #[napi]
    pub fn is_socket(&self) -> bool {
        self.inner.is_socket()
    }
}

#[napi(object)]
pub struct JsR2Options {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub state_key: Option<String>,
}

#[napi(object)]
pub struct JsNodeFsOptions {
    pub read_only: Option<bool>,
}

#[napi(object)]
pub struct JsAutoMountOptions {
    pub transport: Option<String>,
    pub read_only: Option<bool>,
    pub unmount_timeout_ms: Option<f64>,
    /// Apply hard mounts and same-host locking when the selected transport is
    /// NFS. This does not enable WAL or distributed SQLite locking.
    pub nfs_sqlite_single_host: Option<bool>,
}

#[napi(object)]
pub struct JsTransportProbe {
    pub usable: bool,
    pub reason: Option<String>,
}

#[napi(object)]
pub struct JsAutoProbe {
    pub platform: String,
    pub chosen: Option<String>,
    pub preference: Vec<String>,
    pub fuse: JsTransportProbe,
    #[napi(js_name = "9p")]
    pub nine_p: JsTransportProbe,
    pub nfs: JsTransportProbe,
    pub reason: Option<String>,
}

#[napi(object)]
pub struct JsMountFailure {
    pub transport: Option<String>,
    pub message: String,
}

/// One independently configured provider used by `createChunkedDriver`.
/// `kind` is intentionally a closed string set validated by Rust; an unknown
/// backend never falls back to an in-memory store.
#[napi(object)]
pub struct JsChunkedStoreOptions {
    pub kind: String,
    pub uri: Option<String>,
    pub key: Option<String>,
    pub durable: Option<bool>,
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

#[napi(object)]
pub struct JsChunkedOptions {
    pub metadata: JsChunkedStoreOptions,
    pub blocks: JsChunkedStoreOptions,
    pub chunk_size: f64,
    pub owner: Option<String>,
    pub ttl_ms: Option<f64>,
    pub uid: Option<f64>,
    pub gid: Option<f64>,
    pub umask: Option<f64>,
    pub root_mode: Option<f64>,
}

/// The selected provider is dynamic at the JavaScript boundary, while the
/// chunked driver remains generic over concrete Rust types. These adapters
/// forward the async-trait ABI manually so N-API does not need another direct
/// runtime dependency just to erase the provider choice.
#[derive(Clone)]
struct DynMetadataStore(Arc<dyn MetadataStore>);

impl MetadataStore for DynMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable()
    }

    fn load<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<LoadedMetadata>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        Box::pin(async move { inner.load().await })
    }

    fn acquire_writer<'a, 'b, 'async_trait>(
        &'a self,
        owner: &'b str,
        ttl: Duration,
    ) -> Pin<Box<dyn Future<Output = CoreResult<WriterLease>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        let owner = owner.to_owned();
        Box::pin(async move { inner.acquire_writer(&owner, ttl).await })
    }

    fn renew_writer<'a, 'b, 'async_trait>(
        &'a self,
        lease: &'b WriterLease,
        ttl: Duration,
    ) -> Pin<Box<dyn Future<Output = CoreResult<WriterLease>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        let lease = lease.clone();
        Box::pin(async move { inner.renew_writer(&lease, ttl).await })
    }

    fn release_writer<'a, 'b, 'async_trait>(
        &'a self,
        lease: &'b WriterLease,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        let lease = lease.clone();
        Box::pin(async move { inner.release_writer(&lease).await })
    }

    fn publish<'a, 'b, 'async_trait>(
        &'a self,
        expected_revision: u64,
        lease: &'b WriterLease,
        namespace: Namespace,
    ) -> Pin<Box<dyn Future<Output = CoreResult<u64>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        let lease = lease.clone();
        Box::pin(async move { inner.publish(expected_revision, &lease, namespace).await })
    }

    fn flush<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        Box::pin(async move { inner.flush().await })
    }
}

#[derive(Clone)]
struct DynBlockStore(Arc<dyn BlockStore>);

impl BlockStore for DynBlockStore {
    fn durable(&self) -> bool {
        self.0.durable()
    }

    fn put<'a, 'b, 'async_trait>(
        &'a self,
        bytes: &'b [u8],
    ) -> Pin<Box<dyn Future<Output = CoreResult<BlockId>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        let bytes = bytes.to_vec();
        Box::pin(async move { inner.put(&bytes).await })
    }

    fn get<'a, 'b, 'async_trait>(
        &'a self,
        id: &'b BlockId,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Vec<u8>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        let id = id.clone();
        Box::pin(async move { inner.get(&id).await })
    }

    fn flush<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        Box::pin(async move { inner.flush().await })
    }

    fn delete<'a, 'b, 'async_trait>(
        &'a self,
        id: &'b BlockId,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        let id = id.clone();
        Box::pin(async move { inner.delete(&id).await })
    }
}

/// An owned forwarding driver for the native transport facade. The N-API
/// `Filesystem` class keeps its driver behind an `Arc`; transport crates take
/// ownership of a concrete `FsDriver`, so this small adapter preserves the
/// same shared driver without adding another async runtime dependency.
#[derive(Clone)]
struct MountDriver(Arc<dyn FsDriver>);

impl FsDriver for MountDriver {
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }

    fn syncfs<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.0);
        Box::pin(async move { inner.syncfs().await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.stat(&path).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.lstat(&path).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.statfs(&path).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.readdir(&path).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        let flags = flags.to_owned();
        Box::pin(async move { inner.open(&path, &flags, mode).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.open_flags(&path, flags, mode).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.mkdir(&path, options).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.rmdir(&path).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.unlink(&path).await })
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
        let inner = Arc::clone(&self.0);
        let old_path = old_path.to_owned();
        let new_path = new_path.to_owned();
        Box::pin(async move { inner.rename(&old_path, &new_path).await })
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
        let inner = Arc::clone(&self.0);
        let existing_path = existing_path.to_owned();
        let new_path = new_path.to_owned();
        Box::pin(async move { inner.link(&existing_path, &new_path).await })
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
        let inner = Arc::clone(&self.0);
        let target = target.to_owned();
        let path = path.to_owned();
        Box::pin(async move { inner.symlink(&target, &path).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.readlink(&path).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.chmod(&path, mode).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.chown(&path, uid, gid).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.lchown(&path, uid, gid).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.truncate(&path, length).await })
    }

    fn has_utimens(&self) -> bool {
        self.0.has_utimens()
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move {
            inner
                .utimens(&path, atime_ns, mtime_ns, follow_symlinks)
                .await
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.utimes(&path, atime_ms, mtime_ms).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.lutimes(&path, atime_ms, mtime_ms).await })
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
        let inner = Arc::clone(&self.0);
        let path = path.to_owned();
        Box::pin(async move { inner.mknod(&path, mode, dev).await })
    }
}

fn config_error(message: impl Into<String>) -> Error {
    to_js_error(
        FsError::new(ErrorCode::Einval)
            .with_syscall("createChunkedDriver")
            .with_message(message),
    )
}

fn reject_set<T>(value: &Option<T>, field: &str) -> Result<(), Error> {
    if value.is_some() {
        Err(config_error(format!(
            "{field} is not valid for this backend"
        )))
    } else {
        Ok(())
    }
}

fn required_string(value: &Option<String>, field: &str) -> Result<String, Error> {
    let value = value
        .as_deref()
        .ok_or_else(|| config_error(format!("{field} is required")))?;
    if value.trim().is_empty() {
        return Err(config_error(format!("{field} must not be empty")));
    }
    Ok(value.to_owned())
}

fn validate_chunk_size(value: f64) -> Result<usize, Error> {
    if !value.is_finite()
        || value.fract() != 0.0
        || value <= 0.0
        || value > MAX_SAFE_INTEGER
        || value > usize::MAX as f64
    {
        return Err(range_error(
            "chunkSize",
            "> 0, an integer, and platform-sized",
            value,
        ));
    }
    usize::try_from(value as u64)
        .map_err(|_| range_error("chunkSize", "a platform-sized integer", value))
}

fn validate_ttl(value: Option<f64>) -> Result<Duration, Error> {
    let value = value.unwrap_or(30_000.0);
    if !value.is_finite()
        || value.fract() != 0.0
        || value == 0.0
        || !(0.0..=MAX_SAFE_INTEGER).contains(&value)
    {
        return Err(range_error("ttlMs", "> 0 and <= 9007199254740991", value));
    }
    Ok(Duration::from_millis(value as u64))
}

fn optional_u32(name: &str, value: Option<f64>, default: u32) -> Result<u32, Error> {
    value
        .map(|value| validate_u32(name, value))
        .unwrap_or(Ok(default))
}

async fn build_metadata_store(
    options: &JsChunkedStoreOptions,
) -> Result<(Arc<dyn MetadataStore>, Option<PgliteMetadataStore>), Error> {
    match options.kind.as_str() {
        "memory" => {
            reject_set(&options.uri, "metadata.uri")?;
            reject_set(&options.key, "metadata.key")?;
            reject_set(&options.durable, "metadata.durable")?;
            reject_set(&options.endpoint, "metadata.endpoint")?;
            reject_set(&options.bucket, "metadata.bucket")?;
            reject_set(&options.access_key_id, "metadata.accessKeyId")?;
            reject_set(&options.secret_access_key, "metadata.secretAccessKey")?;
            Ok((Arc::new(MemoryMetadataStore::new()), None))
        }
        "sqlite" => {
            let uri = required_string(&options.uri, "metadata.uri")?;
            reject_set(&options.key, "metadata.key")?;
            reject_set(&options.durable, "metadata.durable")?;
            reject_set(&options.endpoint, "metadata.endpoint")?;
            reject_set(&options.bucket, "metadata.bucket")?;
            reject_set(&options.access_key_id, "metadata.accessKeyId")?;
            reject_set(&options.secret_access_key, "metadata.secretAccessKey")?;
            Ok((
                Arc::new(SqliteMetadataStore::open(uri).map_err(to_js_error)?),
                None,
            ))
        }
        "pglite" => {
            let uri = required_string(&options.uri, "metadata.uri")?;
            let key = required_string(&options.key, "metadata.key")?;
            reject_set(&options.endpoint, "metadata.endpoint")?;
            reject_set(&options.bucket, "metadata.bucket")?;
            reject_set(&options.access_key_id, "metadata.accessKeyId")?;
            reject_set(&options.secret_access_key, "metadata.secretAccessKey")?;
            let storage =
                PgliteStorageOptions::new(key).with_durable(options.durable.unwrap_or(false));
            let store = PgliteMetadataStore::connect_with_options(&uri, storage)
                .await
                .map_err(to_js_error)?;
            Ok((Arc::new(store.clone()), Some(store)))
        }
        "r2" => Err(config_error(
            "R2 is a block-only backend; metadata must use memory, sqlite, or pglite",
        )),
        other => Err(config_error(format!("unknown metadata backend: {other}"))),
    }
}

async fn build_block_store(
    options: &JsChunkedStoreOptions,
) -> Result<(Arc<dyn BlockStore>, Option<PgliteBlockStore>), Error> {
    match options.kind.as_str() {
        "memory" => {
            reject_set(&options.uri, "blocks.uri")?;
            reject_set(&options.key, "blocks.key")?;
            reject_set(&options.durable, "blocks.durable")?;
            reject_set(&options.endpoint, "blocks.endpoint")?;
            reject_set(&options.bucket, "blocks.bucket")?;
            reject_set(&options.access_key_id, "blocks.accessKeyId")?;
            reject_set(&options.secret_access_key, "blocks.secretAccessKey")?;
            Ok((Arc::new(MemoryBlockStore::new()), None))
        }
        "sqlite" => {
            let uri = required_string(&options.uri, "blocks.uri")?;
            reject_set(&options.key, "blocks.key")?;
            reject_set(&options.durable, "blocks.durable")?;
            reject_set(&options.endpoint, "blocks.endpoint")?;
            reject_set(&options.bucket, "blocks.bucket")?;
            reject_set(&options.access_key_id, "blocks.accessKeyId")?;
            reject_set(&options.secret_access_key, "blocks.secretAccessKey")?;
            Ok((
                Arc::new(SqliteBlockStore::open(uri).map_err(to_js_error)?),
                None,
            ))
        }
        "pglite" => {
            let uri = required_string(&options.uri, "blocks.uri")?;
            let key = required_string(&options.key, "blocks.key")?;
            reject_set(&options.endpoint, "blocks.endpoint")?;
            reject_set(&options.bucket, "blocks.bucket")?;
            reject_set(&options.access_key_id, "blocks.accessKeyId")?;
            reject_set(&options.secret_access_key, "blocks.secretAccessKey")?;
            let storage =
                PgliteStorageOptions::new(key).with_durable(options.durable.unwrap_or(false));
            let store = PgliteBlockStore::connect_with_options(&uri, storage)
                .await
                .map_err(to_js_error)?;
            Ok((Arc::new(store.clone()), Some(store)))
        }
        "r2" => {
            let prefix = required_string(&options.key, "blocks.key")?;
            let endpoint = required_string(&options.endpoint, "blocks.endpoint")?;
            let bucket = required_string(&options.bucket, "blocks.bucket")?;
            let access_key_id = required_string(&options.access_key_id, "blocks.accessKeyId")?;
            let secret_access_key =
                required_string(&options.secret_access_key, "blocks.secretAccessKey")?;
            reject_set(&options.uri, "blocks.uri")?;
            let config = R2Config {
                endpoint,
                bucket,
                access_key_id,
                secret_access_key,
                // The chunked provider only uses R2 for immutable blocks. The
                // snapshot state key is retained solely to satisfy the shared
                // R2 configuration type and is never opened here.
                state_key: "mount-rs-napi/unused-state".to_owned(),
            };
            let blocks = match options.durable {
                Some(durable) => {
                    R2BlockStore::new(config.build_store().map_err(to_js_error)?, prefix, durable)
                }
                None => R2BlockStore::from_config(&config, prefix),
            }
            .map_err(to_js_error)?;
            Ok((Arc::new(blocks), None))
        }
        other => Err(config_error(format!("unknown block backend: {other}"))),
    }
}

static NEXT_CHUNKED_OWNER: AtomicU64 = AtomicU64::new(1);

fn chunked_owner(owner: Option<String>) -> Result<String, Error> {
    if let Some(owner) = owner {
        if owner.trim().is_empty() {
            return Err(config_error("owner must not be empty"));
        }
        return Ok(owner);
    }
    let sequence = NEXT_CHUNKED_OWNER.fetch_add(1, Ordering::Relaxed);
    Ok(format!("mount-rs-napi-{}-{sequence}", std::process::id()))
}

fn transport_name(transport: Transport) -> String {
    match transport {
        Transport::Fuse => "fuse",
        Transport::P9 => "9p",
        Transport::Nfs => "nfs",
    }
    .to_owned()
}

fn transport_probe(probe: TransportProbe) -> JsTransportProbe {
    JsTransportProbe {
        usable: probe.usable,
        reason: probe.reason,
    }
}

fn auto_probe(probe: mount_rs_auto::AutoProbe) -> JsAutoProbe {
    // Rust names the Darwin target `macos`, while Node's public platform
    // contract (and the upstream TypeScript facade) uses `darwin`.
    let platform = match probe.platform.as_str() {
        "macos" => "darwin".to_owned(),
        platform => platform.to_owned(),
    };
    JsAutoProbe {
        platform,
        chosen: probe.chosen.map(transport_name),
        preference: probe.preference.into_iter().map(transport_name).collect(),
        fuse: transport_probe(probe.fuse),
        nine_p: transport_probe(probe.p9),
        nfs: transport_probe(probe.nfs),
        reason: probe.reason,
    }
}

fn parse_auto_transport(value: Option<String>) -> Result<AutoTransport, Error> {
    match value.as_deref().unwrap_or("auto") {
        "auto" => Ok(AutoTransport::Auto),
        "fuse" => Ok(AutoTransport::Fuse),
        "9p" => Ok(AutoTransport::P9),
        "nfs" => Ok(AutoTransport::Nfs),
        other => Err(config_error(format!(
            "unknown transport {other}; expected auto, fuse, 9p, or nfs"
        ))),
    }
}

fn validate_unmount_timeout(value: Option<f64>) -> Result<Option<Duration>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(range_error(
            "unmountTimeoutMs",
            ">= 0 and <= 9007199254740991",
            value,
        ));
    }
    Ok(Some(Duration::from_millis(value as u64)))
}

fn auto_options(options: Option<JsAutoMountOptions>) -> Result<AutoMountOptions, Error> {
    let options = options.unwrap_or(JsAutoMountOptions {
        transport: None,
        read_only: None,
        unmount_timeout_ms: None,
        nfs_sqlite_single_host: None,
    });
    Ok(AutoMountOptions {
        transport: parse_auto_transport(options.transport)?,
        read_only: options.read_only,
        unmount_timeout: validate_unmount_timeout(options.unmount_timeout_ms)?,
        fuse: None,
        p9: None,
        nfs: options
            .nfs_sqlite_single_host
            .unwrap_or(false)
            .then(mount_rs_nfs::NfsMountOptions::sqlite_single_host),
    })
}

fn auto_mount_error(error: AutoMountError, syscall: &str, path: Option<&str>) -> Error {
    let code = match &error {
        AutoMountError::NoTransport(_) => ErrorCode::Enodev,
        AutoMountError::Fuse(_) | AutoMountError::P9(_) | AutoMountError::Nfs(_) => ErrorCode::Eio,
    };
    let message = error.to_string();
    let error = FsError::new(code)
        .with_syscall(syscall)
        .with_message(message);
    match path {
        Some(path) => to_js_error(error.with_path(path)),
        None => to_js_error(error),
    }
}

fn mount_failure(error: AutoMountError) -> JsMountFailure {
    let transport = match &error {
        AutoMountError::NoTransport(_) => None,
        AutoMountError::Fuse(_) => Some("fuse"),
        AutoMountError::P9(_) => Some("9p"),
        AutoMountError::Nfs(_) => Some("nfs"),
    };
    JsMountFailure {
        transport: transport.map(str::to_owned),
        message: error.to_string(),
    }
}

#[napi]
pub struct Mounted {
    inner: Arc<AutoMount>,
}

#[napi]
impl Mounted {
    #[napi(getter)]
    pub fn transport(&self) -> String {
        transport_name(self.inner.transport())
    }

    #[napi(getter)]
    pub fn mountpoint(&self) -> String {
        self.inner.mountpoint().to_string_lossy().into_owned()
    }

    #[napi(getter)]
    pub fn source(&self) -> Option<String> {
        self.inner.source().map(str::to_owned)
    }

    #[napi(getter)]
    pub fn active(&self) -> bool {
        self.inner.active()
    }

    #[napi]
    pub async fn unmount(&self) -> napi::Result<()> {
        self.inner
            .unmount()
            .await
            .map_err(|error| auto_mount_error(error, "unmount", None))
    }
}

#[napi(object)]
pub struct JsReadResult {
    pub bytes_read: f64,
    #[napi(ts_type = "Uint8Array")]
    pub buffer: Buffer,
}

#[napi(object)]
pub struct JsWriteResult {
    pub bytes_written: f64,
    #[napi(ts_type = "Uint8Array")]
    pub buffer: Buffer,
}

/// A Node-facing wrapper around one real core open handle. The core handle
/// owns the cursor and access mode; this layer only performs Node's
/// offset/length/position slicing and returns the requested buffer/result
/// shapes.
#[napi]
pub struct FileHandle {
    inner: Arc<dyn CoreFileHandle>,
}

#[napi]
impl FileHandle {
    #[napi(getter)]
    pub fn fd(&self) -> Option<f64> {
        self.inner.fd().map(|value| value as f64)
    }

    #[napi]
    pub async fn read(
        &self,
        #[napi(ts_arg_type = "Uint8Array")] mut buffer: Buffer,
        offset: Option<f64>,
        length: Option<f64>,
        position: Option<f64>,
    ) -> napi::Result<JsReadResult> {
        let range = validate_range(buffer.len(), offset, length, false)?;
        let position = validate_position(position)?;
        let mut target = vec![0_u8; range.count];
        let bytes_read = self
            .inner
            .read(&mut target, position)
            .await
            .map_err(to_js_error)?;
        if bytes_read > range.count {
            return Err(to_js_error(
                FsError::new(ErrorCode::Eio).with_syscall("read"),
            ));
        }
        if bytes_read > 0 {
            buffer[range.start..range.start + bytes_read].copy_from_slice(&target[..bytes_read]);
        }
        Ok(JsReadResult {
            bytes_read: bytes_read as f64,
            buffer,
        })
    }

    #[napi]
    pub async fn write(
        &self,
        #[napi(ts_arg_type = "Uint8Array")] buffer: Buffer,
        offset: Option<f64>,
        length: Option<f64>,
        position: Option<f64>,
    ) -> napi::Result<JsWriteResult> {
        let range = validate_range(buffer.len(), offset, length, true)?;
        let position = validate_position(position)?;
        let bytes_written = self
            .inner
            .write(&buffer[range.start..range.start + range.count], position)
            .await
            .map_err(to_js_error)?;
        if bytes_written > range.count {
            return Err(to_js_error(
                FsError::new(ErrorCode::Eio).with_syscall("write"),
            ));
        }
        Ok(JsWriteResult {
            bytes_written: bytes_written as f64,
            buffer,
        })
    }

    #[napi]
    pub async fn stat(&self) -> napi::Result<JsStats> {
        self.inner.stat().await.map(Into::into).map_err(to_js_error)
    }

    #[napi]
    pub async fn truncate(&self, length: Option<f64>) -> napi::Result<()> {
        self.inner
            .truncate(validate_length(length)?)
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn sync(&self) -> napi::Result<()> {
        self.inner.sync().await.map_err(to_js_error)
    }

    #[napi]
    pub async fn datasync(&self) -> napi::Result<()> {
        self.inner.datasync().await.map_err(to_js_error)
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.inner.close().await.map_err(to_js_error)
    }
}

/// A Node.js-facing wrapper around any core driver.
type ShutdownFuture = Pin<Box<dyn Future<Output = CoreResult<()>> + Send>>;
type ShutdownCallback = dyn Fn() -> ShutdownFuture + Send + Sync;

#[napi]
pub struct Filesystem {
    driver: Arc<dyn FsDriver>,
    shutdown: Option<Arc<ShutdownCallback>>,
}

#[napi]
impl Filesystem {
    #[napi(factory)]
    pub fn memory() -> Self {
        Self {
            driver: Arc::new(MemoryFs::empty()),
            shutdown: None,
        }
    }

    #[napi(factory)]
    pub async fn sqlite(path: String) -> napi::Result<Self> {
        open_sqlite(path)
            .await
            .map(|filesystem| Self {
                driver: Arc::new(filesystem),
                shutdown: None,
            })
            .map_err(to_js_error)
    }

    #[napi(factory)]
    pub async fn pglite(connection_string: String) -> napi::Result<Self> {
        let (filesystem, store) = connect_pglite_with_store(&connection_string, "mount-rs")
            .await
            .map_err(to_js_error)?;
        let shutdown_store = store.clone();
        let shutdown: Arc<ShutdownCallback> = Arc::new(move || {
            let store = shutdown_store.clone();
            Box::pin(async move { store.close().await })
        });
        Ok(Self {
            driver: Arc::new(filesystem),
            shutdown: Some(shutdown),
        })
    }

    #[napi(factory)]
    pub async fn r2(options: JsR2Options) -> napi::Result<Self> {
        let config = R2Config {
            endpoint: options.endpoint,
            bucket: options.bucket,
            access_key_id: options.access_key_id,
            secret_access_key: options.secret_access_key,
            state_key: options
                .state_key
                .unwrap_or_else(|| "mount-rs/state.json".to_owned()),
        };
        open_r2(config)
            .await
            .map(|filesystem| Self {
                driver: Arc::new(filesystem),
                shutdown: None,
            })
            .map_err(to_js_error)
    }

    fn resolved_capabilities(&self) -> JsCapabilities {
        self.driver.capabilities().into()
    }

    /// Upstream exposes resolved capabilities as a readonly property.
    #[napi(getter)]
    pub fn capabilities(&self) -> JsCapabilities {
        self.resolved_capabilities()
    }

    /// Compatibility alias for callers of the earlier Rust binding method.
    #[napi(js_name = "getCapabilities")]
    pub fn get_capabilities(&self) -> JsCapabilities {
        self.resolved_capabilities()
    }

    /// Release the chunked metadata writer lease immediately. Legacy
    /// snapshot factories have no lease and therefore resolve successfully.
    #[napi]
    pub async fn shutdown(&self) -> napi::Result<()> {
        if let Some(shutdown) = &self.shutdown {
            shutdown().await.map_err(to_js_error)?;
        }
        Ok(())
    }

    #[napi]
    pub async fn stat(&self, path: String) -> napi::Result<JsStats> {
        self.driver
            .stat(&normalized_path(&path))
            .await
            .map(Into::into)
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn lstat(&self, path: String) -> napi::Result<JsStats> {
        self.driver
            .lstat(&normalized_path(&path))
            .await
            .map(Into::into)
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn statfs(&self, path: String) -> napi::Result<JsStatsFs> {
        self.driver
            .statfs(&normalized_path(&path))
            .await
            .map(Into::into)
            .map_err(to_js_error)
    }

    #[napi(js_name = "readdir")]
    pub async fn read_dir(
        &self,
        path: String,
        _options: Option<JsReaddirOptions>,
    ) -> napi::Result<Vec<JsDirEntry>> {
        self.driver
            .readdir(&normalized_path(&path))
            .await
            .map(|entries| entries.into_iter().map(Into::into).collect())
            .map_err(to_js_error)
    }

    #[napi(getter, js_name = "mountx")]
    pub fn mountx(&self) -> JsMountx {
        JsMountx {
            driver: Arc::clone(&self.driver),
        }
    }

    #[napi]
    pub async fn open(
        &self,
        path: String,
        flags: Option<Either<String, f64>>,
        mode: Option<u32>,
    ) -> napi::Result<FileHandle> {
        let path = normalized_path(&path);
        let flags = parse_open_flags(flags.unwrap_or_else(|| Either::A("r".to_owned())), &path)?;
        self.driver
            .open_flags(&path, flags, mode.unwrap_or(0o666))
            .await
            .map(|inner| FileHandle { inner })
            .map_err(to_js_error)
    }

    #[napi(ts_return_type = "Promise<Uint8Array>")]
    pub async fn read_file(&self, path: String) -> napi::Result<Buffer> {
        let path = normalized_path(&path);
        let handle = self
            .driver
            .open_flags(&path, OpenFlags::READ_ONLY, 0)
            .await
            .map_err(to_js_error)?;
        let operation = async {
            let mut output = Vec::new();
            let mut position = 0_u64;
            loop {
                let mut buffer = vec![0_u8; 64 * 1024];
                let count = handle
                    .read(&mut buffer, Some(position))
                    .await
                    .map_err(to_js_error)?;
                if count > buffer.len() {
                    return Err(to_js_error(
                        FsError::new(ErrorCode::Eio)
                            .with_syscall("read")
                            .with_path(&path),
                    ));
                }
                if count == 0 {
                    break;
                }
                output.extend_from_slice(&buffer[..count]);
                position = position.saturating_add(count as u64);
            }
            Ok(Buffer::from(output))
        }
        .await;
        let close = handle.close().await.map_err(to_js_error);
        match (operation, close) {
            (_, Err(error)) => Err(error),
            (result, Ok(())) => result,
        }
    }

    #[napi]
    pub async fn write_file(
        &self,
        path: String,
        #[napi(ts_arg_type = "string | Uint8Array")] data: Either<String, Buffer>,
    ) -> napi::Result<()> {
        let path = normalized_path(&path);
        let data = match data {
            Either::A(data) => data.into_bytes(),
            Either::B(data) => data.to_vec(),
        };
        let handle = self
            .driver
            .open_flags(
                &path,
                OpenFlags {
                    read: false,
                    write: true,
                    create: true,
                    truncate: true,
                    append: false,
                    exclusive: false,
                },
                0o666,
            )
            .await
            .map_err(to_js_error)?;
        let operation = async {
            let mut written = 0_usize;
            while written < data.len() {
                let count = handle
                    .write(&data[written..], Some(written as u64))
                    .await
                    .map_err(to_js_error)?;
                if count == 0 || count > data.len() - written {
                    return Err(to_js_error(
                        FsError::new(ErrorCode::Eio)
                            .with_syscall("write")
                            .with_path(&path),
                    ));
                }
                written += count;
            }
            Ok(())
        }
        .await;
        let close = handle.close().await.map_err(to_js_error);
        match (operation, close) {
            (_, Err(error)) => Err(error),
            (result, Ok(())) => result,
        }
    }

    /// Return the first path created by a recursive mkdir, or `undefined` when
    /// the directory already existed or the call was non-recursive.
    #[napi(ts_return_type = "Promise<string | undefined>")]
    pub async fn mkdir(
        &self,
        path: String,
        options: Option<Either<bool, JsMkdirOptions>>,
    ) -> napi::Result<Option<String>> {
        self.driver
            .mkdir(&normalized_path(&path), parse_mkdir_options(options)?)
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn rmdir(&self, path: String) -> napi::Result<()> {
        self.driver
            .rmdir(&normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn unlink(&self, path: String) -> napi::Result<()> {
        self.driver
            .unlink(&normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn rename(&self, old_path: String, new_path: String) -> napi::Result<()> {
        self.driver
            .rename(&normalized_path(&old_path), &normalized_path(&new_path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn link(&self, existing_path: String, new_path: String) -> napi::Result<()> {
        self.driver
            .link(
                &normalized_path(&existing_path),
                &normalized_path(&new_path),
            )
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn symlink(
        &self,
        target: String,
        path: String,
        _type: Option<String>,
    ) -> napi::Result<()> {
        self.driver
            .symlink(&target, &normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn readlink(&self, path: String) -> napi::Result<String> {
        self.driver
            .readlink(&normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn chmod(&self, path: String, mode: f64) -> napi::Result<()> {
        self.driver
            .chmod(&normalized_path(&path), validate_u32("mode", mode)?)
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn chown(&self, path: String, uid: f64, gid: f64) -> napi::Result<()> {
        self.driver
            .chown(
                &normalized_path(&path),
                validate_owner_id("uid", uid)?,
                validate_owner_id("gid", gid)?,
            )
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn lchown(&self, path: String, uid: f64, gid: f64) -> napi::Result<()> {
        self.driver
            .lchown(
                &normalized_path(&path),
                validate_owner_id("uid", uid)?,
                validate_owner_id("gid", gid)?,
            )
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn truncate(&self, path: String, length: Option<f64>) -> napi::Result<()> {
        self.driver
            .truncate(&normalized_path(&path), validate_length(length)?)
            .await
            .map_err(to_js_error)
    }

    /// Match mountx's public TimeLike number form: seconds since epoch. Date
    /// values are converted by `postlude.cjs` before reaching this native
    /// method; the core contract stores whole milliseconds.
    #[napi]
    pub async fn utimes(
        &self,
        path: String,
        #[napi(ts_arg_type = "number | Date")] atime: f64,
        #[napi(ts_arg_type = "number | Date")] mtime: f64,
    ) -> napi::Result<()> {
        self.driver
            .utimes(
                &normalized_path(&path),
                timestamp_ms("atime", atime)?,
                timestamp_ms("mtime", mtime)?,
            )
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn lutimes(
        &self,
        path: String,
        #[napi(ts_arg_type = "number | Date")] atime: f64,
        #[napi(ts_arg_type = "number | Date")] mtime: f64,
    ) -> napi::Result<()> {
        self.driver
            .lutimes(
                &normalized_path(&path),
                timestamp_ms("atime", atime)?,
                timestamp_ms("mtime", mtime)?,
            )
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn mknod(&self, path: String, mode: f64, dev: f64) -> napi::Result<()> {
        self.driver
            .mknod(
                &normalized_path(&path),
                validate_u32("mode", mode)?,
                validate_dev(dev)?,
            )
            .await
            .map_err(to_js_error)
    }
}

/// Construct a filesystem over independently selected metadata and immutable
/// block providers. The returned driver's `shutdown()` releases its writer
/// lease; callers should invoke it when the driver is no longer in use.
async fn shutdown_chunked_filesystem(
    filesystem: ChunkedFs<DynMetadataStore, DynBlockStore>,
    metadata: Option<PgliteMetadataStore>,
    blocks: Option<PgliteBlockStore>,
) -> CoreResult<()> {
    // Always attempt provider teardown even if lease release reports an
    // error. A stale lease must not keep the PostgreSQL-wire clients alive
    // until JavaScript garbage-collects the retained Filesystem object.
    let filesystem_result = filesystem.shutdown().await;
    let metadata_result = match metadata {
        Some(store) => store.close().await,
        None => Ok(()),
    };
    let blocks_result = match blocks {
        Some(store) => store.close().await,
        None => Ok(()),
    };

    filesystem_result
        .err()
        .or_else(|| metadata_result.err())
        .or_else(|| blocks_result.err())
        .map_or(Ok(()), Err)
}

#[napi]
pub async fn create_chunked_driver(options: JsChunkedOptions) -> napi::Result<Filesystem> {
    let (metadata_store, metadata_close) = build_metadata_store(&options.metadata).await?;
    let (block_store, blocks_close) = build_block_store(&options.blocks).await?;
    let metadata = DynMetadataStore(metadata_store);
    let blocks = DynBlockStore(block_store);
    let owner = chunked_owner(options.owner)?;
    let chunk_size = validate_chunk_size(options.chunk_size)?;
    let ttl = validate_ttl(options.ttl_ms)?;
    let uid = optional_u32("uid", options.uid, 0)?;
    let gid = optional_u32("gid", options.gid, 0)?;
    let umask = optional_u32("umask", options.umask, 0)?;
    let root_mode = optional_u32("rootMode", options.root_mode, 0o755)?;
    let chunk_options = ChunkedOptions::fixed(owner, chunk_size)
        .map_err(to_js_error)?
        .with_lease_ttl(ttl)
        .with_identity(uid, gid, umask)
        .with_root_mode(root_mode);

    let filesystem = ChunkedFs::open(metadata, blocks, chunk_options)
        .await
        .map_err(to_js_error)?;
    let shutdown_filesystem = filesystem.clone();
    let shutdown_metadata = metadata_close;
    let shutdown_blocks = blocks_close;
    let shutdown: Arc<ShutdownCallback> = Arc::new(move || {
        let filesystem = shutdown_filesystem.clone();
        let metadata = shutdown_metadata.clone();
        let blocks = shutdown_blocks.clone();
        Box::pin(async move { shutdown_chunked_filesystem(filesystem, metadata, blocks).await })
    });
    Ok(Filesystem {
        driver: Arc::new(filesystem),
        shutdown: Some(shutdown),
    })
}

/// Create the rooted host-filesystem driver used by the upstream
/// `createNodeFsDriver` API. Construction is synchronous; host I/O remains
/// asynchronous inside the Rust driver and the root is resolved lexically.
#[napi]
pub fn create_node_fs_driver(root: String, options: Option<JsNodeFsOptions>) -> Filesystem {
    let read_only = options
        .and_then(|options| options.read_only)
        .unwrap_or(false);
    Filesystem {
        driver: Arc::new(HostFs::with_options(root, HostFsOptions { read_only })),
        shutdown: None,
    }
}

/// Probe host facts without attempting a mount. This is safe and rootless on
/// macOS and Linux; it reports the exact automatic preference and reasons.
#[napi]
pub async fn probe_transports() -> JsAutoProbe {
    auto_probe(mount_rs_auto::probe_transports())
}

/// Mount a filesystem through the named transport or the automatic facade.
/// The Rust transport remains authoritative for platform prerequisites; this
/// function never silently falls back after a named transport fails.
#[napi]
pub async fn mount(
    driver: &Filesystem,
    mountpoint: String,
    options: Option<JsAutoMountOptions>,
) -> napi::Result<Mounted> {
    let options = auto_options(options)?;
    let mountpoint_for_error = mountpoint.clone();
    let mounted =
        mount_rs_auto::mount(MountDriver(Arc::clone(&driver.driver)), mountpoint, options)
            .await
            .map_err(|error| auto_mount_error(error, "mount", Some(&mountpoint_for_error)))?;
    Ok(Mounted {
        inner: Arc::new(mounted),
    })
}

/// Return facade-visible live mounts. The result contains shared lifecycle
/// handles; callers still own explicit `unmount()` responsibility.
#[napi]
pub async fn live_mounts() -> Vec<Mounted> {
    mount_rs_auto::live_mounts()
        .into_iter()
        .map(|mount| Mounted {
            inner: Arc::new(mount),
        })
        .collect()
}

/// Tear down every mount visible to the automatic facade and return failures
/// individually instead of rejecting the cleanup operation.
#[napi]
pub async fn unmount_all() -> Vec<JsMountFailure> {
    mount_rs_auto::unmount_all()
        .await
        .into_iter()
        .map(mount_failure)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_flags_use_the_host_namespace() {
        let flags = decode_numeric_flags(0.0, "/file").expect("O_RDONLY");
        assert_eq!(flags, OpenFlags::READ_ONLY);

        #[cfg(target_os = "linux")]
        let bits = 1 | 0o100 | 0o1000 | 0o2000 | 0o200;
        #[cfg(target_os = "macos")]
        let bits = 1 | 0x0200 | 0x0400 | 0x0008 | 0x0800;
        let flags = decode_numeric_flags(bits as f64, "/file").expect("numeric flags");
        assert!(flags.write);
        assert!(flags.create);
        assert!(flags.truncate);
        assert!(flags.append);
        assert!(flags.exclusive);
    }

    #[test]
    fn numeric_flags_match_upstream_access_mode_decoding() {
        let flags = decode_numeric_flags(3.0, "/file").expect("numeric access mode");
        assert!(!flags.read);
        assert!(!flags.write);
    }

    #[test]
    fn numeric_flags_preserve_javascript_to_int32_coercion() {
        let nan = decode_numeric_flags(f64::NAN, "/file").expect("NaN flags");
        assert_eq!(nan, OpenFlags::READ_ONLY);

        let fractional = decode_numeric_flags(1.9, "/file").expect("fractional flags");
        assert!(!fractional.read);
        assert!(fractional.write);

        let negative = decode_numeric_flags(-1.0, "/file").expect("negative flags");
        assert!(!negative.read);
        assert!(!negative.write);
        assert!(negative.create);
        assert!(negative.truncate);
        assert!(negative.append);
        assert!(negative.exclusive);
    }

    #[test]
    fn range_validation_matches_node_zero_read_exception() {
        let range = validate_range(4, Some(8.0), Some(0.0), false).expect("zero read");
        assert_eq!(range.count, 0);
        assert_eq!(range.start, 8);
        assert!(validate_range(4, Some(8.0), Some(0.0), true).is_err());
    }

    #[test]
    fn ownership_sentinel_and_timestamp_bounds_are_checked() {
        assert_eq!(validate_owner_id("uid", -1.0).unwrap(), u32::MAX);
        assert_eq!(timestamp_ms("time", 1.5).unwrap(), 1500);
        assert!(timestamp_ms("time", f64::INFINITY).is_err());
        assert!(validate_length(Some(MAX_SAFE_INTEGER + 1.0)).is_err());
    }

    #[test]
    fn chunked_options_validate_fixed_size_and_lease_values() {
        assert_eq!(validate_chunk_size(4096.0).unwrap(), 4096);
        assert!(validate_chunk_size(0.0).is_err());
        assert!(validate_chunk_size(1.5).is_err());
        assert_eq!(validate_ttl(None).unwrap(), Duration::from_secs(30));
        assert_eq!(
            validate_ttl(Some(120.0)).unwrap(),
            Duration::from_millis(120)
        );
        assert!(validate_ttl(Some(0.0)).is_err());
    }

    #[test]
    fn chunked_owner_is_unique_by_default_and_rejects_empty_explicit_ids() {
        let first = chunked_owner(None).unwrap();
        let second = chunked_owner(None).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            chunked_owner(Some("caller-owner".to_owned())).unwrap(),
            "caller-owner"
        );
        assert!(chunked_owner(Some("  ".to_owned())).is_err());
    }

    #[test]
    fn auto_facade_preserves_node_platform_names_and_async_option_bounds() {
        let probe = auto_probe(mount_rs_auto::probe_transports_for("macos"));
        assert_eq!(probe.platform, "darwin");
        assert_eq!(probe.preference, vec!["nfs", "fuse", "9p"]);

        assert_eq!(parse_auto_transport(None).unwrap(), AutoTransport::Auto);
        assert_eq!(
            parse_auto_transport(Some("9p".to_owned())).unwrap(),
            AutoTransport::P9
        );
        assert!(parse_auto_transport(Some("bogus".to_owned())).is_err());
        assert_eq!(
            validate_unmount_timeout(Some(0.0)).unwrap(),
            Some(Duration::ZERO)
        );
        assert!(validate_unmount_timeout(Some(1.5)).is_err());
    }

    #[test]
    fn auto_facade_exposes_opt_in_nfs_sqlite_profile() {
        assert!(auto_options(None).unwrap().nfs.is_none());
        let options = auto_options(Some(JsAutoMountOptions {
            transport: Some("nfs".into()),
            read_only: None,
            unmount_timeout_ms: None,
            nfs_sqlite_single_host: Some(true),
        }))
        .unwrap();
        let nfs = options.nfs.unwrap();
        assert!(nfs.hard);
        let native =
            mount_rs_nfs::native::nfs_mount_options(2049, &nfs, mount_rs_nfs::NfsPlatform::Macos)
                .unwrap();
        assert!(native.split(',').any(|value| value == "locallocks"));
        assert!(native.split(',').any(|value| value == "hard"));
    }
}
