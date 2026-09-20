//! Rootless, path-style S3 gateway for mount-rs drivers.
//!
//! The transport deliberately keeps HTTP/S3 concerns outside `mount-rs-core`.
//! [`S3Session`] is useful for embedding the protocol in another HTTP runtime;
//! [`S3Server`] provides the bundled Tokio/Axum listener.

pub mod chunked;
pub mod constants;
pub mod protocol;
pub mod server;
pub mod session;
pub mod sigv4;
pub mod xml;

pub use chunked::{
    AwsChunkedDecoder, AwsChunkedParams, CHUNK_ALGORITHM, CHUNK_SIGNATURE_PARAMETER,
    CHUNKED_MAX_DECLARED_SIZE, CHUNKED_MAX_FRAME, CHUNKED_MAX_HEADER_BYTES, CHUNKED_MAX_HEX_DIGITS,
    CHUNKED_MAX_TRAILER_BYTES, ChunkedError, ChunkedLimits, ChunkedRefusal, ChunkedSignature,
    ChunkedTrailer, StreamingPayloadKind, TRAILER_ALGORITHM, TRAILER_SIGNATURE_HEADER,
    chunk_string_to_sign, chunked_signing_key, decode_aws_chunked, is_chunked_error,
    sign_chunk as sign_chunked, sign_trailer as sign_trailer_chunked, streaming_payload_kind,
    trailer_string_to_sign,
};
pub use constants::{
    AWS_CHUNKED_ENCODING, MAX_KEY_BYTES, MAX_KEYS, MAX_PART_SIZE, MAX_PARTS, MAX_PARTS_PER_PAGE,
    MAX_XML_BYTES, MIN_PART_SIZE, MULTIPART_PREFIX, OBJECT_CONTENT_TYPE, S3Error, S3Failure,
    S3Result, STREAMING_PAYLOAD, STREAMING_PAYLOAD_TRAILER, STREAMING_UNSIGNED_PAYLOAD_TRAILER,
    SYNTHETIC_OWNER_ID, SYNTHETIC_OWNER_NAME, UNSIGNED_PAYLOAD, XML_CONTENT_TYPE,
    error_with_message, s3_error, s3_error_of, s3_error_of_errno,
};
pub use protocol::S3Response;
pub use server::{S3BindError, S3Server, S3ServerOptions, create_s3_server};
pub use session::{S3Request, S3RequestHead, S3Session, S3SessionOptions, S3SessionStats};
pub use sigv4::{
    AuthorizationHeader, CredentialScope, Credentials, EMPTY_PAYLOAD_SHA256, HEADER_AUTHORIZATION,
    HEADER_CONTENT_SHA256, HEADER_DATE, HeaderEntry, MAX_CLOCK_SKEW_MS, MAX_PRESIGNED_EXPIRES,
    ParsedPresignedQuery, PresignRequest, PresignedRequest, QUERY_ALGORITHM, QUERY_CREDENTIAL,
    QUERY_DATE, QUERY_EXPIRES, QUERY_SIGNATURE, QUERY_SIGNED_HEADERS, QueryEntry, S3_SERVICE,
    SIGV4_ALGORITHM, SIGV4_TERMINATOR, SigV4Credentials, SigV4Failure, SignRequest, VerifyRequest,
    canonical_headers, canonical_query, canonical_request, canonical_signed_headers, canonical_uri,
    credential_scope, format_amz_date, header_list, header_value, is_presigned, parse_amz_date,
    parse_authorization_header, parse_presigned_query, presign_request, sha256_hex, sign_chunk,
    sign_request, sign_trailer, signature_of, signatures_match, signing_key, string_to_sign,
    uri_encode,
};
pub use xml::{
    ListObjectsXml, ListPartsXml, ListedObject, ListedPart, S3_XMLNS, XML_DECLARATION,
    XML_MAX_BYTES, XML_MAX_DEPTH, XML_MAX_DEPTH_CEILING, XML_MAX_ELEMENTS, complete_multipart_xml,
    copy_object_xml, delete_result_xml, error_response, initiate_multipart_xml, list_buckets_xml,
    list_objects_xml, list_parts_xml, parse_complete_document, parse_delete_document, xml_escape,
    xml_response,
};
