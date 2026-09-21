pub mod fuse_codec;
pub mod fuse_inodes;
pub mod js_driver;
pub mod kv_binding;
pub mod memory_factory;
pub mod nfs_codec;
pub mod p9_codec;
pub mod servers;
pub mod utilities;

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use mount_rs_auto::{
    AutoMount, AutoMountError, AutoMountOptions, AutoTransport, Transport, TransportProbe,
};
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{
    BlockId, BlockReconcileReport, BlockStore, LoadedMetadata, MetadataStore, Namespace,
    WriterLease,
};
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle as CoreFileHandle, FsDriver, FsError, MemoryFs,
    MkdirOptions, OpenFlags, Result as CoreResult, Stats, StatsFs,
};
#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
use mount_rs_foundationdb::{
    FoundationDbLimits, FoundationDbSharedLeaseOracle, FoundationDbStorage,
    FoundationDbStorageOptions,
};
use mount_rs_host::{HostFs, HostFsOptions};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
#[cfg(feature = "observability")]
use mount_rs_observability::{InstrumentedDriver, Telemetry};
use mount_rs_pglite::{
    PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions, connect_pglite_with_store,
};
use mount_rs_r2::{R2BlockStore, R2Config, open_r2};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore, open_sqlite};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use napi::bindgen_prelude::{Buffer, Either};
use napi::{Error, Status};
use napi_derive::napi;

const ERROR_MARKER: &str = "__mount_rs_error_v1__";
const RANGE_ERROR_MARKER: &str = "__mount_rs_range_error_v1__";
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Apply the optional Node application-boundary decorator at construction
/// time. The feature is off by default; when enabled, an embedding Rust
/// application may install a process-wide handle, while Node consumers can
/// opt in with `MOUNT_RS_TELEMETRY=1` before loading the native module.
pub(crate) fn instrument_driver(driver: Arc<dyn FsDriver>) -> Arc<dyn FsDriver> {
    #[cfg(feature = "observability")]
    {
        let telemetry = {
            let global = mount_rs_observability::global();
            if global.is_enabled() {
                global
            } else {
                Telemetry::from_env("mount-rs-node")
            }
        };
        InstrumentedDriver::from_arc(driver, telemetry).into_arc()
    }

    #[cfg(not(feature = "observability"))]
    driver
}

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

    // Node exposes the Microsoft CRT open flags on Windows. These are not
    // the Linux values: _O_CREAT/_O_EXCL/_O_TRUNC/_O_APPEND are 0x0100,
    // 0x0400, 0x0200, and 0x0008 respectively.
    #[cfg(target_os = "windows")]
    const O_CREAT: u64 = 0x0100;
    #[cfg(target_os = "windows")]
    const O_EXCL: u64 = 0x0400;
    #[cfg(target_os = "windows")]
    const O_TRUNC: u64 = 0x0200;
    #[cfg(target_os = "windows")]
    const O_APPEND: u64 = 0x0008;

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(to_js_error(
        FsError::new(ErrorCode::Einval)
            .with_syscall("open")
            .with_path(_path)
            .with_message("numeric open flags are unsupported on this platform"),
    ));

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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

#[napi(object)]
pub struct JsBlockReconcileReport {
    pub scanned: f64,
    pub protected: f64,
    pub recent: f64,
    pub deleted: f64,
}

impl From<BlockReconcileReport> for JsBlockReconcileReport {
    fn from(value: BlockReconcileReport) -> Self {
        Self {
            scanned: value.scanned as f64,
            protected: value.protected as f64,
            recent: value.recent as f64,
            deleted: value.deleted as f64,
        }
    }
}

#[napi]
pub struct JsDirEntry {
    inner: DirEntry,
}

/// Shared ownership slot for a JavaScript filesystem and the lightweight
/// wrappers it creates (currently `mountx`). `shutdown()` must be able to
/// release the native provider even when JavaScript retains those wrappers;
/// storing the driver directly in each wrapper would keep SQLite connections
/// alive until garbage collection. The slot also makes post-shutdown calls
/// fail closed with EBADF instead of dereferencing a released provider.
struct DriverSlot {
    driver: Mutex<Option<Arc<dyn FsDriver>>>,
    capabilities: Capabilities,
    has_utimens: bool,
}

impl DriverSlot {
    fn new(driver: Arc<dyn FsDriver>) -> Self {
        let capabilities = driver.capabilities();
        let has_utimens = driver.has_utimens();
        Self {
            driver: Mutex::new(Some(driver)),
            capabilities,
            has_utimens,
        }
    }

    fn get(&self) -> CoreResult<Arc<dyn FsDriver>> {
        let driver = self
            .driver
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("driver lock poisoned"))?;
        driver.clone().ok_or_else(|| {
            FsError::new(ErrorCode::Ebadf)
                .with_syscall("filesystem")
                .with_message("filesystem is closed")
        })
    }

    fn clear(&self) -> CoreResult<()> {
        let mut driver = self
            .driver
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("driver lock poisoned"))?;
        driver.take();
        Ok(())
    }
}

impl FsDriver for DriverSlot {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn syncfs<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.syncfs().await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.stat(path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.lstat(path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.statfs(path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.readdir(path).await })
    }

