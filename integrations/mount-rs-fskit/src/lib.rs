//! Bounded control protocol and `FsDriver` worker for the macOS FSKit bridge.
//!
//! XPC transports one `Frame` as a `Data` value. The persistent worker keeps a
//! provider-backed handle table alive for the service session. The bundled
//! C-ABI constructor accepts memory, single-database SQLite, and split SQLite
//! metadata/block providers; a containing host still owns the service paths
//! and signed FSKit packaging.

#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::convert::TryFrom;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::{
    Capabilities, DirEntry, FileHandle, FsDriver, FsError, MemoryFs, MkdirOptions, Stats, StatsFs,
};
use mount_rs_host::{HostFs, HostFsOptions};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore, open_sqlite};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

pub const MAGIC: [u8; 4] = *b"MRFS";
pub const PROTOCOL_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 20;
pub const MAX_BODY_LEN: usize = 1024 * 1024;

const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;

/// Backend selection passed to the native worker constructor.
///
/// `splitSqlite` deliberately takes two paths. The metadata database contains
/// the fenced namespace and writer lease, while the block database contains
/// immutable file bytes. They are never silently collapsed into one SQLite
/// file; this keeps the provider composition identical to the existing CLI
/// split-store configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "backend", rename_all = "camelCase")]
pub enum WorkerBackendConfig {
    Memory {
        #[serde(default, rename = "readOnly")]
        read_only: bool,
    },
    Host {
        root: PathBuf,
        #[serde(default, rename = "readOnly")]
        read_only: bool,
    },
    Sqlite {
        #[serde(rename = "databasePath")]
        database_path: PathBuf,
        #[serde(default, rename = "readOnly")]
        read_only: bool,
    },
    SplitSqlite {
        #[serde(rename = "metadataPath")]
        metadata_path: PathBuf,
        #[serde(rename = "blocksPath")]
        blocks_path: PathBuf,
        #[serde(default = "default_chunk_size", rename = "chunkSize")]
        chunk_size: usize,
        #[serde(default, rename = "readOnly")]
        read_only: bool,
    },
}

fn default_chunk_size() -> usize {
    DEFAULT_CHUNK_SIZE
}

impl WorkerBackendConfig {
    pub fn memory(read_only: bool) -> Self {
        Self::Memory { read_only }
    }
}

const FLAG_MASK: u8 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageKind {
    Hello = 1,
    Shutdown = 2,
    Operation = 3,
    Reply = 0x80,
    Error = 0x81,
}

