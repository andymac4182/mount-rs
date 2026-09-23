//! S3 request routing, key validation, HTTP metadata, XML codecs, and error
//! mapping. This module contains no sockets and no filesystem calls.

#![allow(unexpected_cfgs)]

use std::{cmp::Ordering, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use mount_rs_core::{ErrorCode, FsError};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use thiserror::Error;

use crate::sigv4::{HeaderEntry, QueryEntry, header_list, header_value};

pub const MAX_KEYS: usize = 1000;
pub const MAX_PARTS: u32 = 10_000;
pub const MAX_PARTS_PER_PAGE: usize = 1000;
pub const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;
pub const MAX_PART_SIZE: u64 = 5 * 1024 * 1024 * 1024;
pub const MAX_KEY_BYTES: usize = 1024;
pub const MAX_XML_BYTES: usize = 16 * 1024 * 1024;
pub const MULTIPART_PREFIX: &str = ".mountx-multipart";
pub const STREAMING_STAGING_PREFIX: &str = ".mountx-put-";
pub const OBJECT_CONTENT_TYPE: &str = "application/octet-stream";
pub const XML_CONTENT_TYPE: &str = "application/xml";
pub const SYNTHETIC_OWNER_ID: &str = "mountx-gateway";
pub const SYNTHETIC_OWNER_NAME: &str = "mountx";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error {
    pub code: String,
    pub status: u16,
    pub message: String,
}

impl S3Error {
    pub fn new(code: impl Into<String>, status: u16, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            status,
            message: message.into(),
        }
    }
}

impl fmt::Display for S3Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

pub fn s3_error(code: &str) -> S3Error {
    let (status, message) = match code {
        "AccessDenied" => (403, "Access Denied"),
        "BadDigest" => (
            400,
            "The Content-MD5 you specified did not match what we received.",
        ),
        "AuthorizationHeaderMalformed" => {
            (400, "The authorization header you provided is invalid.")
        }
        "AuthorizationQueryParametersError" => (
            400,
            "Query-string authentication version 4 requires the X-Amz-Algorithm, X-Amz-Credential, X-Amz-Signature, X-Amz-Date, X-Amz-SignedHeaders, and X-Amz-Expires parameters.",
        ),
        "BucketNotEmpty" => (409, "The bucket you tried to delete is not empty."),
        "EntityTooLarge" => (
            400,
            "Your proposed upload exceeds the maximum allowed object size.",
        ),
        "EntityTooSmall" => (
            400,
            "Your proposed upload is smaller than the minimum allowed object size.",
        ),
        "IncompleteBody" => (
            400,
            "You did not provide the number of bytes specified by the Content-Length HTTP header.",
        ),
        "InternalError" => (500, "We encountered an internal error. Please try again."),
        "InvalidAccessKeyId" => (
            403,
            "The AWS access key ID you provided does not exist in our records.",
        ),
        "InvalidArgument" => (400, "Invalid Argument"),
        "InvalidBucketName" => (400, "The specified bucket is not valid."),
        "InvalidPart" => (
            400,
            "One or more of the specified parts could not be found.",
        ),
        "InvalidPartOrder" => (400, "The list of parts was not in ascending order."),
        "InvalidRange" => (416, "The requested range cannot be satisfied."),
        "InvalidRequest" => (400, "Invalid Request"),
        "InvalidURI" => (400, "Couldn't parse the specified URI."),
        "KeyTooLongError" => (400, "Your key is too long."),
        "MalformedXML" => (
            400,
            "The XML you provided was not well-formed or did not validate against our published schema.",
        ),
        "MaxMessageLengthExceeded" => (400, "Your request was too big."),
        "MethodNotAllowed" => (
            405,
            "The specified method is not allowed against this resource.",
        ),
        "MissingContentLength" => (411, "You must provide the Content-Length HTTP header."),
        "NoSuchBucket" => (404, "The specified bucket does not exist."),
        "NoSuchKey" => (404, "The specified key does not exist."),
        "NoSuchUpload" => (
            404,
            "The specified multipart upload does not exist. The upload ID might be invalid, or the multipart upload might have been aborted or completed.",
        ),
        "NotImplemented" => (
            501,
            "A header you provided implies functionality that is not implemented.",
        ),
        "OperationAborted" => (
            409,
            "A conflicting conditional operation is currently in progress against this resource. Try again.",
        ),
        "PreconditionFailed" => (
            412,
            "At least one of the preconditions you specified did not hold.",
        ),
        "RequestTimeTooSkewed" => (
            403,
            "The difference between the request time and the server's time is too large.",
        ),
        "ServiceUnavailable" => (503, "Service is unable to handle request."),
        "SignatureDoesNotMatch" => (
            403,
            "The request signature we calculated does not match the signature you provided. Check your AWS secret access key and signing method.",
        ),
        "SlowDown" => (503, "Reduce your request rate."),
        _ => (500, "We encountered an internal error. Please try again."),
    };
    S3Error::new(code, status, message)
}

