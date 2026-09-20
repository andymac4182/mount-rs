//! Public S3 wire constants and error mapping.
//!
//! The values here are deliberately re-exports of the protocol and SigV4
//! implementations rather than a second table that can drift. The generic XML
//! parser and the session's incremental body state remain outside this facade.

pub use crate::protocol::{
    MAX_KEY_BYTES, MAX_KEYS, MAX_PART_SIZE, MAX_PARTS, MAX_PARTS_PER_PAGE, MAX_XML_BYTES,
    MIN_PART_SIZE, MULTIPART_PREFIX, OBJECT_CONTENT_TYPE, S3Error, S3Failure, S3Result,
    SYNTHETIC_OWNER_ID, SYNTHETIC_OWNER_NAME, XML_CONTENT_TYPE, error_with_message, s3_error,
    s3_error_of,
};
pub use crate::sigv4::{
    EMPTY_PAYLOAD_SHA256, STREAMING_PAYLOAD, STREAMING_PAYLOAD_TRAILER,
    STREAMING_UNSIGNED_PAYLOAD_TRAILER, UNSIGNED_PAYLOAD,
};

/// The S3 "Content-Encoding" token used for AWS chunked framing.
pub const AWS_CHUNKED_ENCODING: &str = "aws-chunked";

/// Return the S3 error for a POSIX errno name, or the InternalError fallback
/// if unknown.
///
/// This is the string-shaped counterpart to
/// crate::protocol::s3_error_of, which accepts the core driver's typed
/// mount_rs_core::FsError.
pub fn s3_error_of_errno(name: Option<&str>) -> S3Error {
    let code = match name {
        Some("EPERM") | Some("EACCES") | Some("EROFS") => "AccessDenied",
        Some("ENOENT") | Some("ENOTDIR") | Some("ESTALE") => "NoSuchKey",
        Some("EAGAIN") | Some("EMFILE") | Some("ENFILE") => "SlowDown",
        Some("ENOMEM") | Some("ENOSPC") | Some("EDQUOT") => "ServiceUnavailable",
        Some("EBUSY") | Some("EEXIST") => "OperationAborted",
        Some("EISDIR") => "InvalidRequest",
        Some("EINVAL") => "InvalidArgument",
        Some("EFBIG") => "EntityTooLarge",
        Some("ENAMETOOLONG") => "KeyTooLongError",
        Some("ENOSYS") | Some("ENOTSUP") => "NotImplemented",
        Some("ENOTEMPTY") => "BucketNotEmpty",
        _ => "InternalError",
    };
    s3_error(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_name_mapping_has_the_upstream_fallback_and_load_bearing_rows() {
        assert_eq!(s3_error_of_errno(Some("ENOENT")).status, 404);
        assert_eq!(s3_error_of_errno(Some("EACCES")).code, "AccessDenied");
        assert_eq!(s3_error_of_errno(Some("ENOTEMPTY")).code, "BucketNotEmpty");
        assert_eq!(s3_error_of_errno(Some("ENOSYS")).code, "NotImplemented");
        assert_eq!(s3_error_of_errno(None).code, "InternalError");
        assert_eq!(s3_error_of_errno(Some("toString")).code, "InternalError");
    }
}