impl TryFrom<u8> for MessageKind {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, DecodeError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Shutdown),
            3 => Ok(Self::Operation),
            0x80 => Ok(Self::Reply),
            0x81 => Ok(Self::Error),
            other => Err(DecodeError::UnknownKind(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    BodyTooLarge { len: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    TooShort { actual: usize },
    InvalidMagic,
    UnsupportedVersion { version: u16 },
    UnsupportedFlags { flags: u8 },
    UnknownKind(u8),
    BodyTooLarge { len: usize },
    LengthMismatch { expected: usize, actual: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: MessageKind,
    pub request_id: u64,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(
        kind: MessageKind,
        request_id: u64,
        body: impl Into<Vec<u8>>,
    ) -> Result<Self, FrameError> {
        let body = body.into();
        if body.len() > MAX_BODY_LEN {
            return Err(FrameError::BodyTooLarge { len: body.len() });
        }
        Ok(Self {
            kind,
            request_id,
            body,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        if self.body.len() > MAX_BODY_LEN {
            return Err(FrameError::BodyTooLarge {
                len: self.body.len(),
            });
        }

        let mut encoded = Vec::with_capacity(HEADER_LEN + self.body.len());
        encoded.extend_from_slice(&MAGIC);
        encoded.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        encoded.push(self.kind as u8);
        encoded.push(FLAG_MASK);
        encoded.extend_from_slice(&self.request_id.to_le_bytes());
        encoded.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        encoded.extend_from_slice(&self.body);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, DecodeError> {
        if encoded.len() < HEADER_LEN {
            return Err(DecodeError::TooShort {
                actual: encoded.len(),
            });
        }
        if encoded[..MAGIC.len()] != MAGIC {
            return Err(DecodeError::InvalidMagic);
        }

        let version = u16::from_le_bytes([encoded[4], encoded[5]]);
        if version != PROTOCOL_VERSION {
            return Err(DecodeError::UnsupportedVersion { version });
        }

        let flags = encoded[7];
        if flags != FLAG_MASK {
            return Err(DecodeError::UnsupportedFlags { flags });
        }

        let kind = MessageKind::try_from(encoded[6])?;
        let request_id = u64::from_le_bytes(
            encoded[8..16]
                .try_into()
                .expect("the fixed header contains eight request-id bytes"),
        );
        let body_len = u32::from_le_bytes(
            encoded[16..20]
                .try_into()
                .expect("the fixed header contains four body-length bytes"),
        ) as usize;
        if body_len > MAX_BODY_LEN {
            return Err(DecodeError::BodyTooLarge { len: body_len });
        }

        let expected = HEADER_LEN + body_len;
        if encoded.len() != expected {
            return Err(DecodeError::LengthMismatch {
                expected,
                actual: encoded.len(),
            });
        }

        Ok(Self {
            kind,
            request_id,
            body: encoded[HEADER_LEN..].to_vec(),
        })
    }

    fn reply(request_id: u64, body: impl Into<Vec<u8>>) -> Result<Self, FrameError> {
        Self::new(MessageKind::Reply, request_id, body)
    }

    fn error(request_id: u64, code: ErrorCode) -> Result<Self, FrameError> {
        Self::new(MessageKind::Error, request_id, code.as_u16().to_le_bytes())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ErrorCode {
    UnsupportedOperation = 1,
    ShuttingDown = 2,
    InvalidRequestKind = 3,
}

impl ErrorCode {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompileOnlyDelegate {
    shutting_down: bool,
}

impl CompileOnlyDelegate {
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    /// Dispatch a decoded frame without claiming any filesystem operation.
    ///
    /// `Hello` returns the protocol version, `Shutdown` is an idempotent
    /// control reply, and `Operation` always returns a typed protocol error.
    /// Reply and Error frames are not valid requests and are rejected with a
    /// typed protocol error rather than being echoed or fabricated.
    pub fn dispatch(&mut self, request: Frame) -> Result<Frame, FrameError> {
        match request.kind {
            MessageKind::Hello => Frame::reply(request.request_id, PROTOCOL_VERSION.to_le_bytes()),
            MessageKind::Shutdown => {
                self.shutting_down = true;
                Frame::reply(request.request_id, [])
            }
            MessageKind::Operation if self.shutting_down => {
                Frame::error(request.request_id, ErrorCode::ShuttingDown)
            }
            MessageKind::Operation => {
                Frame::error(request.request_id, ErrorCode::UnsupportedOperation)
            }
            MessageKind::Reply | MessageKind::Error => {
                Frame::error(request.request_id, ErrorCode::InvalidRequestKind)
            }
        }
    }

    pub fn dispatch_encoded(&mut self, request: &[u8]) -> Result<Vec<u8>, DispatchError> {
        let request = Frame::decode(request).map_err(DispatchError::MalformedRequest)?;
        let response = self.dispatch(request).map_err(DispatchError::Encode)?;
        response.encode().map_err(DispatchError::Encode)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchError {
    MalformedRequest(DecodeError),
    Encode(FrameError),
}

pub const STATUS_OK: i32 = 0;
pub const STATUS_INVALID_ARGUMENT: i32 = -1;
pub const STATUS_MALFORMED_REQUEST: i32 = -2;
pub const STATUS_RESPONSE_TOO_SMALL: i32 = -3;
pub const STATUS_INTERNAL_ERROR: i32 = -4;

/// The largest data payload accepted by a single worker read or write.
///
/// The outer frame is limited to one MiB. Keeping a margin for the JSON
/// envelope means a successful read can always be encoded as a reply without
/// turning a valid filesystem operation into an unrepresentable response.
/// Keep JSON byte-array requests and replies comfortably below the outer
/// frame limit. The Swift client transparently splits larger FSKit reads and
/// writes into these fixed-size chunks.
pub const MAX_OPERATION_DATA_LEN: usize = 128 * 1024;

#[unsafe(no_mangle)]
pub extern "C" fn mount_rs_fskit_protocol_version() -> u16 {
    PROTOCOL_VERSION
}

/// Dispatch one frame through a fresh protocol-only delegate.
///
/// The caller owns all buffers. The pointer/length combinations are checked
/// before any slice is formed. A stateful worker should retain a delegate and
/// call [`CompileOnlyDelegate::dispatch_encoded`] for each request instead;
/// this C ABI is intentionally a small, testable bridge for a future native
/// embedding. The persistent worker ABI below owns the production-shaped
/// handle lifecycle used by the XPC service.
///
/// # Safety
///
/// The caller must provide a valid readable `request_ptr` region of
/// `request_len` bytes when `request_len` is nonzero, a valid writable
/// `response_ptr` region of `response_capacity` bytes when the capacity is
/// nonzero, and a valid writable `response_len` pointer. The regions must not
/// overlap in a way that violates the normal `copy_nonoverlapping` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mount_rs_fskit_dispatch(
    request_ptr: *const u8,
    request_len: usize,
    response_ptr: *mut u8,
    response_capacity: usize,
    response_len: *mut usize,
) -> i32 {
    if response_len.is_null()
        || (request_len != 0 && request_ptr.is_null())
        || (response_capacity != 0 && response_ptr.is_null())
    {
        return STATUS_INVALID_ARGUMENT;
    }

    let request = if request_len == 0 {
        &[]
    } else {
        // SAFETY: the caller contract requires a valid readable region of
        // request_len bytes whenever request_ptr is non-null.
        unsafe { std::slice::from_raw_parts(request_ptr, request_len) }
    };

    let mut delegate = CompileOnlyDelegate::default();
    let encoded = match delegate.dispatch_encoded(request) {
        Ok(encoded) => encoded,
        Err(DispatchError::MalformedRequest(_)) => return STATUS_MALFORMED_REQUEST,
        Err(DispatchError::Encode(_)) => return STATUS_INTERNAL_ERROR,
    };

    // SAFETY: response_len was checked non-null above and is an out-parameter
    // owned by the caller under the C ABI contract.
    unsafe { *response_len = encoded.len() };
    if response_capacity < encoded.len() {
        return STATUS_RESPONSE_TOO_SMALL;
    }

    if !encoded.is_empty() {
        // SAFETY: response_ptr is non-null when response_capacity is nonzero,
        // and the capacity check above proves the destination is large enough.
        unsafe {
            std::ptr::copy_nonoverlapping(encoded.as_ptr(), response_ptr, encoded.len());
        }
    }
    STATUS_OK
}

/// JSON request body carried by an `Operation` frame.
///
/// The enum deliberately names the Rust driver's operations instead of
/// reimplementing filesystem behavior in Swift. The Swift FSKit adapter only
/// translates Apple objects into this schema and translates the typed result
/// back. Paths are normalized at the worker boundary before they reach a
/// provider, matching the existing `Loopback` contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "camelCase")]
pub enum OperationRequest {
    Handshake {
        #[serde(rename = "readOnly")]
        read_only: bool,
    },
    Capabilities,
    Sync,
    Stat {
        path: String,
    },
    Lstat {
        path: String,
    },
    Statfs {
        path: String,
    },
    Readdir {
        path: String,
    },
    Open {
        path: String,
        flags: String,
        mode: u32,
    },
    Read {
        handle: u64,
        offset: u64,
        length: u64,
    },
    Write {
        handle: u64,
        offset: u64,
        data: Vec<u8>,
    },
    HandleStat {
        handle: u64,
    },
    TruncateHandle {
        handle: u64,
        length: u64,
    },
    SyncHandle {
        handle: u64,
    },
    DatasyncHandle {
        handle: u64,
    },
    Close {
        handle: u64,
    },
    Mkdir {
        path: String,
        recursive: bool,
        mode: Option<u32>,
    },
    Rmdir {
        path: String,
    },
    Unlink {
        path: String,
    },
    Rename {
        #[serde(rename = "oldPath")]
        old_path: String,
        #[serde(rename = "newPath")]
        new_path: String,
    },
    Link {
        #[serde(rename = "existingPath")]
        existing_path: String,
        #[serde(rename = "newPath")]
        new_path: String,
    },
    Symlink {
        target: String,
        path: String,
    },
    Readlink {
        path: String,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    Chown {
        path: String,
        uid: u32,
        gid: u32,
    },
    Lchown {
        path: String,
        uid: u32,
        gid: u32,
    },
    Truncate {
        path: String,
        length: u64,
    },
    Utimes {
        path: String,
        #[serde(rename = "atimeMs")]
        atime_ms: i64,
        #[serde(rename = "mtimeMs")]
        mtime_ms: i64,
    },
    Lutimes {
        path: String,
        #[serde(rename = "atimeMs")]
        atime_ms: i64,
        #[serde(rename = "mtimeMs")]
        mtime_ms: i64,
    },
    Mknod {
        path: String,
        mode: u32,
        dev: u64,
    },
}

impl OperationRequest {
    fn requires_write(&self) -> bool {
        match self {
            Self::Open { path, flags, .. } => mount_rs_core::OpenFlags::parse(flags, path)
                .map(|flags| flags.write || flags.create || flags.truncate || flags.append)
                .unwrap_or(true),
            Self::Write { .. }
            | Self::TruncateHandle { .. }
            | Self::Mkdir { .. }
            | Self::Rmdir { .. }
            | Self::Unlink { .. }
            | Self::Rename { .. }
            | Self::Link { .. }
            | Self::Symlink { .. }
            | Self::Chmod { .. }
            | Self::Chown { .. }
            | Self::Lchown { .. }
            | Self::Truncate { .. }
            | Self::Utimes { .. }
            | Self::Lutimes { .. }
            | Self::Mknod { .. } => true,
            Self::Handshake { .. }
            | Self::Capabilities
            | Self::Sync
            | Self::Stat { .. }
            | Self::Lstat { .. }
            | Self::Statfs { .. }
            | Self::Readdir { .. }
            | Self::Read { .. }
            | Self::Readlink { .. }
            | Self::HandleStat { .. }
            | Self::SyncHandle { .. }
            | Self::DatasyncHandle { .. }
            | Self::Close { .. } => false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkerDirEntry {
    pub entry: DirEntry,
    pub stats: Stats,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "response", rename_all = "camelCase")]
pub enum OperationResponse {
    #[serde(rename = "ok")]
    Ok,
    Capabilities {
        capabilities: Capabilities,
    },
    Stats {
        stats: Stats,
    },
    Statfs {
        stats: StatsFs,
    },
    Entries {
        entries: Vec<WorkerDirEntry>,
    },
    Open {
        handle: u64,
        stats: Stats,
    },
    Read {
        data: Vec<u8>,
        count: usize,
    },
    Write {
        count: usize,
    },
    Text {
        value: String,
    },
}

/// A stable, JSON-serializable description of an error crossing the Swift
/// boundary. In particular, the POSIX errno is preserved; Swift must not
/// infer an error from a missing reply or manufacture a generic I/O error.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerError {
    pub errno: i32,
    pub code: String,
    pub message: String,
    pub syscall: Option<String>,
    pub path: Option<String>,
    pub dest: Option<String>,
}

impl WorkerError {
    fn from_fs_error(error: FsError) -> Self {
        Self {
            errno: error.code.errno(),
            code: error.code.as_str().to_owned(),
            message: error.to_string(),
            syscall: error.syscall,
            path: error.path,
            dest: error.dest,
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        let error = FsError::new(mount_rs_core::ErrorCode::Eproto)
            .with_syscall("fskit-worker")
            .with_message(message);
        Self::from_fs_error(error)
    }

    fn encode(&self) -> Result<Vec<u8>, FrameError> {
        serde_json::to_vec(self)
            .map_err(|_| FrameError::BodyTooLarge {
                len: MAX_BODY_LEN + 1,
            })
            .and_then(|body| {
                if body.len() > MAX_BODY_LEN {
                    Err(FrameError::BodyTooLarge { len: body.len() })
                } else {
                    Ok(body)
                }
            })
    }
}

/// A typed Rust-driver worker used by the FSKit volume and the XPC service.
///
/// The worker owns provider handles and is the only layer that invokes
/// [`FsDriver`]. Swift does not get a second filesystem implementation: it
/// retains only opaque item paths and these worker-issued handle IDs.
type DriverShutdown =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = mount_rs_core::Result<()>> + Send>> + Send + Sync>;

pub struct DriverWorker {
    driver: Arc<dyn FsDriver>,
    handles: Mutex<HashMap<u64, Arc<dyn FileHandle>>>,
    next_handle: AtomicU64,
    read_only: AtomicBool,
    configured_read_only: bool,
    shutting_down: AtomicBool,
    shutdown_hook: Option<DriverShutdown>,
}

impl DriverWorker {
    pub fn new(driver: Arc<dyn FsDriver>, read_only: bool) -> Self {
        Self {
            driver,
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            read_only: AtomicBool::new(read_only),
            configured_read_only: read_only,
            shutting_down: AtomicBool::new(false),
            shutdown_hook: None,
        }
    }

    fn with_shutdown_hook(
        driver: Arc<dyn FsDriver>,
        read_only: bool,
        shutdown_hook: DriverShutdown,
    ) -> Self {
        Self {
            driver,
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            read_only: AtomicBool::new(read_only),
            configured_read_only: read_only,
            shutting_down: AtomicBool::new(false),
            shutdown_hook: Some(shutdown_hook),
        }
    }

    pub fn memory(read_only: bool) -> Self {
        Self::new(Arc::new(MemoryFs::empty()), read_only)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    pub fn capabilities(&self) -> Capabilities {
        let mut capabilities = self.driver.capabilities();
        capabilities.read_only |= self.read_only.load(Ordering::Acquire);
        capabilities
    }

    /// Dispatch one already-decoded frame. A malformed operation body gets a
    /// typed protocol error frame; a malformed outer frame remains a transport
    /// error and is returned by [`Self::dispatch_encoded`].
    pub async fn dispatch(&self, request: Frame) -> Result<Frame, FrameError> {
        match request.kind {
            MessageKind::Hello => Frame::reply(request.request_id, PROTOCOL_VERSION.to_le_bytes()),
            MessageKind::Shutdown => match self.shutdown().await {
                Ok(()) => {
                    self.shutting_down.store(true, Ordering::Release);
                    Frame::reply(request.request_id, [])
                }
                Err(error) => {
                    self.worker_error_frame(request.request_id, WorkerError::from_fs_error(error))
                }
            },
            MessageKind::Operation if self.is_shutting_down() => {
                Frame::error(request.request_id, ErrorCode::ShuttingDown)
            }
            MessageKind::Operation => {
                let operation = match serde_json::from_slice::<OperationRequest>(&request.body) {
                    Ok(operation) => operation,
                    Err(error) => {
                        return self.worker_error_frame(
                            request.request_id,
                            WorkerError::protocol(format!("invalid operation request: {error}")),
                        );
                    }
                };
                if let OperationRequest::Handshake { read_only } = &operation {
                    self.read_only
                        .store(*read_only || self.configured_read_only, Ordering::Release);
                }
                if self.read_only.load(Ordering::Acquire) && operation.requires_write() {
                    return self.worker_error_frame(
                        request.request_id,
                        WorkerError::from_fs_error(
                            FsError::new(mount_rs_core::ErrorCode::Erofs)
                                .with_syscall("fskit-worker"),
                        ),
                    );
                }
                match self.dispatch_operation(operation).await {
                    Ok(response) => {
                        let body = serde_json::to_vec(&response).map_err(|_| {
                            FrameError::BodyTooLarge {
                                len: MAX_BODY_LEN + 1,
                            }
                        })?;
                        Frame::reply(request.request_id, body)
                    }
                    Err(error) => self
                        .worker_error_frame(request.request_id, WorkerError::from_fs_error(error)),
                }
            }
            MessageKind::Reply | MessageKind::Error => {
                Frame::error(request.request_id, ErrorCode::InvalidRequestKind)
            }
        }
    }

    pub async fn dispatch_encoded(&self, request: &[u8]) -> Result<Vec<u8>, DispatchError> {
        let request = Frame::decode(request).map_err(DispatchError::MalformedRequest)?;
        let response = self
            .dispatch(request)
            .await
            .map_err(DispatchError::Encode)?;
        response.encode().map_err(DispatchError::Encode)
    }

    fn worker_error_frame(&self, request_id: u64, error: WorkerError) -> Result<Frame, FrameError> {
        Frame::new(MessageKind::Error, request_id, error.encode()?)
    }

    fn handle(&self, id: u64) -> std::result::Result<Arc<dyn FileHandle>, FsError> {
        self.handles
            .lock()
            .map_err(|_| FsError::backend("worker handle table is poisoned"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| {
                FsError::new(mount_rs_core::ErrorCode::Ebadf)
                    .with_syscall("fskit-worker")
                    .with_message(format!("EBADF: unknown worker handle {id}"))
            })
    }

    fn allocate_handle(&self, handle: Arc<dyn FileHandle>) -> std::result::Result<u64, FsError> {
        let id = self
            .next_handle
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| FsError::new(mount_rs_core::ErrorCode::Emfile).with_syscall("open"))?;
        self.handles
            .lock()
            .map_err(|_| FsError::backend("worker handle table is poisoned"))?
            .insert(id, handle);
        Ok(id)
    }

    async fn close_handle(&self, id: u64) -> std::result::Result<(), FsError> {
        let handle = self
            .handles
            .lock()
            .map_err(|_| FsError::backend("worker handle table is poisoned"))?
            .remove(&id)
            .ok_or_else(|| {
                FsError::new(mount_rs_core::ErrorCode::Ebadf)
                    .with_syscall("close")
                    .with_message(format!("EBADF: unknown worker handle {id}"))
            })?;
        handle.close().await
    }

    async fn shutdown(&self) -> std::result::Result<(), FsError> {
        let handles = self
            .handles
            .lock()
            .map_err(|_| FsError::backend("worker handle table is poisoned"))?
            .drain()
            .map(|(_, handle)| handle)
            .collect::<Vec<_>>();
        let mut first_error = None;
        for handle in handles {
            if let Err(error) = handle.close().await {
                first_error.get_or_insert(error);
            }
        }
        let result = if let Some(error) = first_error {
            Err(error)
        } else {
            self.driver.syncfs().await
        };
        let Some(shutdown_hook) = &self.shutdown_hook else {
            return result;
        };
        let cleanup = shutdown_hook().await;
        match (result, cleanup) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    async fn dispatch_operation(
        &self,
        operation: OperationRequest,
    ) -> std::result::Result<OperationResponse, FsError> {
        use OperationRequest as Request;

        match operation {
            Request::Handshake { .. } | Request::Capabilities => {
                Ok(OperationResponse::Capabilities {
                    capabilities: self.capabilities(),
                })
            }
            Request::Sync => {
                self.driver.syncfs().await?;
                Ok(OperationResponse::Ok)
            }
            Request::Stat { path } => Ok(OperationResponse::Stats {
                stats: self
                    .driver
                    .stat(&mount_rs_core::path::normalize_path(&path))
                    .await?,
            }),
            Request::Lstat { path } => Ok(OperationResponse::Stats {
                stats: self
                    .driver
                    .lstat(&mount_rs_core::path::normalize_path(&path))
                    .await?,
            }),
            Request::Statfs { path } => Ok(OperationResponse::Statfs {
                stats: self
                    .driver
                    .statfs(&mount_rs_core::path::normalize_path(&path))
                    .await?,
            }),
            Request::Readdir { path } => {
                let path = mount_rs_core::path::normalize_path(&path);
                let entries = self.driver.readdir(&path).await?;
                let mut enriched = Vec::with_capacity(entries.len());
                for entry in entries {
                    let child_path = if path == "/" {
                        format!("/{0}", entry.name)
                    } else {
                        format!("{path}/{0}", entry.name)
                    };
                    enriched.push(WorkerDirEntry {
                        stats: self.driver.lstat(&child_path).await?,
                        entry,
                    });
                }
                Ok(OperationResponse::Entries { entries: enriched })
            }
            Request::Open { path, flags, mode } => {
                let path = mount_rs_core::path::normalize_path(&path);
                let handle = self.driver.open(&path, &flags, mode).await?;
                let stats = handle.stat().await?;
                let handle_id = self.allocate_handle(handle)?;
                Ok(OperationResponse::Open {
                    handle: handle_id,
                    stats,
                })
            }
            Request::Read {
                handle,
                offset,
                length,
            } => {
                let length = usize::try_from(length).map_err(|_| {
                    FsError::new(mount_rs_core::ErrorCode::Efbig).with_syscall("read")
                })?;
                if length > MAX_OPERATION_DATA_LEN {
                    return Err(FsError::new(mount_rs_core::ErrorCode::Efbig).with_syscall("read"));
                }
                let handle = self.handle(handle)?;
                let mut data = vec![0_u8; length];
                let count = handle.read(&mut data, Some(offset)).await?;
                data.truncate(count);
                Ok(OperationResponse::Read { data, count })
            }
            Request::Write {
                handle,
                offset,
                data,
            } => {
                if data.len() > MAX_OPERATION_DATA_LEN {
                    return Err(FsError::new(mount_rs_core::ErrorCode::Efbig).with_syscall("write"));
                }
                let count = self.handle(handle)?.write(&data, Some(offset)).await?;
                Ok(OperationResponse::Write { count })
            }
            Request::HandleStat { handle } => Ok(OperationResponse::Stats {
                stats: self.handle(handle)?.stat().await?,
            }),
            Request::TruncateHandle { handle, length } => {
                self.handle(handle)?.truncate(length).await?;
                Ok(OperationResponse::Ok)
            }
            Request::SyncHandle { handle } => {
                self.handle(handle)?.sync().await?;
                Ok(OperationResponse::Ok)
            }
            Request::DatasyncHandle { handle } => {
                self.handle(handle)?.datasync().await?;
                Ok(OperationResponse::Ok)
            }
            Request::Close { handle } => {
                self.close_handle(handle).await?;
                Ok(OperationResponse::Ok)
            }
            Request::Mkdir {
                path,
                recursive,
                mode,
            } => {
                self.driver
                    .mkdir(
                        &mount_rs_core::path::normalize_path(&path),
                        MkdirOptions { recursive, mode },
                    )
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Rmdir { path } => {
                self.driver
                    .rmdir(&mount_rs_core::path::normalize_path(&path))
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Unlink { path } => {
                self.driver
                    .unlink(&mount_rs_core::path::normalize_path(&path))
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Rename { old_path, new_path } => {
                self.driver
                    .rename(
                        &mount_rs_core::path::normalize_path(&old_path),
                        &mount_rs_core::path::normalize_path(&new_path),
                    )
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Link {
                existing_path,
                new_path,
            } => {
                self.driver
                    .link(
                        &mount_rs_core::path::normalize_path(&existing_path),
                        &mount_rs_core::path::normalize_path(&new_path),
                    )
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Symlink { target, path } => {
                self.driver
                    .symlink(&target, &mount_rs_core::path::normalize_path(&path))
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Readlink { path } => Ok(OperationResponse::Text {
                value: self
                    .driver
                    .readlink(&mount_rs_core::path::normalize_path(&path))
                    .await?,
            }),
            Request::Chmod { path, mode } => {
                self.driver
                    .chmod(&mount_rs_core::path::normalize_path(&path), mode)
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Chown { path, uid, gid } => {
                self.driver
                    .chown(&mount_rs_core::path::normalize_path(&path), uid, gid)
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Lchown { path, uid, gid } => {
                self.driver
                    .lchown(&mount_rs_core::path::normalize_path(&path), uid, gid)
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Truncate { path, length } => {
                self.driver
                    .truncate(&mount_rs_core::path::normalize_path(&path), length)
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Utimes {
                path,
                atime_ms,
                mtime_ms,
            } => {
                self.driver
                    .utimes(
                        &mount_rs_core::path::normalize_path(&path),
                        atime_ms,
                        mtime_ms,
                    )
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Lutimes {
                path,
                atime_ms,
                mtime_ms,
            } => {
                self.driver
                    .lutimes(
                        &mount_rs_core::path::normalize_path(&path),
                        atime_ms,
                        mtime_ms,
                    )
                    .await?;
                Ok(OperationResponse::Ok)
            }
            Request::Mknod { path, mode, dev } => {
                self.driver
                    .mknod(&mount_rs_core::path::normalize_path(&path), mode, dev)
                    .await?;
                Ok(OperationResponse::Ok)
            }
        }
    }
}

static NEXT_BACKEND_OWNER: AtomicU64 = AtomicU64::new(1);
const MAX_CONFIG_LEN: usize = 64 * 1024;

fn backend_owner() -> String {
    let sequence = NEXT_BACKEND_OWNER.fetch_add(1, Ordering::Relaxed);
    format!("mount-rs-fskit-{}-{sequence}", std::process::id())
}

fn validate_database_path(path: &Path, label: &'static str) -> mount_rs_core::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(FsError::new(mount_rs_core::ErrorCode::Einval)
            .with_syscall("fskit-worker")
            .with_message(format!("{label} path must not be empty")));
    }
    Ok(())
}

async fn build_driver_worker(config: WorkerBackendConfig) -> mount_rs_core::Result<DriverWorker> {
    match config {
        WorkerBackendConfig::Memory { read_only } => Ok(DriverWorker::memory(read_only)),
        WorkerBackendConfig::Host { root, read_only } => {
            validate_database_path(&root, "host root")?;
            let driver = HostFs::with_options(root, HostFsOptions { read_only });
            Ok(DriverWorker::new(Arc::new(driver), read_only))
        }
        WorkerBackendConfig::Sqlite {
            database_path,
            read_only,
        } => {
            validate_database_path(&database_path, "SQLite database")?;
            let driver = open_sqlite(database_path).await?;
            Ok(DriverWorker::new(Arc::new(driver), read_only))
        }
        WorkerBackendConfig::SplitSqlite {
            metadata_path,
            blocks_path,
            chunk_size,
            read_only,
        } => {
            validate_database_path(&metadata_path, "SQLite metadata database")?;
            validate_database_path(&blocks_path, "SQLite block database")?;
            if metadata_path == blocks_path {
                return Err(FsError::new(mount_rs_core::ErrorCode::Einval)
                    .with_syscall("fskit-worker")
                    .with_message(
                        "SQLite metadata and block databases must be independent paths",
                    ));
            }
            let options = ChunkedOptions::fixed(backend_owner(), chunk_size)?;
            let driver = ChunkedFs::open(
                SqliteMetadataStore::open(metadata_path)?,
                SqliteBlockStore::open(blocks_path)?,
                options,
            )
            .await?;
            let shutdown_driver = driver.clone();
            let shutdown_hook: DriverShutdown = Arc::new(move || {
                let driver = shutdown_driver.clone();
                Box::pin(async move { driver.shutdown().await })
            });
            Ok(DriverWorker::with_shutdown_hook(
                Arc::new(driver),
                read_only,
                shutdown_hook,
            ))
        }
    }
}

/// Opaque native handle for the bundled XPC service. The C ABI uses the same
/// caller-buffer convention as the protocol bridge and never returns a
/// fabricated response when the request frame itself is malformed.
pub struct RustWorkerHandle {
    runtime: Runtime,
    worker: DriverWorker,
}

fn create_worker_handle(config: WorkerBackendConfig) -> *mut RustWorkerHandle {
    let Ok(runtime) = Runtime::new() else {
        return std::ptr::null_mut();
    };
    let Ok(worker) = runtime.block_on(build_driver_worker(config)) else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(RustWorkerHandle { runtime, worker }))
}

/// Create a persistent worker from a bounded JSON backend configuration.
///
/// The accepted shapes are `memory`, `host`, `sqlite`, and `splitSqlite`. The latter
/// requires distinct `metadataPath` and `blocksPath` values and uses the same
/// `SqliteMetadataStore`/`SqliteBlockStore` pair as the existing split-store
/// runtime. Invalid JSON, paths, or provider initialization return null and do
/// not start a partially initialized worker.
///
/// # Safety
///
/// When `config_len` is nonzero, `config_ptr` must point to a readable UTF-8
/// JSON region of that length. The region is copied and is not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mount_rs_fskit_create_worker(
    config_ptr: *const u8,
    config_len: usize,
) -> *mut RustWorkerHandle {
    if config_len > MAX_CONFIG_LEN || (config_len != 0 && config_ptr.is_null()) {
        return std::ptr::null_mut();
    }
    let config = if config_len == 0 {
        return std::ptr::null_mut();
    } else {
        // SAFETY: the caller contract requires a readable region when the
        // pointer is non-null and the length is nonzero.
        unsafe { std::slice::from_raw_parts(config_ptr, config_len) }
    };
    let Ok(config) = serde_json::from_slice::<WorkerBackendConfig>(config) else {
        return std::ptr::null_mut();
    };
    create_worker_handle(config)
}

#[unsafe(no_mangle)]
pub extern "C" fn mount_rs_fskit_create_memory_worker(read_only: u8) -> *mut RustWorkerHandle {
    create_worker_handle(WorkerBackendConfig::memory(read_only != 0))
}

/// Dispatch one frame through a persistent provider-backed worker.
///
/// # Safety
///
/// `worker` must be a pointer returned by
/// [`mount_rs_fskit_create_worker`] or
/// [`mount_rs_fskit_create_memory_worker`] that has not been destroyed.
/// Buffer pointers must satisfy the same readable/writable region contracts
/// as [`mount_rs_fskit_dispatch`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mount_rs_fskit_worker_dispatch(
    worker: *mut RustWorkerHandle,
    request_ptr: *const u8,
    request_len: usize,
    response_ptr: *mut u8,
    response_capacity: usize,
    response_len: *mut usize,
) -> i32 {
    if worker.is_null()
        || response_len.is_null()
        || (request_len != 0 && request_ptr.is_null())
        || (response_capacity != 0 && response_ptr.is_null())
    {
        return STATUS_INVALID_ARGUMENT;
    }

    let request = if request_len == 0 {
        &[]
    } else {
        // SAFETY: validated by the caller contract above.
        unsafe { std::slice::from_raw_parts(request_ptr, request_len) }
    };
    // SAFETY: the caller contract requires a live handle from the create
    // function and it is not aliased mutably by this call.
    let worker = unsafe { &mut *worker };
    let encoded = match worker
        .runtime
        .block_on(worker.worker.dispatch_encoded(request))
    {
        Ok(encoded) => encoded,
        Err(DispatchError::MalformedRequest(_)) => return STATUS_MALFORMED_REQUEST,
        Err(DispatchError::Encode(_)) => return STATUS_INTERNAL_ERROR,
    };

    // SAFETY: response_len was checked non-null above.
    unsafe { *response_len = encoded.len() };
    if response_capacity < encoded.len() {
        return STATUS_RESPONSE_TOO_SMALL;
    }
    if !encoded.is_empty() {
        // SAFETY: the capacity check proves the destination is large enough.
        unsafe { std::ptr::copy_nonoverlapping(encoded.as_ptr(), response_ptr, encoded.len()) };
    }
    STATUS_OK
}

/// Destroy a worker created by [`mount_rs_fskit_create_worker`] or
/// [`mount_rs_fskit_create_memory_worker`].
///
/// # Safety
///
/// `worker` must be null or a pointer returned by the create function, and no
/// other thread may use it after this call begins.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mount_rs_fskit_destroy_worker(worker: *mut RustWorkerHandle) {
    if !worker.is_null() {
        // SAFETY: the pointer is owned by the caller under the function
        // contract and is consumed exactly once here.
        unsafe { drop(Box::from_raw(worker)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_response(
        worker: &DriverWorker,
        request_id: u64,
        request: OperationRequest,
    ) -> OperationResponse {
        let body = serde_json::to_vec(&request).expect("operation serializes");
        let request = frame(MessageKind::Operation, request_id, &body);
        let runtime = Runtime::new().expect("test runtime");
        let response = runtime
            .block_on(worker.dispatch(request))
            .expect("response frame");
        assert_eq!(response.kind, MessageKind::Reply);
        serde_json::from_slice(&response.body).expect("operation response decodes")
    }

    fn frame(kind: MessageKind, request_id: u64, body: &[u8]) -> Frame {
        Frame::new(kind, request_id, body.to_vec()).expect("test frame is bounded")
    }

    #[test]
    fn hello_wire_bytes_are_fixed_and_little_endian() {
        let encoded = frame(MessageKind::Hello, 0x0102_0304_0506_0708, &[])
            .encode()
            .expect("hello encodes");

        assert_eq!(
            encoded,
            vec![
                b'M', b'R', b'F', b'S', 1, 0, 1, 0, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0,
            ]
        );
        assert_eq!(
            Frame::decode(&encoded),
            Ok(frame(MessageKind::Hello, 0x0102_0304_0506_0708, &[]))
        );
    }

    #[test]
    fn malformed_frames_are_rejected_without_normalizing_them() {
        let valid = frame(MessageKind::Hello, 7, &[]).encode().expect("valid");

        let mut bad_magic = valid.clone();
        bad_magic[0] = b'X';
        assert_eq!(Frame::decode(&bad_magic), Err(DecodeError::InvalidMagic));

        let mut bad_version = valid.clone();
        bad_version[4] = 2;
        assert_eq!(
            Frame::decode(&bad_version),
            Err(DecodeError::UnsupportedVersion { version: 2 })
        );

        let mut bad_flags = valid.clone();
        bad_flags[7] = 1;
        assert_eq!(
            Frame::decode(&bad_flags),
            Err(DecodeError::UnsupportedFlags { flags: 1 })
        );

        let mut bad_length = valid.clone();
        bad_length[16] = 1;
        assert_eq!(
            Frame::decode(&bad_length),
            Err(DecodeError::LengthMismatch {
                expected: HEADER_LEN + 1,
                actual: HEADER_LEN,
            })
        );

        assert_eq!(
            Frame::decode(&valid[..HEADER_LEN - 1]),
            Err(DecodeError::TooShort {
                actual: HEADER_LEN - 1,
            })
        );
    }

    #[test]
    fn body_limit_is_enforced_before_allocation() {
        let mut encoded = frame(MessageKind::Hello, 1, &[]).encode().expect("header");
        let too_large = (MAX_BODY_LEN as u32) + 1;
        encoded[16..20].copy_from_slice(&too_large.to_le_bytes());
        assert_eq!(
            Frame::decode(&encoded),
            Err(DecodeError::BodyTooLarge {
                len: MAX_BODY_LEN + 1
            })
        );
    }

    #[test]
    fn compile_only_delegate_has_explicit_control_and_operation_semantics() {
        let mut delegate = CompileOnlyDelegate::default();

        let hello = delegate
            .dispatch(frame(MessageKind::Hello, 11, &[]))
            .expect("hello reply");
        assert_eq!(hello.kind, MessageKind::Reply);
        assert_eq!(hello.request_id, 11);
        assert_eq!(hello.body, PROTOCOL_VERSION.to_le_bytes());

        let operation = delegate
            .dispatch(frame(MessageKind::Operation, 12, b"read"))
            .expect("typed operation reply");
        assert_eq!(operation.kind, MessageKind::Error);
        assert_eq!(
            operation.body,
            ErrorCode::UnsupportedOperation.as_u16().to_le_bytes()
        );

        let shutdown = delegate
            .dispatch(frame(MessageKind::Shutdown, 13, &[]))
            .expect("shutdown reply");
        assert_eq!(shutdown.kind, MessageKind::Reply);
        assert!(delegate.is_shutting_down());

        let after_shutdown = delegate
            .dispatch(frame(MessageKind::Operation, 14, b"write"))
            .expect("typed shutdown error");
        assert_eq!(after_shutdown.kind, MessageKind::Error);
        assert_eq!(
            after_shutdown.body,
            ErrorCode::ShuttingDown.as_u16().to_le_bytes()
        );
    }

    #[test]
    fn c_abi_dispatch_uses_caller_buffer_and_reports_required_capacity() {
        let request = frame(MessageKind::Hello, 21, &[])
            .encode()
            .expect("request");
        let mut response = [0_u8; HEADER_LEN + 2];
        let mut response_len = 0;

        let status = unsafe {
            mount_rs_fskit_dispatch(
                request.as_ptr(),
                request.len(),
                response.as_mut_ptr(),
                response.len(),
                &mut response_len,
            )
        };
        assert_eq!(status, STATUS_OK);
        assert_eq!(response_len, response.len());
        assert_eq!(
            Frame::decode(&response),
            Ok(frame(MessageKind::Reply, 21, &[1, 0]))
        );

        let mut short = [0_u8; HEADER_LEN];
        let status = unsafe {
            mount_rs_fskit_dispatch(
                request.as_ptr(),
                request.len(),
                short.as_mut_ptr(),
                short.len(),
                &mut response_len,
            )
        };
        assert_eq!(status, STATUS_RESPONSE_TOO_SMALL);
        assert_eq!(response_len, HEADER_LEN + 2);
    }

    #[test]
    fn c_abi_rejects_malformed_input_without_fabricating_response() {
        let mut response = [0_u8; HEADER_LEN + 2];
        let mut response_len = 99;
        let status = unsafe {
            mount_rs_fskit_dispatch(
                response.as_ptr(),
                HEADER_LEN - 1,
                response.as_mut_ptr(),
                response.len(),
                &mut response_len,
            )
        };
        assert_eq!(status, STATUS_MALFORMED_REQUEST);
        assert_eq!(response_len, 99);

        let status = unsafe {
            mount_rs_fskit_dispatch(
                std::ptr::null(),
                0,
                response.as_mut_ptr(),
                response.len(),
                &mut response_len,
            )
        };
        assert_eq!(status, STATUS_MALFORMED_REQUEST);
    }

    #[test]
    fn driver_worker_dispatches_real_fsdriver_namespace_and_io() {
        let worker = DriverWorker::memory(false);

        let capabilities = operation_response(&worker, 30, OperationRequest::Capabilities);
        let OperationResponse::Capabilities { capabilities } = capabilities else {
            panic!("capabilities response expected");
        };
        assert!(capabilities.handles);
        assert!(capabilities.symlinks);

        let _ = operation_response(
            &worker,
            31,
            OperationRequest::Open {
                path: "/hello.txt".to_owned(),
                flags: "w".to_owned(),
                mode: 0o644,
            },
        );
        let open = operation_response(
            &worker,
            32,
            OperationRequest::Open {
                path: "/hello.txt".to_owned(),
                flags: "r+".to_owned(),
                mode: 0,
            },
        );
        let OperationResponse::Open { handle, .. } = open else {
            panic!("open response expected");
        };
        let written = operation_response(
            &worker,
            33,
            OperationRequest::Write {
                handle,
                offset: 0,
                data: b"hello".to_vec(),
            },
        );
        assert!(matches!(written, OperationResponse::Write { count: 5 }));
        let read = operation_response(
            &worker,
            34,
            OperationRequest::Read {
                handle,
                offset: 0,
                length: 32,
            },
        );
        assert!(matches!(read, OperationResponse::Read { data, count: 5 } if data == b"hello"));
        assert!(matches!(
            operation_response(&worker, 35, OperationRequest::Close { handle }),
            OperationResponse::Ok
        ));

        let entries = operation_response(
            &worker,
            36,
            OperationRequest::Readdir {
                path: "/".to_owned(),
            },
        );
        let OperationResponse::Entries { entries } = entries else {
            panic!("directory response expected");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.name, "hello.txt");
        assert!(entries[0].stats.is_file());
    }

    #[test]
    fn host_backend_round_trips_bytes_to_its_rooted_path() {
        let root = std::env::temp_dir().join(format!(
            "mount-rs-fskit-host-{}-{}",
            std::process::id(),
            NEXT_BACKEND_OWNER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).expect("host backend root directory");
        let config = WorkerBackendConfig::Host {
            root: root.clone(),
            read_only: false,
        };
        let runtime = Runtime::new().expect("test runtime");
        let worker = runtime
            .block_on(build_driver_worker(config))
            .expect("host worker");

        let OperationResponse::Open { handle, .. } = operation_response(
            &worker,
            37,
            OperationRequest::Open {
                path: "/shared.txt".to_owned(),
                flags: "w+".to_owned(),
                mode: 0o644,
            },
        ) else {
            panic!("open response expected");
        };
        let payload = b"FSKit and NFS share this rooted path".to_vec();
        assert!(matches!(
            operation_response(
                &worker,
                38,
                OperationRequest::Write {
                    handle,
                    offset: 0,
                    data: payload.clone(),
                },
            ),
            OperationResponse::Write { count } if count == payload.len()
        ));
        assert!(matches!(
            operation_response(&worker, 39, OperationRequest::Close { handle }),
            OperationResponse::Ok
        ));
        assert_eq!(
            std::fs::read(root.join("shared.txt")).expect("host file exists"),
            payload
        );

        let shutdown = runtime
            .block_on(worker.dispatch(frame(MessageKind::Shutdown, 40, &[])))
            .expect("host shutdown response");
        assert_eq!(shutdown.kind, MessageKind::Reply);
        drop(worker);
        std::fs::remove_dir_all(root).expect("remove host backend root");
    }

    #[test]
    fn operation_wire_keys_match_swift_client_camel_case() {
        let handshake = serde_json::to_value(OperationRequest::Handshake { read_only: true })
            .expect("handshake serializes");
        assert_eq!(handshake["operation"], "handshake");
        assert_eq!(handshake["readOnly"], true);

        let rename = serde_json::to_value(OperationRequest::Rename {
            old_path: "/old".to_owned(),
            new_path: "/new".to_owned(),
        })
        .expect("rename serializes");
        assert_eq!(rename["oldPath"], "/old");
        assert_eq!(rename["newPath"], "/new");

        let utimes = serde_json::to_value(OperationRequest::Utimes {
            path: "/file".to_owned(),
            atime_ms: 1,
            mtime_ms: 2,
        })
        .expect("utimes serializes");
        assert_eq!(utimes["atimeMs"], 1);
        assert_eq!(utimes["mtimeMs"], 2);
    }

    #[test]
    fn driver_worker_preserves_provider_errno_and_read_only_policy() {
        let worker = DriverWorker::memory(true);
        let body = serde_json::to_vec(&OperationRequest::Open {
            path: "/new.txt".to_owned(),
            flags: "w".to_owned(),
            mode: 0o644,
        })
        .expect("operation serializes");
        let runtime = Runtime::new().expect("test runtime");
        let response = runtime
            .block_on(worker.dispatch(frame(MessageKind::Operation, 40, &body)))
            .expect("response frame");
        assert_eq!(response.kind, MessageKind::Error);
        let error: WorkerError = serde_json::from_slice(&response.body).expect("worker error");
        assert_eq!(error.errno, 30);
        assert_eq!(error.code, "EROFS");

        let missing = serde_json::to_vec(&OperationRequest::Stat {
            path: "/missing".to_owned(),
        })
        .expect("operation serializes");
        let response = runtime
            .block_on(worker.dispatch(frame(MessageKind::Operation, 41, &missing)))
            .expect("response frame");
        let error: WorkerError = serde_json::from_slice(&response.body).expect("worker error");
        assert_eq!(error.errno, 2);
        assert_eq!(error.code, "ENOENT");
    }

    #[test]
    fn driver_worker_rejects_bad_operation_and_closes_handles_on_shutdown() {
        let worker = DriverWorker::memory(false);
        let runtime = Runtime::new().expect("test runtime");
        let malformed = runtime
            .block_on(worker.dispatch(frame(MessageKind::Operation, 50, b"not-json")))
            .expect("typed protocol error");
        assert_eq!(malformed.kind, MessageKind::Error);
        let error: WorkerError = serde_json::from_slice(&malformed.body).expect("worker error");
        assert_eq!(error.errno, 71);
        assert_eq!(error.code, "EPROTO");

        let body = serde_json::to_vec(&OperationRequest::Open {
            path: "/held.txt".to_owned(),
            flags: "w".to_owned(),
            mode: 0o644,
        })
        .expect("operation serializes");
        let open = runtime
            .block_on(worker.dispatch(frame(MessageKind::Operation, 51, &body)))
            .expect("open response");
        assert_eq!(open.kind, MessageKind::Reply);

        let shutdown = runtime
            .block_on(worker.dispatch(frame(MessageKind::Shutdown, 52, &[])))
            .expect("shutdown response");
        assert_eq!(shutdown.kind, MessageKind::Reply);
        assert!(worker.is_shutting_down());
        let after = runtime
            .block_on(worker.dispatch(frame(MessageKind::Operation, 53, &body)))
            .expect("post-shutdown response");
        assert_eq!(after.kind, MessageKind::Error);
        assert_eq!(after.body, ErrorCode::ShuttingDown.as_u16().to_le_bytes());
    }

    #[test]
    fn split_sqlite_backend_reopens_binary_partial_io() {
        let stem = std::env::temp_dir().join(format!(
            "mount-rs-fskit-split-{}-{}",
            std::process::id(),
            NEXT_BACKEND_OWNER.fetch_add(1, Ordering::Relaxed)
        ));
        let metadata_path = stem.with_extension("metadata.sqlite");
        let blocks_path = stem.with_extension("blocks.sqlite");
        let config = WorkerBackendConfig::SplitSqlite {
            metadata_path: metadata_path.clone(),
            blocks_path: blocks_path.clone(),
            chunk_size: 4096,
            read_only: false,
        };
        let runtime = Runtime::new().expect("test runtime");
        let worker = runtime
            .block_on(build_driver_worker(config.clone()))
            .expect("split SQLite worker");

        let OperationResponse::Open { handle, .. } = operation_response(
            &worker,
            60,
            OperationRequest::Open {
                path: "/partial.bin".to_owned(),
                flags: "w+".to_owned(),
                mode: 0o600,
            },
        ) else {
            panic!("open response expected");
        };
        let first = vec![0_u8, 1, 127, 128, 254, 255];
        let second = vec![0x00_u8, 0x80, 0xff, 0x42];
        assert!(matches!(
            operation_response(
                &worker,
                61,
                OperationRequest::Write {
                    handle,
                    offset: 3,
                    data: first.clone(),
                },
            ),
            OperationResponse::Write { count: 6 }
        ));
        assert!(matches!(
            operation_response(
                &worker,
                62,
                OperationRequest::Write {
                    handle,
                    offset: 4096,
                    data: second.clone(),
                },
            ),
            OperationResponse::Write { count: 4 }
        ));
        assert!(matches!(
            operation_response(&worker, 63, OperationRequest::Close { handle }),
            OperationResponse::Ok
        ));
        let shutdown = runtime
            .block_on(worker.dispatch(frame(MessageKind::Shutdown, 64, &[])))
            .expect("shutdown response");
        assert_eq!(shutdown.kind, MessageKind::Reply);
        drop(worker);

        let reopened = runtime
            .block_on(build_driver_worker(config))
            .expect("reopened split SQLite worker");
        let OperationResponse::Open { handle, stats } = operation_response(
            &reopened,
            65,
            OperationRequest::Open {
                path: "/partial.bin".to_owned(),
                flags: "r".to_owned(),
                mode: 0,
            },
        ) else {
            panic!("reopen response expected");
        };
        assert_eq!(stats.size, 4096 + second.len() as u64);
        let OperationResponse::Read { data, count } = operation_response(
            &reopened,
            66,
            OperationRequest::Read {
                handle,
                offset: 0,
                length: stats.size,
            },
        ) else {
            panic!("read response expected");
        };
        let mut expected = vec![0_u8; 4096 + second.len()];
        expected[3..3 + first.len()].copy_from_slice(&first);
        expected[4096..].copy_from_slice(&second);
        assert_eq!(count, expected.len());
        assert_eq!(data, expected);
        assert!(matches!(
            operation_response(&reopened, 67, OperationRequest::Close { handle }),
            OperationResponse::Ok
        ));
        let shutdown = runtime
            .block_on(reopened.dispatch(frame(MessageKind::Shutdown, 68, &[])))
            .expect("reopened shutdown response");
        assert_eq!(shutdown.kind, MessageKind::Reply);

        let _ = std::fs::remove_file(metadata_path);
        let _ = std::fs::remove_file(blocks_path);
    }
}
