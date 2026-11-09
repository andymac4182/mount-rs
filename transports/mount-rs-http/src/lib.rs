//! Versioned, multi-drive HTTP transport for mount-rs.
//!
//! The transport deliberately accepts the same `Arc<dyn FsDriver>` boundary as
//! the native transports.  A drive is only a name, an authorization secret,
//! and that driver; HTTP never mounts or reimplements a filesystem.

mod server;

pub use server::{
    BoxError, DEFAULT_DRAIN_TIMEOUT, DEFAULT_MAX_REQUEST_BYTES, DEFAULT_READ_CHUNK_BYTES,
    DriveConfig, DriveRegistry, HttpBody, HttpRequestHandler, HttpServer, HttpServerError,
    HttpServerOptions, RegistryError,
};