pub fn error_with_message(code: &str, message: impl Into<String>) -> S3Error {
    let mut error = s3_error(code);
    error.message = message.into();
    error
}

pub fn s3_error_of(error: &FsError) -> S3Error {
    let code = match error.code {
        ErrorCode::Eperm | ErrorCode::Eacces | ErrorCode::Erofs => "AccessDenied",
        ErrorCode::Enoent | ErrorCode::Enotdir | ErrorCode::Estale => "NoSuchKey",
        ErrorCode::Eagain | ErrorCode::Emfile | ErrorCode::Enfile => "SlowDown",
        ErrorCode::Enomem | ErrorCode::Enospc | ErrorCode::Edquot => "ServiceUnavailable",
        ErrorCode::Ebusy | ErrorCode::Eexist => "OperationAborted",
        ErrorCode::Eisdir => "InvalidRequest",
        ErrorCode::Einval => "InvalidArgument",
        ErrorCode::Efbig => "EntityTooLarge",
        ErrorCode::Enametoolong => "KeyTooLongError",
        ErrorCode::Enosys | ErrorCode::Enotsup => "NotImplemented",
        ErrorCode::Enotempty => "BucketNotEmpty",
        _ => "InternalError",
    };
    s3_error(code)
}

#[derive(Debug, Error)]
pub enum S3Failure {
    #[error("{0}")]
    S3(S3Error),
    #[error("{error}")]
    S3Resource { error: S3Error, resource: String },
    #[error("{0}")]
    Fs(#[from] FsError),
    #[error("{0}")]
    Internal(String),
}

impl S3Failure {
    pub fn s3(code: &str) -> Self {
        Self::S3(s3_error(code))
    }

    pub fn with_resource(self, resource: impl Into<String>) -> Self {
        let resource = resource.into();
        match self {
            Self::S3(error) | Self::S3Resource { error, .. } => {
                Self::S3Resource { error, resource }
            }
            other => other,
        }
    }

    pub fn error(&self) -> S3Error {
        match self {
            Self::S3(error) => error.clone(),
            Self::S3Resource { error, .. } => error.clone(),
            Self::Fs(error) => s3_error_of(error),
            Self::Internal(_) => s3_error("InternalError"),
        }
    }

    pub fn resource(&self) -> Option<&str> {
        match self {
            Self::S3Resource { resource, .. } => Some(resource),
            Self::S3(_) | Self::Fs(_) | Self::Internal(_) => None,
        }
    }
}

impl From<S3Error> for S3Failure {
    fn from(value: S3Error) -> Self {
        Self::S3(value)
    }
}

pub type S3Result<T> = Result<T, S3Failure>;

#[derive(Debug, Clone, Default)]
pub struct S3Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl S3Response {
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }
}

pub fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn error_response(error: &S3Error, request_id: &str, resource: Option<&str>) -> S3Response {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    xml.push_str("<Error>");
    xml.push_str(&format!("<Code>{}</Code>", xml_escape(&error.code)));
    xml.push_str(&format!(
        "<Message>{}</Message>",
        xml_escape(&error.message)
    ));
    if let Some(resource) = resource {
        xml.push_str(&format!("<Resource>{}</Resource>", xml_escape(resource)));
    }
    xml.push_str(&format!(
        "<RequestId>{}</RequestId></Error>",
        xml_escape(request_id)
    ));
    S3Response::empty(error.status)
        .header("content-type", XML_CONTENT_TYPE)
        .header("content-length", xml.len().to_string())
        .header("x-amz-request-id", request_id)
        .with_body(xml.into_bytes())
}

pub fn xml_response(status: u16, xml: String, request_id: &str) -> S3Response {
    let mut response = S3Response::empty(status)
        .header("content-type", XML_CONTENT_TYPE)
        .header("content-length", xml.len().to_string())
        .with_body(xml.into_bytes());
    if !request_id.is_empty() {
        response = response.header("x-amz-request-id", request_id);
    }
    response
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTarget {
    pub path: String,
    pub query: Vec<QueryEntry>,
}

fn decode_component(value: &str, form: bool) -> Option<String> {
    let value = if form {
        value.replace('+', " ")
    } else {
        value.to_owned()
    };
    percent_decode_str(&value)
        .decode_utf8()
        .ok()
        .map(|value| value.into_owned())
}

pub fn parse_request_target(target: &str) -> S3Result<ParsedTarget> {
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    if !raw_path.starts_with('/') {
        return Err(S3Failure::s3("InvalidURI"));
    }
    let path = decode_component(raw_path, false).ok_or_else(|| {
        S3Failure::S3(error_with_message(
            "InvalidURI",
            "Malformed percent-encoding in URI.",
        ))
    })?;
    let mut query = Vec::new();
    for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = decode_component(name, true).ok_or_else(|| S3Failure::s3("InvalidURI"))?;
        let value = decode_component(value, true).ok_or_else(|| S3Failure::s3("InvalidURI"))?;
        query.push(QueryEntry::new(name, value));
    }
    Ok(ParsedTarget { path, query })
}

