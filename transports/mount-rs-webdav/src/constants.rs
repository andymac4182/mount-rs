//! RFC 4918 literals and the errno-to-HTTP mapping used by the transport.

use mount_rs_core::ErrorCode;

pub const DAV_NS: &str = "DAV:";
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DAV_COMPLIANCE: &str = "1, 2, 3";
pub const MS_AUTHOR_VIA: &str = "DAV";
pub const RESOURCE_CONTENT_TYPE: &str = "application/octet-stream";
pub const COLLECTION_CONTENT_TYPE: &str = "httpd/unix-directory";
pub const XML_CONTENT_TYPE: &str = "application/xml; charset=\"utf-8\"";
pub const MAX_XML_BYTES: usize = 256 * 1024;
pub const MAX_XML_DEPTH: usize = 32;
pub const MAX_XML_ELEMENTS: usize = 100_000;
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
/// Maximum number of child resources materialized by one WebDAV directory
/// walk. Drivers enforce the bound before returning the directory listing.
pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 4096;
pub const READ_CHUNK_BYTES: usize = 128 * 1024;
pub const LOCK_TOKEN_PREFIX: &str = "urn:uuid:";
pub const DEFAULT_LOCK_TIMEOUT_SECONDS: u64 = 600;
pub const MAX_LOCK_TIMEOUT_SECONDS: u64 = 3600;
pub const MAX_LOCKS: usize = 4096;

pub const ALLOW_HEADER: &str =
    "OPTIONS, HEAD, GET, PUT, DELETE, MKCOL, COPY, MOVE, PROPFIND, PROPPATCH, LOCK, UNLOCK";

pub fn status_text(status: u16) -> Option<&'static str> {
    Some(match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        423 => "Locked",
        424 => "Failed Dependency",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        _ => return None,
    })
}

pub fn status_line(status: u16) -> String {
    match status_text(status) {
        Some(text) => format!("HTTP/1.1 {status} {text}"),
        None => format!("HTTP/1.1 {status}"),
    }
}

pub fn status_for_error(code: ErrorCode) -> u16 {
    match code {
        ErrorCode::Eperm | ErrorCode::Eacces | ErrorCode::Erofs | ErrorCode::Emlink => 403,
        ErrorCode::Enoent | ErrorCode::Enodev | ErrorCode::Enodata | ErrorCode::Estale => 404,
        ErrorCode::Eintr
        | ErrorCode::Eio
        | ErrorCode::Ebadf
        | ErrorCode::Espipe
        | ErrorCode::Erange
        | ErrorCode::Eproto
        | ErrorCode::Eoverflow => 500,
        ErrorCode::Enxio => 403,
        ErrorCode::Eagain | ErrorCode::Enomem | ErrorCode::Enfile | ErrorCode::Emfile => 503,
        ErrorCode::Ebusy | ErrorCode::Eexist | ErrorCode::Enotdir | ErrorCode::Enotempty => 409,
        ErrorCode::Exdev => 502,
        ErrorCode::Eisdir => 405,
        ErrorCode::Einval => 400,
        ErrorCode::Efbig => 413,
        ErrorCode::Enospc | ErrorCode::Edquot => 507,
        ErrorCode::Enametoolong => 414,
        ErrorCode::Enosys | ErrorCode::Enotsup => 501,
        ErrorCode::Eloop => 508,
    }
}

pub fn is_absent(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::Enoent | ErrorCode::Enotdir)
}