    fn readdir_bounded<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        max_entries: usize,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Vec<DirEntry>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.readdir_bounded(path, max_entries).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.open(path, flags, mode).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.open_flags(path, flags, mode).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.mkdir(path, options).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.rmdir(path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.unlink(path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.rename(old_path, new_path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.link(existing_path, new_path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.symlink(target, path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.readlink(path).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.chmod(path, mode).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.chown(path, uid, gid).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.lchown(path, uid, gid).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.truncate(path, length).await })
    }

    fn has_utimens(&self) -> bool {
        self.has_utimens
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move {
            driver
                .utimens(path, atime_ns, mtime_ns, follow_symlinks)
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.utimes(path, atime_ms, mtime_ms).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.lutimes(path, atime_ms, mtime_ms).await })
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
        let driver = match self.get() {
            Ok(driver) => driver,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { driver.mknod(path, mode, dev).await })
    }
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
    /// Supported values are memory, sqlite, pglite, tidb, foundationdb, and r2
    /// (blocks only). FoundationDB requires the native feature and an
    /// explicit persisted-single-authority or shared-provider authority.
    pub kind: String,
    pub uri: Option<String>,
    pub key: Option<String>,
    pub durable: Option<bool>,
    /// FoundationDB only: persisted-single-authority or shared-provider.
    pub lease_authority: Option<String>,
    /// FoundationDB shared-provider only: the authority record key prefix.
    pub authority_prefix: Option<String>,
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
    /// Defaults to the current process uid, matching the memory driver.
    pub uid: Option<f64>,
    /// Defaults to the current process gid, matching the memory driver.
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
        self.0.load()
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
        self.0.acquire_writer(owner, ttl)
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
        self.0.renew_writer(lease, ttl)
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
        self.0.release_writer(lease)
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
        self.0.publish(expected_revision, lease, namespace)
    }

    fn flush<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        self.0.flush()
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
        self.0.put(bytes)
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
        self.0.get(id)
    }

    fn flush<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        self.0.flush()
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
        self.0.delete(id)
    }

    fn reconcile<'a, 'b, 'async_trait>(
        &'a self,
        live: &'b BTreeSet<BlockId>,
        grace: Duration,
    ) -> Pin<Box<dyn Future<Output = CoreResult<BlockReconcileReport>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.0.reconcile(live, grace)
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
        self.0.syncfs()
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
        self.0.stat(path)
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
        self.0.lstat(path)
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
        self.0.statfs(path)
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
        self.0.readdir(path)
    }

    fn readdir_bounded<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        max_entries: usize,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Vec<DirEntry>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        self.0.readdir_bounded(path, max_entries)
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
        self.0.open(path, flags, mode)
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
        self.0.open_flags(path, flags, mode)
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
        self.0.mkdir(path, options)
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
        self.0.rmdir(path)
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
        self.0.unlink(path)
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
        self.0.rename(old_path, new_path)
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
        self.0.link(existing_path, new_path)
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
        self.0.symlink(target, path)
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
        self.0.readlink(path)
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
        self.0.chmod(path, mode)
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
        self.0.chown(path, uid, gid)
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
        self.0.lchown(path, uid, gid)
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
        self.0.truncate(path, length)
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
        self.0.utimens(path, atime_ns, mtime_ns, follow_symlinks)
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
        self.0.utimes(path, atime_ms, mtime_ms)
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
        self.0.lutimes(path, atime_ms, mtime_ms)
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
        self.0.mknod(path, mode, dev)
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