pub fn query_value(query: &[QueryEntry], name: &str) -> Option<String> {
    query
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.value.clone())
}

pub fn has_query(query: &[QueryEntry], name: &str) -> bool {
    query.iter().any(|entry| entry.name == name)
}

pub fn parse_unsigned(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn checked_part_number_marker(value: u64) -> Option<u32> {
    u32::try_from(value).ok()
}

#[cfg(kani)]
mod verification {
    use super::*;

    // The one symbolic digit spans both sides of u32::MAX without a symbolic
    // string length. The harness checks the production parser and checked
    // conversion for this ten-digit boundary slice only.
    #[kani::proof]
    #[kani::unwind(16)]
    fn part_number_marker_boundary_does_not_wrap() {
        let last_digit: u8 = kani::any();
        kani::assume(last_digit.is_ascii_digit());
        let digits = [
            b'4', b'2', b'9', b'4', b'9', b'6', b'7', b'2', b'9', last_digit,
        ];
        let text = std::str::from_utf8(&digits).expect("ASCII decimal marker");
        let marker = parse_unsigned(text).and_then(checked_part_number_marker);
        kani::cover!(marker.is_some());
        kani::cover!(marker.is_none());
        assert_eq!(marker.is_some(), last_digit <= b'5');
        if let Some(marker) = marker {
            assert_eq!(
                u64::from(marker),
                4_294_967_290 + u64::from(last_digit - b'0')
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTarget {
    pub bucket: String,
    pub key: String,
    pub path: String,
    pub directory: bool,
}

pub fn is_staging_key(key: &str) -> bool {
    key == MULTIPART_PREFIX
        || key.starts_with(&format!("{MULTIPART_PREFIX}/"))
        || key.starts_with(STREAMING_STAGING_PREFIX)
}

pub fn parse_object_key(bucket: &str, key: &str) -> S3Result<ObjectTarget> {
    if key.is_empty() {
        return Err(S3Failure::S3(error_with_message(
            "InvalidArgument",
            "The object key must not be empty.",
        )));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(S3Failure::s3("KeyTooLongError"));
    }
    if key.contains('\0') {
        return Err(S3Failure::s3("InvalidArgument"));
    }
    let directory = key.ends_with('/');
    let components = if directory {
        &key[..key.len() - 1]
    } else {
        key
    };
    let parts = components.split('/').collect::<Vec<_>>();
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(S3Failure::s3("InvalidArgument"));
    }
    if is_staging_key(key) {
        return Err(S3Failure::s3("NoSuchKey"));
    }
    Ok(ObjectTarget {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
        path: format!("/{}", parts.join("/")),
        directory,
    })
}

pub fn is_valid_bucket_name(bucket: &str) -> bool {
    !bucket.is_empty()
        && bucket != "."
        && bucket != ".."
        && bucket.len() <= 255
        && bucket
            .chars()
            .all(|character| !character.is_control() && character != '/' && character != '\\')
}

#[derive(Debug, Clone)]
pub enum Operation {
    ListBuckets,
    HeadBucket {
        bucket: String,
    },
    ListObjectsV2 {
        bucket: String,
        prefix: String,
        delimiter: Option<String>,
        max_keys: usize,
        continuation_token: Option<String>,
        start_after: Option<String>,
        fetch_owner: bool,
        encoding_type_url: bool,
    },
    GetObject(ObjectTarget),
    HeadObject(ObjectTarget),
    PutObject(ObjectTarget),
    DeleteObject(ObjectTarget),
    DeleteObjects {
        bucket: String,
    },
    CopyObject {
        destination: ObjectTarget,
        source: ObjectTarget,
        replace_metadata: bool,
    },
    CreateMultipartUpload(ObjectTarget),
    UploadPart {
        target: ObjectTarget,
        upload_id: String,
        part_number: u32,
    },
    CompleteMultipartUpload {
        target: ObjectTarget,
        upload_id: String,
    },
    AbortMultipartUpload {
        target: ObjectTarget,
        upload_id: String,
    },
    ListParts {
        target: ObjectTarget,
        upload_id: String,
        max_parts: usize,
        marker: u32,
    },
}

fn unsupported_query(name: &str) -> bool {
    matches!(
        name,
        "acl"
            | "cors"
            | "encryption"
            | "legal-hold"
            | "lifecycle"
            | "location"
            | "logging"
            | "notification"
            | "object-lock"
            | "policy"
            | "policyStatus"
            | "publicAccessBlock"
            | "replication"
            | "tagging"
            | "torrent"
            | "versioning"
            | "versions"
            | "website"
            | "select"
            | "select-type"
            | "restore"
            | "retention"
            | "requestPayment"
            | "analytics"
            | "inventory"
            | "metrics"
            | "accelerate"
            | "renameObject"
            | "session"
    )
}

fn method_not_allowed(method: &str, resource: &str) -> S3Failure {
    S3Failure::S3(error_with_message(
        "MethodNotAllowed",
        format!("The method {method} is not allowed against this resource."),
    ))
    .with_resource(resource)
}

fn staging_answer(method: &str) -> S3Result<Operation> {
    if method.eq_ignore_ascii_case("DELETE") {
        return Ok(Operation::DeleteObject(ObjectTarget {
            bucket: String::new(),
            key: String::new(),
            path: String::new(),
            directory: false,
        }));
    }
    Err(S3Failure::s3("NoSuchKey"))
}

fn parse_copy_source(value: &str) -> S3Result<(String, String)> {
    let (raw_path, raw_query) = value.split_once('?').unwrap_or((value, ""));
    for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = decode_component(name, true).ok_or_else(|| S3Failure::s3("InvalidURI"))?;
        let value = decode_component(value, true).ok_or_else(|| S3Failure::s3("InvalidURI"))?;
        if name == "versionId" && !value.is_empty() && value != "null" {
            return Err(S3Failure::s3("NotImplemented"));
        }
    }
    let path = decode_component(raw_path, false).ok_or_else(|| S3Failure::s3("InvalidURI"))?;
    let path = path.strip_prefix('/').unwrap_or(&path);
    let (bucket, key) = path
        .split_once('/')
        .ok_or_else(|| S3Failure::s3("InvalidURI"))?;
    if !is_valid_bucket_name(bucket) || key.is_empty() {
        return Err(S3Failure::s3("InvalidURI"));
    }
    Ok((bucket.to_owned(), key.to_owned()))
}

pub fn route_request(
    method: &str,
    target: &ParsedTarget,
    headers: &[HeaderEntry],
) -> S3Result<Operation> {
    let method = method.to_ascii_uppercase();
    for entry in &target.query {
        if unsupported_query(&entry.name) {
            return Err(S3Failure::s3("NotImplemented"));
        }
    }
    if let Some(version) = query_value(&target.query, "versionId")
        && !version.is_empty()
        && version != "null"
    {
        return Err(S3Failure::s3("NotImplemented"));
    }
    let without_leading = target.path.strip_prefix('/').unwrap_or_default();
    let (bucket, raw_key) = without_leading
        .split_once('/')
        .map_or((without_leading, ""), |(bucket, key)| (bucket, key));
    if bucket.is_empty() {
        if !raw_key.is_empty() {
            return Err(S3Failure::s3("InvalidBucketName"));
        }
        return if method == "GET" {
            Ok(Operation::ListBuckets)
        } else {
            Err(method_not_allowed(&method, &target.path))
        };
    }
    if !is_valid_bucket_name(bucket) {
        return Err(S3Failure::s3("InvalidBucketName"));
    }
    let uploads = has_query(&target.query, "uploads");
    let upload_id = query_value(&target.query, "uploadId");
    let delete = has_query(&target.query, "delete");
    let copy_source = header_value(headers, "x-amz-copy-source");
    if raw_key.is_empty() {
        if uploads {
            return if method == "GET" {
                Err(S3Failure::s3("NotImplemented"))
            } else {
                Err(method_not_allowed(&method, &target.path))
            };
        }
        if upload_id.is_some() {
            return Err(method_not_allowed(&method, &target.path));
        }
        return match method.as_str() {
            "GET" => {
                let list_type = query_value(&target.query, "list-type");
                match list_type.as_deref() {
                    None => {
                        return Err(S3Failure::S3(error_with_message(
                            "NotImplemented",
                            "ListObjects (V1) is not implemented by this gateway.",
                        )));
                    }
                    Some("2") => {}
                    Some(value) => {
                        return Err(S3Failure::S3(error_with_message(
                            "InvalidArgument",
                            format!("Invalid List Type: {value}"),
                        )));
                    }
                }
                let max_keys = match query_value(&target.query, "max-keys") {
                    None => MAX_KEYS,
                    Some(value) if value.is_empty() => MAX_KEYS,
                    Some(value) => {
                        let value = parse_unsigned(&value)
                            .filter(|value| *value <= 2_147_483_647)
                            .ok_or_else(|| S3Failure::s3("InvalidArgument"))?;
                        value.min(MAX_KEYS as u64) as usize
                    }
                };
                let delimiter = match query_value(&target.query, "delimiter") {
                    None => None,
                    Some(value) if value.is_empty() => None,
                    Some(value) if value == "/" => Some(value),
                    Some(_) => {
                        return Err(S3Failure::S3(error_with_message(
                            "NotImplemented",
                            "A delimiter other than \"/\" is not implemented by this gateway.",
                        )));
                    }
                };
                let encoding_type_url = match query_value(&target.query, "encoding-type") {
                    None => false,
                    Some(value) if value.is_empty() => false,
                    Some(value) if value == "url" => true,
                    Some(_) => return Err(S3Failure::s3("InvalidArgument")),
                };
                let fetch_owner = query_value(&target.query, "fetch-owner")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"));
                Ok(Operation::ListObjectsV2 {
                    bucket: bucket.to_owned(),
                    prefix: query_value(&target.query, "prefix").unwrap_or_default(),
                    delimiter,
                    max_keys,
                    continuation_token: query_value(&target.query, "continuation-token")
                        .filter(|value| !value.is_empty()),
                    start_after: query_value(&target.query, "start-after")
                        .filter(|value| !value.is_empty()),
                    fetch_owner,
                    encoding_type_url,
                })
            }
            "HEAD" => Ok(Operation::HeadBucket {
                bucket: bucket.to_owned(),
            }),
            "POST" if delete => Ok(Operation::DeleteObjects {
                bucket: bucket.to_owned(),
            }),
            "POST" => Err(S3Failure::s3("NotImplemented")),
            "PUT" | "DELETE" => Err(S3Failure::s3("NotImplemented")),
            _ => Err(method_not_allowed(&method, &target.path)),
        };
    }
    if is_staging_key(raw_key) {
        return staging_answer(&method);
    }
    let destination = parse_object_key(bucket, raw_key)?;
    if delete && method == "POST" {
        return Err(method_not_allowed(&method, &target.path));
    }
    if uploads {
        return if method == "POST" {
            Ok(Operation::CreateMultipartUpload(destination))
        } else {
            Err(S3Failure::s3("NotImplemented"))
        };
    }
    if let Some(upload_id) = upload_id {
        if upload_id.is_empty() {
            return Err(S3Failure::s3("InvalidArgument"));
        }
        return match method.as_str() {
            "PUT" => {
                if copy_source.is_some() {
                    return Err(S3Failure::s3("NotImplemented"));
                }
                let part_number = query_value(&target.query, "partNumber")
                    .and_then(|value| parse_unsigned(&value))
                    .filter(|number| (1..=MAX_PARTS as u64).contains(number))
                    .ok_or_else(|| S3Failure::s3("InvalidArgument"))?
                    as u32;
                Ok(Operation::UploadPart {
                    target: destination,
                    upload_id,
                    part_number,
                })
            }
            "POST" => Ok(Operation::CompleteMultipartUpload {
                target: destination,
                upload_id,
            }),
            "DELETE" => Ok(Operation::AbortMultipartUpload {
                target: destination,
                upload_id,
            }),
            "GET" => {
                let max_parts = query_value(&target.query, "max-parts")
                    .map(|value| {
                        parse_unsigned(&value).ok_or_else(|| S3Failure::s3("InvalidArgument"))
                    })
                    .transpose()?
                    .unwrap_or(MAX_PARTS_PER_PAGE as u64)
                    .min(MAX_PARTS_PER_PAGE as u64) as usize;
                let marker = query_value(&target.query, "part-number-marker")
                    .map(|value| {
                        parse_unsigned(&value)
                            .and_then(checked_part_number_marker)
                            .ok_or_else(|| S3Failure::s3("InvalidArgument"))
                    })
                    .transpose()?
                    .unwrap_or(0);
                Ok(Operation::ListParts {
                    target: destination,
                    upload_id,
                    max_parts,
                    marker,
                })
            }
            _ => Err(method_not_allowed(&method, &target.path)),
        };
    }
    if (method == "GET" || method == "HEAD") && has_query(&target.query, "partNumber") {
        let operation = if method == "GET" {
            "GetObject for a single part"
        } else {
            "HeadObject for a single part"
        };
        return Err(S3Failure::S3(error_with_message(
            "NotImplemented",
            format!("{operation} is not implemented by this gateway."),
        )));
    }
    match method.as_str() {
        "GET" => Ok(Operation::GetObject(destination)),
        "HEAD" => Ok(Operation::HeadObject(destination)),
        "DELETE" => Ok(Operation::DeleteObject(destination)),
        "PUT" if copy_source.is_none() => Ok(Operation::PutObject(destination)),
        "PUT" => {
            let source = parse_copy_source(&copy_source.unwrap())?;
            let source = parse_object_key(&source.0, &source.1)?;
            let replace_metadata = header_value(headers, "x-amz-metadata-directive")
                .is_some_and(|value| value == "REPLACE");
            if let Some(value) = header_value(headers, "x-amz-metadata-directive")
                && value != "COPY"
                && value != "REPLACE"
            {
                return Err(S3Failure::s3("InvalidArgument"));
            }
            Ok(Operation::CopyObject {
                destination,
                source,
                replace_metadata,
            })
        }
        _ => Err(method_not_allowed(&method, &target.path)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

pub fn parse_range(value: &str, size: u64) -> Option<ByteRange> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || size == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = parse_unsigned(end)?;
        if suffix == 0 {
            return None;
        }
        let length = suffix.min(size);
        return Some(ByteRange {
            start: size - length,
            end: size - 1,
        });
    }
    let start = parse_unsigned(start)?;
    if start >= size {
        return None;
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        parse_unsigned(end)?.min(size - 1)
    };
    (start <= end).then_some(ByteRange { start, end })
}

pub fn format_http_date(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch"))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

pub fn parse_http_date(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.timestamp_millis())
        .or_else(|| {
            DateTime::parse_from_str(value, "%a, %d %b %Y %H:%M:%S GMT")
                .ok()
                .map(|date| date.timestamp_millis())
        })
}

pub fn etag_header(tag: &str) -> String {
    format!("\"{tag}\"")
}

pub fn compare_utf8(left: &str, right: &str) -> Ordering {
    left.as_bytes().cmp(right.as_bytes())
}

pub fn encode_continuation_token(key: &str) -> String {
    BASE64.encode(key.as_bytes())
}

pub fn decode_continuation_token(token: &str) -> Option<String> {
    let bytes = BASE64.decode(token).ok()?;
    let key = String::from_utf8(bytes).ok()?;
    (encode_continuation_token(&key) == token).then_some(key)
}

pub fn parse_meta_mtime(value: Option<&str>) -> Option<i64> {
    let value = value?.parse::<f64>().ok()?;
    if !value.is_finite() || value.abs() > 8_640_000_000_000.0 {
        return None;
    }
    Some((value * 1000.0).round() as i64)
}

pub fn format_meta_mtime(timestamp_ms: i64) -> String {
    if timestamp_ms % 1000 == 0 {
        (timestamp_ms / 1000).to_string()
    } else {
        format!("{:.3}", timestamp_ms as f64 / 1000.0)
    }
}

pub fn object_headers(
    etag: &str,
    size: u64,
    mtime_ms: i64,
    range: Option<ByteRange>,
    total: u64,
) -> Vec<(String, String)> {
    let reply_size = range.map_or(size, |range| range.end - range.start + 1);
    let mut headers = vec![
        ("content-type".to_owned(), OBJECT_CONTENT_TYPE.to_owned()),
        ("content-length".to_owned(), reply_size.to_string()),
        ("etag".to_owned(), etag_header(etag)),
        ("last-modified".to_owned(), format_http_date(mtime_ms)),
        ("accept-ranges".to_owned(), "bytes".to_owned()),
        ("x-amz-meta-mtime".to_owned(), format_meta_mtime(mtime_ms)),
    ];
    if let Some(range) = range {
        headers.push((
            "content-range".to_owned(),
            format!("bytes {}-{}/{}", range.start, range.end, total),
        ));
    }
    headers
}

fn xml_header() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_owned()
}

#[derive(Debug, Clone)]
pub struct ListedObject {
    pub key: String,
    pub last_modified: String,
    pub etag: String,
    pub size: u64,
    pub owner: bool,
}

pub fn list_buckets_xml(buckets: &[(String, String)]) -> String {
    let mut xml = xml_header();
    xml.push_str(&format!("<ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Owner><ID>{}</ID><DisplayName>{}</DisplayName></Owner><Buckets>", SYNTHETIC_OWNER_ID, SYNTHETIC_OWNER_NAME));
    for (name, date) in buckets {
        xml.push_str(&format!(
            "<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>",
            xml_escape(name),
            xml_escape(date)
        ));
    }
    xml.push_str("</Buckets></ListAllMyBucketsResult>");
    xml
}

pub struct ListObjectsXml<'a> {
    pub bucket: &'a str,
    pub prefix: &'a str,
    pub delimiter: Option<&'a str>,
    pub continuation: Option<&'a str>,
    pub next: Option<&'a str>,
    pub start_after: Option<&'a str>,
    pub max_keys: usize,
    pub key_count: usize,
    pub truncated: bool,
    pub encoding_url: bool,
    pub objects: &'a [ListedObject],
    pub common_prefixes: &'a [String],
}

pub fn list_objects_xml(input: ListObjectsXml<'_>) -> String {
    let ListObjectsXml {
        bucket,
        prefix,
        delimiter,
        continuation,
        next,
        start_after,
        max_keys,
        key_count,
        truncated,
        encoding_url,
        objects,
        common_prefixes,
    } = input;
    let encode = |value: &str| {
        if encoding_url {
            crate::sigv4::uri_encode(value)
        } else {
            value.to_owned()
        }
    };
    let mut xml = xml_header();
    xml.push_str("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    xml.push_str(&format!(
        "<Name>{}</Name><Prefix>{}</Prefix>",
        xml_escape(bucket),
        xml_escape(&encode(prefix))
    ));
    if let Some(value) = continuation {
        xml.push_str(&format!(
            "<ContinuationToken>{}</ContinuationToken>",
            xml_escape(value)
        ));
    }
    if let Some(value) = next {
        xml.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            xml_escape(value)
        ));
    }
    if let Some(value) = start_after {
        xml.push_str(&format!(
            "<StartAfter>{}</StartAfter>",
            xml_escape(&encode(value))
        ));
    }
    xml.push_str(&format!(
        "<KeyCount>{key_count}</KeyCount><MaxKeys>{max_keys}</MaxKeys>"
    ));
    if let Some(delimiter) = delimiter {
        xml.push_str(&format!(
            "<Delimiter>{}</Delimiter>",
            xml_escape(&encode(delimiter))
        ));
    }
    if encoding_url {
        xml.push_str("<EncodingType>url</EncodingType>");
    }
    xml.push_str(&format!(
        "<IsTruncated>{}</IsTruncated>",
        if truncated { "true" } else { "false" }
    ));
    for object in objects {
        xml.push_str(&format!("<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass>", xml_escape(&encode(&object.key)), xml_escape(&object.last_modified), xml_escape(&etag_header(&object.etag)), object.size));
        if object.owner {
            xml.push_str(&format!(
                "<Owner><ID>{}</ID><DisplayName>{}</DisplayName></Owner>",
                SYNTHETIC_OWNER_ID, SYNTHETIC_OWNER_NAME
            ));
        }
        xml.push_str("</Contents>");
    }
    for prefix_value in common_prefixes {
        xml.push_str(&format!(
            "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            xml_escape(&encode(prefix_value))
        ));
    }
    xml.push_str("</ListBucketResult>");
    xml
}

