//! Portable WebDAV transport for `mount-rs-core`.
//!
//! The crate serves the core driver's hierarchical namespace over HTTP.  It
//! deliberately does not invoke a native mount helper: loopback HTTP tests are
//! portable on macOS and Linux, while `davfs2`/FUSE on Linux and
//! `mount_webdav` on macOS are external client prerequisites rather than part
//! of this library.
//!
//! Supported surface:
//!
//! * RFC 4918 class 1 methods: `OPTIONS`, `GET`, `HEAD`, `PUT`, `MKCOL`,
//!   `DELETE`, `COPY`, `MOVE`, and bounded `PROPFIND` (`Depth: 0`/`1`).
//! * Class 2 `LOCK`/`UNLOCK`, exclusive/shared write locks, `If` state tokens,
//!   and lock expiry.
//! * `PROPPATCH` for the core driver's writable `getlastmodified` property;
//!   derived/dead properties are reported as protected and the operation is
//!   atomic.
//! * Basic authentication, byte ranges, conditional requests, bounded request
//!   bodies, streamed file responses, recursive transfer/delete, and
//!   errno-aware HTTP status mapping.
//!
//! Intentional remainder: extended MKCOL bodies, dead-property storage,
//! multi-range responses, and native kernel mount orchestration are not part of
//! the core driver contract or this crate.  Unsupported methods answer `405`
//! with `Allow`.  The HTTP integration tests never claim native mount
//! verification.

mod constants;
pub mod locks;
pub mod protocol;
pub mod server;
pub mod session;

pub use constants::DEFAULT_HOST;
pub use locks::{
    DavLock, DavLockGrant, DavLockRequest, DavLockTable, DavLockTableOptions, LockDepth,
};
pub use protocol::{
    DavFault, DavPropertyName, Depth, FileBody, IfCondition, IfList, LockInfoRequest, LockTimeout,
    MultistatusEntry, PropfindRequest, ProppatchInstruction, ProppatchRequest, ProppatchSet,
    Propstat, RangeSpec, WebdavBody, WebdavError, WebdavRequestHead, WebdavResponse, XmlNode,
};
pub use server::{
    DEFAULT_DRAIN_TIMEOUT, WebdavBindError, WebdavServer, WebdavServerError, WebdavServerOptions,
    bind_refusal, create_webdav_server, is_loopback_host,
};
pub use session::{WebdavCredentials, WebdavSession, WebdavSessionOptions, WebdavSessionStats};