#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
fn open_foundationdb_storage(
    options: &JsChunkedStoreOptions,
    role: &str,
) -> Result<FoundationDbStorage, Error> {
    let uri = required_string(&options.uri, &format!("{role}.uri"))?;
    let key = required_string(&options.key, &format!("{role}.key"))?;
    let authority = required_string(&options.lease_authority, &format!("{role}.leaseAuthority"))?;
    reject_set(&options.endpoint, &format!("{role}.endpoint"))?;
    reject_set(&options.bucket, &format!("{role}.bucket"))?;
    reject_set(&options.access_key_id, &format!("{role}.accessKeyId"))?;
    reject_set(
        &options.secret_access_key,
        &format!("{role}.secretAccessKey"),
    )?;
    let storage =
        FoundationDbStorageOptions::new(key).with_durable(options.durable.unwrap_or(false));
    let storage = match authority.as_str() {
        "persisted-single-authority" => {
            reject_set(
                &options.authority_prefix,
                &format!("{role}.authorityPrefix"),
            )?;
            storage.with_persisted_lease_oracle()
        }
        "shared-provider" => {
            let authority_prefix = required_string(
                &options.authority_prefix,
                &format!("{role}.authorityPrefix"),
            )?;
            let oracle = FoundationDbSharedLeaseOracle::connect(
                uri.as_str(),
                authority_prefix,
                FoundationDbLimits::default(),
            )
            .map_err(to_js_error)?;
            storage.with_production_lease_oracle(oracle)
        }
        _ => {
            return Err(config_error(format!(
                "{role}.leaseAuthority must be 'persisted-single-authority' or 'shared-provider'"
            )));
        }
    };
    FoundationDbStorage::connect(uri, storage).map_err(to_js_error)
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

fn validate_directory_entry_limit(value: f64) -> Result<usize, Error> {
    if !value.is_finite()
        || value.fract() != 0.0
        || value <= 0.0
        || value > MAX_SAFE_INTEGER
        || value > usize::MAX as f64
    {
        return Err(range_error(
            "maxEntries",
            "> 0, an integer, and platform-sized",
            value,
        ));
    }
    usize::try_from(value as u64)
        .map_err(|_| range_error("maxEntries", "a platform-sized integer", value))
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

fn validate_reconcile_grace(value: f64) -> Result<Duration, Error> {
    if !value.is_finite() || value.fract() != 0.0 || value <= 0.0 || value > MAX_SAFE_INTEGER {
        return Err(range_error("graceMs", "> 0 and <= 9007199254740991", value));
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
) -> Result<(Arc<dyn MetadataStore>, Option<ChunkedProviderResource>), Error> {
    if options.kind != "foundationdb" {
        reject_set(&options.lease_authority, "metadata.leaseAuthority")?;
        reject_set(&options.authority_prefix, "metadata.authorityPrefix")?;
    }
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
            Ok((
                Arc::new(store.clone()),
                Some(ChunkedProviderResource::PgliteMetadata(store)),
            ))
        }
        "tidb" => {
            let uri = required_string(&options.uri, "metadata.uri")?;
            let key = required_string(&options.key, "metadata.key")?;
            reject_set(&options.endpoint, "metadata.endpoint")?;
            reject_set(&options.bucket, "metadata.bucket")?;
            reject_set(&options.access_key_id, "metadata.accessKeyId")?;
            reject_set(&options.secret_access_key, "metadata.secretAccessKey")?;
            let storage =
                TidbStorageOptions::new(key).with_durable(options.durable.unwrap_or(false));
            let store = TidbMetadataStore::connect_with_options(&uri, storage)
                .await
                .map_err(to_js_error)?;
            Ok((
                Arc::new(store.clone()),
                Some(ChunkedProviderResource::TidbMetadata(store)),
            ))
        }
        "foundationdb" => {
            #[cfg(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            ))]
            {
                let storage = open_foundationdb_storage(options, "metadata")?;
                let store = storage.metadata();
                Ok((
                    Arc::new(store),
                    Some(ChunkedProviderResource::FoundationDb(storage)),
                ))
            }
            #[cfg(not(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            )))]
            {
                Err(config_error(
                    "FoundationDB requires the foundationdb feature on a supported native target",
                ))
            }
        }
        "r2" => Err(config_error(
            "R2 is a block-only backend; metadata must use memory, sqlite, pglite, tidb, or foundationdb",
        )),
        other => Err(config_error(format!("unknown metadata backend: {other}"))),
    }
}

async fn build_block_store(
    options: &JsChunkedStoreOptions,
) -> Result<(Arc<dyn BlockStore>, Option<ChunkedProviderResource>), Error> {
    if options.kind != "foundationdb" {
        reject_set(&options.lease_authority, "blocks.leaseAuthority")?;
        reject_set(&options.authority_prefix, "blocks.authorityPrefix")?;
    }
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
            Ok((
                Arc::new(store.clone()),
                Some(ChunkedProviderResource::PgliteBlocks(store)),
            ))
        }
        "tidb" => {
            let uri = required_string(&options.uri, "blocks.uri")?;
            let key = required_string(&options.key, "blocks.key")?;
            reject_set(&options.endpoint, "blocks.endpoint")?;
            reject_set(&options.bucket, "blocks.bucket")?;
            reject_set(&options.access_key_id, "blocks.accessKeyId")?;
            reject_set(&options.secret_access_key, "blocks.secretAccessKey")?;
            let storage =
                TidbStorageOptions::new(key).with_durable(options.durable.unwrap_or(false));
            let store = TidbBlockStore::connect_with_options(&uri, storage)
                .await
                .map_err(to_js_error)?;
            Ok((
                Arc::new(store.clone()),
                Some(ChunkedProviderResource::TidbBlocks(store)),
            ))
        }
        "foundationdb" => {
            #[cfg(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            ))]
            {
                let storage = open_foundationdb_storage(options, "blocks")?;
                let store = storage.blocks();
                Ok((
                    Arc::new(store),
                    Some(ChunkedProviderResource::FoundationDb(storage)),
                ))
            }
            #[cfg(not(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            )))]
            {
                Err(config_error(
                    "FoundationDB requires the foundationdb feature on a supported native target",
                ))
            }
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
    // Rust names Darwin and Windows targets `macos` and `windows`, while
    // Node's public platform contract (and the upstream TypeScript facade)
    // uses `darwin` and `win32`.
    let platform = match probe.platform.as_str() {
        "macos" => "darwin".to_owned(),
        "windows" => "win32".to_owned(),
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
        nfs: options.nfs_sqlite_single_host.unwrap_or(false).then(|| {
            let mut nfs = mount_rs_nfs::NfsMountOptions::sqlite_single_host();
            nfs.read_only = options.read_only.unwrap_or(false);
            if let Some(timeout) = options.unmount_timeout_ms {
                nfs.unmount_timeout = Some(Duration::from_millis(timeout as u64));
            }
            nfs
        }),
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
type ReconcileFuture = Pin<Box<dyn Future<Output = CoreResult<BlockReconcileReport>> + Send>>;
type ReconcileCallback = dyn Fn(Duration) -> ReconcileFuture + Send + Sync;

struct ShutdownAttemptState {
    result: Option<CoreResult<()>>,
    waiters: Vec<Waker>,
}

struct ShutdownAttempt {
    state: Mutex<ShutdownAttemptState>,
}

impl ShutdownAttempt {
    fn new() -> Self {
        Self {
            state: Mutex::new(ShutdownAttemptState {
                result: None,
                waiters: Vec::new(),
            }),
        }
    }

    fn finish(&self, result: CoreResult<()>) {
        let waiters = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.result = Some(result);
            std::mem::take(&mut state.waiters)
        };
        for waiter in waiters {
            waiter.wake();
        }
    }
}

struct ShutdownWait {
    attempt: Arc<ShutdownAttempt>,
}

impl Future for ShutdownWait {
    type Output = CoreResult<()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self
            .attempt
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &state.result {
            Some(result) => Poll::Ready(result.clone()),
            None => {
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
    }
}

struct ShutdownRunGuard {
    lifecycle: Arc<ShutdownState>,
    attempt: Arc<ShutdownAttempt>,
    callback: Option<Arc<ShutdownCallback>>,
    completed: bool,
}

impl ShutdownRunGuard {
    fn new(
        lifecycle: Arc<ShutdownState>,
        attempt: Arc<ShutdownAttempt>,
        callback: Option<Arc<ShutdownCallback>>,
    ) -> Self {
        Self {
            lifecycle,
            attempt,
            callback,
            completed: false,
        }
    }

    fn finish_error(&mut self, error: FsError) {
        let callback = self.callback.take();
        let mut state = self
            .lifecycle
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = ShutdownLifecycle::Open(callback);
        drop(state);
        self.completed = true;
        self.attempt.finish(Err(error));
    }

    fn finish_success(&mut self) {
        // The callback can own the last provider clone outside DriverSlot. It
        // must be dropped while the lifecycle is still Running, and before
        // the shared attempt is completed, so neither a third caller nor a
        // resolved promise can observe a still-live native provider.
        drop(self.callback.take());
        self.completed = true;
        self.attempt.finish(Ok(()));
        let mut state = self
            .lifecycle
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = ShutdownLifecycle::Closed;
    }
}

impl Drop for ShutdownRunGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }

        // The spawned task is detached from the caller's Promise. If a
        // provider callback panics after the task has entered Running, this
        // guard still publishes a bounded error and restores the callback so
        // a later shutdown call can retry rather than hanging forever.
        let callback = self.callback.take();
        let mut state = self
            .lifecycle
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = ShutdownLifecycle::Open(callback);
        drop(state);
        self.attempt.finish(Err(shutdown_panic_error()));
    }
}