pub fn copy_object_xml(etag: &str, modified: &str) -> String {
    format!(
        "{}<CopyObjectResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><LastModified>{}</LastModified><ETag>{}</ETag></CopyObjectResult>",
        xml_header(),
        xml_escape(modified),
        xml_escape(&etag_header(etag))
    )
}

pub fn delete_result_xml(deleted: &[String], errors: &[(String, S3Error)], quiet: bool) -> String {
    let mut xml = format!(
        "{}<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
        xml_header()
    );
    if !quiet {
        for key in deleted {
            xml.push_str(&format!(
                "<Deleted><Key>{}</Key></Deleted>",
                xml_escape(key)
            ));
        }
    }
    for (key, error) in errors {
        xml.push_str(&format!(
            "<Error><Key>{}</Key><Code>{}</Code><Message>{}</Message></Error>",
            xml_escape(key),
            xml_escape(&error.code),
            xml_escape(&error.message)
        ));
    }
    xml.push_str("</DeleteResult>");
    xml
}

pub fn initiate_multipart_xml(bucket: &str, key: &str, upload_id: &str) -> String {
    format!(
        "{}<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><Key>{}</Key><UploadId>{}</UploadId></InitiateMultipartUploadResult>",
        xml_header(),
        xml_escape(bucket),
        xml_escape(key),
        xml_escape(upload_id)
    )
}

