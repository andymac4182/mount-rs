//! Public, bounded S3 XML helpers.
//!
//! These are the S3-specific encoders and request-body parsers already used by
//! the session. The generic XML tree/parser is intentionally not exposed here:
//! it is an implementation detail, and publishing it would make its broader
//! grammar and resource contract part of this crate's API.

/// The namespace used by S3 result documents.
pub const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The XML declaration emitted by the existing S3 response builders.
pub const XML_DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>";

/// Upstream's default bounded XML document size.
pub const XML_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Upstream's default XML nesting limit.
pub const XML_MAX_DEPTH: usize = 32;

/// Hard ceiling for a caller-supplied XML nesting limit.
pub const XML_MAX_DEPTH_CEILING: usize = 256;

/// Upstream's default XML element-count limit.
pub const XML_MAX_ELEMENTS: usize = 100_000;

pub use crate::protocol::{
    ListObjectsXml, ListPartsXml, ListedObject, ListedPart, S3Error, S3Failure, S3Response,
    S3Result, complete_multipart_xml, copy_object_xml, delete_result_xml, error_response,
    initiate_multipart_xml, list_buckets_xml, list_objects_xml, list_parts_xml,
    parse_complete_document, parse_delete_document, xml_escape, xml_response,
};
