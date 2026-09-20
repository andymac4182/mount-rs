use std::sync::Arc;

use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle as CoreFileHandle, FsDriver, FsError, MemoryFs,
    MkdirOptions, OpenFlags, Stats, StatsFs,
};
use mount_rs_pglite::connect_pglite;
use mount_rs_r2::{R2Config, open_r2};
use mount_rs_sqlite::open_sqlite;
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
#[napi]
pub struct Filesystem {
    driver: Arc<dyn FsDriver>,
}

#[napi]
impl Filesystem {
    #[napi(factory)]
    pub fn memory() -> Self {
        Self {
            driver: Arc::new(MemoryFs::empty()),
        }
    }

    #[napi(factory)]
    pub async fn sqlite(path: String) -> napi::Result<Self> {
        open_sqlite(path)
            .await
            .map(|filesystem| Self {
                driver: Arc::new(filesystem),
            })
            .map_err(to_js_error)
    }

    #[napi(factory)]
    pub async fn pglite(connection_string: String) -> napi::Result<Self> {
        connect_pglite(&connection_string)
            .await
            .map(|filesystem| Self {
                driver: Arc::new(filesystem),
            })
            .map_err(to_js_error)
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

    /// The core driver contract returns only success for mkdir. In particular,
    /// it cannot report the first component created by a recursive operation.
    /// Do not derive that result with a stat/preflight pass: another caller can
    /// create a component between the observation and mkdir. An atomic core
    /// `mkdir` result such as `Result<Option<String>>` is required for exact
    /// upstream semantics.
    #[napi]
    pub async fn mkdir(
        &self,
        path: String,
        options: Option<Either<bool, JsMkdirOptions>>,
    ) -> napi::Result<()> {
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
}
