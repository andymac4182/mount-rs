//! Rootless, path-style S3 gateway for mount-rs drivers.
//!
//! The transport deliberately keeps HTTP/S3 concerns outside `mount-rs-core`.
//! [`S3Session`] is useful for embedding the protocol in another HTTP runtime;
//! [`S3Server`] provides the bundled Tokio/Axum listener.

mod protocol;
mod server;
mod session;
mod sigv4;

pub use protocol::S3Response;
pub use server::{S3BindError, S3Server, S3ServerOptions, create_s3_server};
pub use session::{S3Request, S3RequestHead, S3Session, S3SessionOptions, S3SessionStats};
pub use sigv4::{
    CredentialScope, Credentials, EMPTY_PAYLOAD_SHA256, HeaderEntry, PresignRequest,
    PresignedRequest, QueryEntry, STREAMING_PAYLOAD, STREAMING_PAYLOAD_TRAILER,
    STREAMING_UNSIGNED_PAYLOAD_TRAILER, SigV4Failure, SignRequest, UNSIGNED_PAYLOAD, VerifyRequest,
    canonical_query, canonical_request, format_amz_date, presign_request, sha256_hex, sign_chunk,
    sign_request, sign_trailer,
};