pub fn complete_multipart_xml(location: &str, bucket: &str, key: &str, etag: &str) -> String {
    format!(
        "{}<CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Location>{}</Location><Bucket>{}</Bucket><Key>{}</Key><ETag>{}</ETag></CompleteMultipartUploadResult>",
        xml_header(),
        xml_escape(location),
        xml_escape(bucket),
        xml_escape(key),
        xml_escape(&etag_header(etag))
    )
}

#[derive(Debug, Clone)]
pub struct ListedPart {
    pub number: u32,
    pub modified: String,
    pub etag: String,
    pub size: u64,
}

pub struct ListPartsXml<'a> {
    pub bucket: &'a str,
    pub key: &'a str,
    pub upload_id: &'a str,
    pub marker: u32,
    pub max_parts: usize,
    pub truncated: bool,
    pub next_marker: Option<u32>,
    pub parts: &'a [ListedPart],
}

pub fn list_parts_xml(input: ListPartsXml<'_>) -> String {
    let ListPartsXml {
        bucket,
        key,
        upload_id,
        marker,
        max_parts,
        truncated,
        next_marker,
        parts,
    } = input;
    let mut xml = format!(
        "{}<ListPartsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><Key>{}</Key><UploadId>{}</UploadId><Initiator><ID>{}</ID><DisplayName>{}</DisplayName></Initiator><Owner><ID>{}</ID><DisplayName>{}</DisplayName></Owner><StorageClass>STANDARD</StorageClass><PartNumberMarker>{marker}</PartNumberMarker>",
        xml_header(),
        xml_escape(bucket),
        xml_escape(key),
        xml_escape(upload_id),
        SYNTHETIC_OWNER_ID,
        SYNTHETIC_OWNER_NAME,
        SYNTHETIC_OWNER_ID,
        SYNTHETIC_OWNER_NAME
    );
    if let Some(marker) = next_marker {
        xml.push_str(&format!(
            "<NextPartNumberMarker>{marker}</NextPartNumberMarker>"
        ));
    }
    xml.push_str(&format!(
        "<MaxParts>{max_parts}</MaxParts><IsTruncated>{}</IsTruncated>",
        if truncated { "true" } else { "false" }
    ));
    for part in parts {
        xml.push_str(&format!("<Part><PartNumber>{}</PartNumber><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size></Part>", part.number, xml_escape(&part.modified), xml_escape(&etag_header(&part.etag)), part.size));
    }
    xml.push_str("</ListPartsResult>");
    xml
}

