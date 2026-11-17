//! Versioned, multi-drive HTTP transport for mount-rs.
//!
//! The transport deliberately accepts the same `Arc<dyn FsDriver>` boundary as
//! the native transports.  A drive is only a name, an authorization secret,
//! and that driver; HTTP never mounts or reimplements a filesystem.

mod conditional;
mod server;

pub use server::{
    AccessLog, AccessRecord, BoxError, DEFAULT_DRAIN_TIMEOUT, DEFAULT_MAX_REQUEST_BYTES,
    DEFAULT_MAX_UPLOAD_BYTES, DEFAULT_READ_CHUNK_BYTES, DriveConfig, DriveRegistry, HealthCheck,
    HttpBody, HttpRequestHandler, HttpServer, HttpServerError, HttpServerOptions,
    MAX_REQUEST_ID_BYTES, REQUEST_ID_HEADER, RegistryError, is_file_upload, streams_upload,
};