fn shutdown_panic_error() -> FsError {
    FsError::new(ErrorCode::Eio)
        .with_syscall("shutdown")
        .with_message("shutdown callback panicked")
}

struct CatchUnwind<F> {
    future: Pin<Box<F>>,
}

impl<F> CatchUnwind<F> {
    fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
        }
    }
}

impl<F> Unpin for CatchUnwind<F> {}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = std::thread::Result<F::Output>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.future.as_mut().poll(context)
        })) {
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

enum ShutdownLifecycle {
    Open(Option<Arc<ShutdownCallback>>),
    Running(Arc<ShutdownAttempt>),
    Closed,
}

struct ShutdownState {
    lifecycle: Mutex<ShutdownLifecycle>,
}

impl ShutdownState {
    fn new(callback: Option<Arc<ShutdownCallback>>) -> Self {
        Self {
            lifecycle: Mutex::new(ShutdownLifecycle::Open(callback)),
        }
    }
}

struct ShutdownController {
    lifecycle: Arc<ShutdownState>,
    driver: Arc<DriverSlot>,
}

impl ShutdownController {
    fn new(driver: Arc<DriverSlot>, callback: Option<Arc<ShutdownCallback>>) -> Arc<Self> {
        Arc::new(Self {
            lifecycle: Arc::new(ShutdownState::new(callback)),
            driver,
        })
    }

    fn callback(controller: Arc<Self>) -> Arc<ShutdownCallback> {
        Arc::new(move || {
            let controller = Arc::clone(&controller);
            Box::pin(async move { controller.shutdown().await })
        })
    }

    async fn shutdown(&self) -> CoreResult<()> {
        let (attempt, start) = {
            let mut lifecycle =
                self.lifecycle.lifecycle.lock().map_err(|_| {
                    FsError::new(ErrorCode::Eio).with_message("shutdown lock poisoned")
                })?;
            match &mut *lifecycle {
                ShutdownLifecycle::Closed => (None, None),
                ShutdownLifecycle::Running(attempt) => (Some(Arc::clone(attempt)), None),
                ShutdownLifecycle::Open(callback) => {
                    let attempt = Arc::new(ShutdownAttempt::new());
                    let callback = callback.take();
                    *lifecycle = ShutdownLifecycle::Running(Arc::clone(&attempt));
                    (Some(Arc::clone(&attempt)), Some((attempt, callback)))
                }
            }
        };

        let Some(attempt) = attempt else {
            return Ok(());
        };
        if let Some((attempt, callback)) = start {
            napi::bindgen_prelude::spawn(run_shutdown(
                Arc::clone(&self.lifecycle),
                Arc::clone(&self.driver),
                attempt,
                callback,
            ));
        }
        ShutdownWait { attempt }.await
    }
}

async fn run_shutdown(
    lifecycle: Arc<ShutdownState>,
    driver: Arc<DriverSlot>,
    attempt: Arc<ShutdownAttempt>,
    callback: Option<Arc<ShutdownCallback>>,
) {
    let mut guard = ShutdownRunGuard::new(lifecycle, attempt, callback);
    let callback_result = match guard.callback.as_ref() {
        Some(callback) => match CatchUnwind::new(async { callback().await }).await {
            Ok(result) => result,
            Err(_) => Err(shutdown_panic_error()),
        },
        None => Ok(()),
    };

    match callback_result {
        Err(error) => guard.finish_error(error),
        Ok(()) => match driver.clear() {
            Ok(()) => guard.finish_success(),
            Err(error) => guard.finish_error(error),
        },
    }
}

#[napi]
pub struct Filesystem {
    driver: Arc<dyn FsDriver>,
    shutdown: Option<Arc<ShutdownCallback>>,
    reconcile: Option<Arc<ReconcileCallback>>,
}

#[napi]
impl Filesystem {
    fn from_driver(
        driver: Arc<dyn FsDriver>,
        shutdown: Option<Arc<ShutdownCallback>>,
        reconcile: Option<Arc<ReconcileCallback>>,
    ) -> Self {
        let slot = Arc::new(DriverSlot::new(instrument_driver(driver)));
        let controller = ShutdownController::new(Arc::clone(&slot), shutdown);
        Self {
            driver: slot,
            shutdown: Some(ShutdownController::callback(controller)),
            reconcile,
        }
    }

    fn driver(&self) -> napi::Result<Arc<dyn FsDriver>> {
        Ok(Arc::clone(&self.driver))
    }

    #[napi(factory)]
    pub fn memory() -> Self {
        Self::from_driver(Arc::new(MemoryFs::empty()), None, None)
    }

    #[napi(factory)]
    pub async fn sqlite(path: String) -> napi::Result<Self> {
        open_sqlite(path)
            .await
            .map(|filesystem| Self::from_driver(Arc::new(filesystem), None, None))
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
        Ok(Self::from_driver(
            Arc::new(filesystem),
            Some(shutdown),
            None,
        ))
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
            .map(|filesystem| Self::from_driver(Arc::new(filesystem), None, None))
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

    /// Complete provider shutdown and detach this filesystem's shared driver.
    /// Concurrent callers share an attempt; a failed attempt can be retried.
    /// Close independent handles and servers, and finish in-flight operations,
    /// before relying on shutdown to permit removal of backing files.
    #[napi]
    pub async fn shutdown(&self) -> napi::Result<()> {
        if let Some(shutdown) = &self.shutdown {
            shutdown().await.map_err(to_js_error)?;
        }
        Ok(())
    }

    /// Reconcile aged, unreferenced blocks for a chunked provider. The grace
    /// period is in milliseconds and must be positive. Providers without a
    /// scoped object enumerator return ENOTSUP; no cleanup is inferred from
    /// shutdown or from an unavailable provider capability.
    #[napi(js_name = "reconcileBlocks")]
    pub async fn reconcile_blocks(&self, grace_ms: f64) -> napi::Result<JsBlockReconcileReport> {
        let grace = validate_reconcile_grace(grace_ms)?;
        let callback = self.reconcile.as_ref().ok_or_else(|| {
            to_js_error(
                FsError::new(ErrorCode::Enotsup)
                    .with_syscall("reconcile blocks")
                    .with_message("filesystem does not expose chunked block reconciliation"),
            )
        })?;
        callback(grace).await.map(Into::into).map_err(to_js_error)
    }

    #[napi]
    pub async fn stat(&self, path: String) -> napi::Result<JsStats> {
        let driver = self.driver()?;
        driver
            .stat(&normalized_path(&path))
            .await
            .map(Into::into)
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn lstat(&self, path: String) -> napi::Result<JsStats> {
        let driver = self.driver()?;
        driver
            .lstat(&normalized_path(&path))
            .await
            .map(Into::into)
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn statfs(&self, path: String) -> napi::Result<JsStatsFs> {
        let driver = self.driver()?;
        driver
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
        let driver = self.driver()?;
        driver
            .readdir(&normalized_path(&path))
            .await
            .map(|entries| entries.into_iter().map(Into::into).collect())
            .map_err(to_js_error)
    }

    /// Enumerate a directory only when the provider can enforce the entry
    /// bound before returning its listing. Providers without that capability
    /// fail closed with ENOTSUP.
    #[napi(js_name = "readdirBounded")]
    pub async fn read_dir_bounded(
        &self,
        path: String,
        max_entries: f64,
    ) -> napi::Result<Vec<JsDirEntry>> {
        let max_entries = validate_directory_entry_limit(max_entries)?;
        let driver = self.driver()?;
        driver
            .readdir_bounded(&normalized_path(&path), max_entries)
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
        let driver = self.driver()?;
        driver
            .open_flags(&path, flags, mode.unwrap_or(0o666))
            .await
            .map(|inner| FileHandle { inner })
            .map_err(to_js_error)
    }

    #[napi(ts_return_type = "Promise<Uint8Array>")]
    pub async fn read_file(&self, path: String) -> napi::Result<Buffer> {
        let path = normalized_path(&path);
        let driver = self.driver()?;
        let handle = driver
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
            .driver()?
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
        let driver = self.driver()?;
        driver
            .mkdir(&normalized_path(&path), parse_mkdir_options(options)?)
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn rmdir(&self, path: String) -> napi::Result<()> {
        let driver = self.driver()?;
        driver
            .rmdir(&normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn unlink(&self, path: String) -> napi::Result<()> {
        let driver = self.driver()?;
        driver
            .unlink(&normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn rename(&self, old_path: String, new_path: String) -> napi::Result<()> {
        let driver = self.driver()?;
        driver
            .rename(&normalized_path(&old_path), &normalized_path(&new_path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn link(&self, existing_path: String, new_path: String) -> napi::Result<()> {
        let driver = self.driver()?;
        driver
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
        let driver = self.driver()?;
        driver
            .symlink(&target, &normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn readlink(&self, path: String) -> napi::Result<String> {
        let driver = self.driver()?;
        driver
            .readlink(&normalized_path(&path))
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn chmod(&self, path: String, mode: f64) -> napi::Result<()> {
        let driver = self.driver()?;
        driver
            .chmod(&normalized_path(&path), validate_u32("mode", mode)?)
            .await
            .map_err(to_js_error)
    }

    #[napi]
    pub async fn chown(&self, path: String, uid: f64, gid: f64) -> napi::Result<()> {
        let driver = self.driver()?;
        driver
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
        let driver = self.driver()?;
        driver
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
        let driver = self.driver()?;
        driver
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
        let driver = self.driver()?;
        driver
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
        let driver = self.driver()?;
        driver
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
        let driver = self.driver()?;
        driver
            .mknod(
                &normalized_path(&path),
                validate_u32("mode", mode)?,
                validate_dev(dev)?,
            )
            .await
            .map_err(to_js_error)
    }
}

#[derive(Clone)]
enum ChunkedProviderResource {
    PgliteMetadata(PgliteMetadataStore),
    PgliteBlocks(PgliteBlockStore),
    TidbMetadata(TidbMetadataStore),
    TidbBlocks(TidbBlockStore),
    #[cfg(all(
        feature = "foundationdb",
        any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
        )
    ))]
    FoundationDb(FoundationDbStorage),
}

impl ChunkedProviderResource {
    async fn close(&self) -> CoreResult<()> {
        match self {
            Self::PgliteMetadata(store) => store.close().await,
            Self::PgliteBlocks(store) => store.close().await,
            Self::TidbMetadata(store) => store.close().await,
            Self::TidbBlocks(store) => store.close().await,
            #[cfg(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            ))]
            Self::FoundationDb(storage) => {
                let _ = storage;
                Ok(())
            }
        }
    }
}

async fn close_chunked_resources(resources: Vec<ChunkedProviderResource>) -> CoreResult<()> {
    let mut first_error = None;
    for resource in resources {
        if let Err(error) = resource.close().await
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Construct a filesystem over independently selected metadata and immutable
/// block providers. The returned driver's `shutdown()` releases its writer
/// lease; callers should invoke it when the driver is no longer in use.
async fn shutdown_chunked_filesystem(
    filesystem: ChunkedFs<DynMetadataStore, DynBlockStore>,
    resources: Vec<ChunkedProviderResource>,
) -> CoreResult<()> {
    // Always attempt provider teardown even if lease release reports an
    // error. A stale lease must not keep the PostgreSQL-wire clients alive
    // until JavaScript garbage-collects the retained Filesystem object.
    let filesystem_result = filesystem.shutdown().await;
    let resources_result = close_chunked_resources(resources).await;
    filesystem_result.and(resources_result)
}

#[napi]
pub async fn create_chunked_driver(options: JsChunkedOptions) -> napi::Result<Filesystem> {
    let owner = chunked_owner(options.owner)?;
    let chunk_size = validate_chunk_size(options.chunk_size)?;
    let ttl = validate_ttl(options.ttl_ms)?;
    let (process_uid, process_gid) = memory_factory::native_process_identity();
    let uid = optional_u32("uid", options.uid, process_uid)?;
    let gid = optional_u32("gid", options.gid, process_gid)?;
    let umask = optional_u32("umask", options.umask, 0)?;
    let root_mode = optional_u32("rootMode", options.root_mode, 0o755)?;
    let chunk_options = ChunkedOptions::fixed(owner, chunk_size)
        .map_err(to_js_error)?
        .with_lease_ttl(ttl)
        .with_identity(uid, gid, umask)
        .with_root_mode(root_mode);

    let (metadata_store, metadata_resource) = build_metadata_store(&options.metadata).await?;
    let (block_store, block_resource) = match build_block_store(&options.blocks).await {
        Ok(opened) => opened,
        Err(error) => {
            if let Some(resource) = metadata_resource {
                let _ = resource.close().await;
            }
            return Err(error);
        }
    };
    let mut resources = Vec::with_capacity(2);
    if let Some(resource) = metadata_resource {
        resources.push(resource);
    }
    if let Some(resource) = block_resource {
        resources.push(resource);
    }
    let metadata = DynMetadataStore(metadata_store);
    let blocks = DynBlockStore(block_store);
    let cleanup_resources = resources.clone();
    let filesystem = match ChunkedFs::open(metadata, blocks, chunk_options).await {
        Ok(filesystem) => filesystem,
        Err(error) => {
            let _ = close_chunked_resources(cleanup_resources).await;
            return Err(to_js_error(error));
        }
    };
    let shutdown_filesystem = filesystem.clone();
    let shutdown_resources = resources;
    let shutdown: Arc<ShutdownCallback> = Arc::new(move || {
        let filesystem = shutdown_filesystem.clone();
        let resources = shutdown_resources.clone();
        Box::pin(async move { shutdown_chunked_filesystem(filesystem, resources).await })
    });
    let reconcile_filesystem = filesystem.clone();
    let reconcile: Arc<ReconcileCallback> = Arc::new(move |grace| {
        let filesystem = reconcile_filesystem.clone();
        Box::pin(async move { filesystem.reconcile_blocks(grace).await })
    });
    Ok(Filesystem::from_driver(
        Arc::new(filesystem),
        Some(shutdown),
        Some(reconcile),
    ))
}

/// Create the rooted host-filesystem driver used by the upstream
/// `createNodeFsDriver` API. Construction is synchronous; host I/O remains
/// asynchronous inside the Rust driver and the root is resolved lexically.
#[napi]
pub fn create_node_fs_driver(root: String, options: Option<JsNodeFsOptions>) -> Filesystem {
    let read_only = options
        .and_then(|options| options.read_only)
        .unwrap_or(false);
    Filesystem::from_driver(
        Arc::new(HostFs::with_options(root, HostFsOptions { read_only })),
        None,
        None,
    )
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
    let mount_driver = driver.driver()?;
    let mounted = mount_rs_auto::mount(MountDriver(mount_driver), mountpoint, options)
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
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Condvar, Mutex};
    use std::task::{Context, Poll, Wake, Waker};

    struct NoopWaker;

    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}

        fn wake_by_ref(self: &Arc<Self>) {}
    }

    struct CompletionWaker {
        lifecycle: Arc<ShutdownState>,
        observed_running: Arc<AtomicBool>,
    }

    impl CompletionWaker {
        fn observe(&self) {
            if matches!(
                &*self
                    .lifecycle
                    .lifecycle
                    .lock()
                    .expect("shutdown lifecycle lock"),
                ShutdownLifecycle::Running(_)
            ) {
                self.observed_running.store(true, AtomicOrdering::SeqCst);
            }
        }
    }

    impl Wake for CompletionWaker {
        fn wake(self: Arc<Self>) {
            self.observe();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.observe();
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[derive(Default)]
    struct DropGateState {
        started: bool,
        released: bool,
        finished: bool,
    }

    struct DropGate {
        state: Mutex<DropGateState>,
        condition: Condvar,
    }

    impl DropGate {
        fn new() -> Self {
            Self {
                state: Mutex::new(DropGateState::default()),
                condition: Condvar::new(),
            }
        }

        fn wait_started(&self) {
            let mut state = self.state.lock().expect("drop gate lock");
            while !state.started {
                state = self.condition.wait(state).expect("drop gate wait");
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().expect("drop gate lock");
            state.released = true;
            self.condition.notify_all();
        }

        fn wait_finished(&self) {
            let mut state = self.state.lock().expect("drop gate lock");
            while !state.finished {
                state = self.condition.wait(state).expect("drop gate wait");
            }
        }
    }

    struct CallbackDropProbe(Arc<DropGate>);

    impl Drop for CallbackDropProbe {
        fn drop(&mut self) {
            let mut state = self.0.state.lock().expect("drop gate lock");
            state.started = true;
            self.0.condition.notify_all();
            while !state.released {
                state = self.0.condition.wait(state).expect("drop gate wait");
            }
            state.finished = true;
            self.0.condition.notify_all();
        }
    }

    fn running_lifecycle(
        attempt: &Arc<ShutdownAttempt>,
        callback: Option<Arc<ShutdownCallback>>,
    ) -> Arc<ShutdownState> {
        let lifecycle = Arc::new(ShutdownState::new(callback));
        *lifecycle.lifecycle.lock().expect("shutdown lifecycle lock") =
            ShutdownLifecycle::Running(Arc::clone(attempt));
        lifecycle
    }

    #[test]
    fn numeric_flags_use_the_host_namespace() {
        let flags = decode_numeric_flags(0.0, "/file").expect("O_RDONLY");
        assert_eq!(flags, OpenFlags::READ_ONLY);

        #[cfg(target_os = "linux")]
        let bits = 1 | 0o100 | 0o1000 | 0o2000 | 0o200;
        #[cfg(target_os = "macos")]
        let bits = 1 | 0x0200 | 0x0400 | 0x0008 | 0x0800;
        #[cfg(target_os = "windows")]
        let bits = 1 | 0x0100 | 0x0200 | 0x0008 | 0x0400;
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
    fn bounded_directory_entry_limit_rejects_unsafe_numbers() {
        assert_eq!(validate_directory_entry_limit(4.0).unwrap(), 4);
        assert!(validate_directory_entry_limit(0.0).is_err());
        assert!(validate_directory_entry_limit(-1.0).is_err());
        assert!(validate_directory_entry_limit(1.5).is_err());
        assert!(validate_directory_entry_limit(f64::NAN).is_err());
        assert!(validate_directory_entry_limit(MAX_SAFE_INTEGER + 1.0).is_err());
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
        let windows = auto_probe(mount_rs_auto::probe_transports_for("windows"));
        assert_eq!(windows.platform, "win32");

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
            read_only: Some(true),
            unmount_timeout_ms: Some(1234.0),
            nfs_sqlite_single_host: Some(true),
        }))
        .unwrap();
        let nfs = options.nfs.unwrap();
        assert!(nfs.hard);
        assert!(nfs.read_only);
        assert_eq!(nfs.unmount_timeout, Some(Duration::from_millis(1234)));
        let native =
            mount_rs_nfs::native::nfs_mount_options(2049, &nfs, mount_rs_nfs::NfsPlatform::Macos)
                .unwrap();
        assert!(native.split(',').any(|value| value == "locallocks"));
        assert!(native.split(',').any(|value| value == "hard"));
    }

    #[test]
    fn shutdown_wait_deduplicates_wakers_after_cancellation() {
        let attempt = Arc::new(ShutdownAttempt::new());
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut wait = Box::pin(ShutdownWait {
            attempt: Arc::clone(&attempt),
        });

        for _ in 0..32 {
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        }
        assert_eq!(
            attempt
                .state
                .lock()
                .expect("shutdown attempt lock")
                .waiters
                .len(),
            1
        );

        // Dropping the caller's wait must not cancel the detached shutdown
        // attempt or make its completion path panic on the stale waker.
        drop(wait);
        attempt.finish(Ok(()));
        assert!(matches!(
            attempt.state.lock().expect("shutdown attempt lock").result,
            Some(Ok(()))
        ));
    }

    #[test]
    fn shutdown_keeps_running_until_provider_callback_reference_is_dropped() {
        let gate = Arc::new(DropGate::new());
        let probe = CallbackDropProbe(Arc::clone(&gate));
        let callback: Arc<ShutdownCallback> = Arc::new(move || {
            let _ = &probe;
            Box::pin(async { Ok(()) })
        });
        let attempt = Arc::new(ShutdownAttempt::new());
        let lifecycle = running_lifecycle(&attempt, None);
        let driver = Arc::new(DriverSlot::new(Arc::new(MemoryFs::empty())));

        let run_lifecycle = Arc::clone(&lifecycle);
        let run_driver = Arc::clone(&driver);
        let run_attempt = Arc::clone(&attempt);
        let run = std::thread::spawn(move || {
            block_on(run_shutdown(
                run_lifecycle,
                run_driver,
                run_attempt,
                Some(callback),
            ));
        });

        gate.wait_started();
        let running_before_release = {
            let state = lifecycle.lifecycle.lock().expect("shutdown lifecycle lock");
            matches!(&*state, ShutdownLifecycle::Running(_))
        };

        // Two callers can join the same attempt; while the provider-owned
        // callback reference is blocked in Drop, neither may observe Closed
        // or resolve a third shutdown call early.
        let observed_running = Arc::new(AtomicBool::new(false));
        let waker = Waker::from(Arc::new(CompletionWaker {
            lifecycle: Arc::clone(&lifecycle),
            observed_running: Arc::clone(&observed_running),
        }));
        let mut context = Context::from_waker(&waker);
        let mut first = Box::pin(ShutdownWait {
            attempt: Arc::clone(&attempt),
        });
        let mut second = Box::pin(ShutdownWait {
            attempt: Arc::clone(&attempt),
        });
        assert!(matches!(first.as_mut().poll(&mut context), Poll::Pending));
        assert!(matches!(second.as_mut().poll(&mut context), Poll::Pending));

        gate.release();
        run.join().expect("shutdown worker");
        gate.wait_finished();

        assert!(running_before_release);
        assert!(observed_running.load(AtomicOrdering::SeqCst));
        assert!(matches!(
            &*lifecycle.lifecycle.lock().expect("shutdown lifecycle lock"),
            ShutdownLifecycle::Closed
        ));
        assert!(matches!(
            attempt.state.lock().expect("shutdown attempt lock").result,
            Some(Ok(()))
        ));
        assert_eq!(block_on(first).expect("first shutdown waiter"), ());
        assert_eq!(block_on(second).expect("second shutdown waiter"), ());
    }

    #[test]
    fn shutdown_callback_panic_is_bounded_and_retryable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback: Arc<ShutdownCallback> = Arc::new({
            let calls = Arc::clone(&calls);
            move || {
                let call = calls.fetch_add(1, AtomicOrdering::SeqCst);
                Box::pin(async move {
                    if call == 0 {
                        panic!("test shutdown panic");
                    }
                    Ok(())
                })
            }
        });
        let driver = Arc::new(DriverSlot::new(Arc::new(MemoryFs::empty())));
        let attempt = Arc::new(ShutdownAttempt::new());
        let lifecycle = Arc::new(ShutdownState::new(Some(callback)));
        let callback = {
            let mut state = lifecycle.lifecycle.lock().expect("shutdown lifecycle lock");
            match &mut *state {
                ShutdownLifecycle::Open(callback) => callback.take(),
                _ => panic!("expected open shutdown state"),
            }
        };
        *lifecycle.lifecycle.lock().expect("shutdown lifecycle lock") =
            ShutdownLifecycle::Running(Arc::clone(&attempt));

        block_on(run_shutdown(
            Arc::clone(&lifecycle),
            Arc::clone(&driver),
            Arc::clone(&attempt),
            callback,
        ));
        assert!(matches!(
            attempt
                .state
                .lock()
                .expect("shutdown attempt lock")
                .result,
            Some(Err(ref error)) if error.code == ErrorCode::Eio
        ));
        assert!(matches!(
            &*lifecycle.lifecycle.lock().expect("shutdown lifecycle lock"),
            ShutdownLifecycle::Open(Some(_))
        ));

        let retry_attempt = Arc::new(ShutdownAttempt::new());
        let retry_callback = {
            let mut state = lifecycle.lifecycle.lock().expect("shutdown lifecycle lock");
            let callback = match &mut *state {
                ShutdownLifecycle::Open(callback) => callback.take(),
                _ => panic!("expected retryable shutdown state"),
            };
            *state = ShutdownLifecycle::Running(Arc::clone(&retry_attempt));
            callback
        };
        block_on(run_shutdown(
            lifecycle,
            driver,
            retry_attempt.clone(),
            retry_callback,
        ));
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
        assert!(matches!(
            retry_attempt
                .state
                .lock()
                .expect("shutdown attempt lock")
                .result,
            Some(Ok(()))
        ));
    }
}