#[derive(Debug, Deserialize)]
struct DeleteDocument {
    #[serde(rename = "Object", default)]
    objects: Vec<DeleteDocumentObject>,
    #[serde(rename = "Quiet", default)]
    quiet: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct DeleteDocumentObject {
    #[serde(rename = "Key")]
    key: String,
}

pub fn parse_delete_document(bytes: &[u8], max_xml_bytes: usize) -> S3Result<(Vec<String>, bool)> {
    if bytes.len() > max_xml_bytes.min(MAX_XML_BYTES) {
        return Err(S3Failure::s3("MaxMessageLengthExceeded"));
    }
    let document: DeleteDocument =
        quick_xml::de::from_reader(bytes).map_err(|_| S3Failure::s3("MalformedXML"))?;
    if document.objects.is_empty() || document.objects.len() > MAX_KEYS {
        return Err(S3Failure::s3("MalformedXML"));
    }
    Ok((
        document
            .objects
            .into_iter()
            .map(|object| object.key)
            .collect(),
        document.quiet.unwrap_or(false),
    ))
}

#[derive(Debug, Deserialize)]
struct CompleteDocument {
    #[serde(rename = "Part", default)]
    parts: Vec<CompleteDocumentPart>,
}

#[derive(Debug, Deserialize)]
struct CompleteDocumentPart {
    #[serde(rename = "PartNumber")]
    number: u32,
    #[serde(rename = "ETag")]
    etag: String,
}

pub fn parse_complete_document(bytes: &[u8], max_xml_bytes: usize) -> S3Result<Vec<(u32, String)>> {
    if bytes.len() > max_xml_bytes.min(MAX_XML_BYTES) {
        return Err(S3Failure::s3("MaxMessageLengthExceeded"));
    }
    let document: CompleteDocument =
        quick_xml::de::from_reader(bytes).map_err(|_| S3Failure::s3("MalformedXML"))?;
    if document.parts.is_empty() {
        return Err(S3Failure::s3("MalformedXML"));
    }
    Ok(document
        .parts
        .into_iter()
        .map(|part| (part.number, part.etag))
        .collect())
}

pub fn unquote_etag(value: &str) -> String {
    value.trim().trim_matches('"').to_owned()
}

pub fn conditional_match(value: &str, etag: &str) -> bool {
    let tag = etag_header(etag);
    value
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate == tag || candidate == etag)
}

pub fn header_md5(headers: &[HeaderEntry]) -> Option<String> {
    header_value(headers, "content-md5")
}

pub fn content_encoding_chunked(headers: &[HeaderEntry]) -> bool {
    header_list(headers, "content-encoding").is_some_and(|value| {
        value
            .split(',')
            .any(|item| item.trim().eq_ignore_ascii_case("aws-chunked"))
    })
}
