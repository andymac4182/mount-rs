//! AWS Signature Version 4 primitives used by the S3 gateway.
//!
//! The implementation follows the AWS signing specification: S3 canonicalizes
//! its URI once, preserves path separators, sorts encoded query pairs, folds
//! repeated signed headers in arrival order, and derives the four-stage HMAC
//! signing key.  All request parsing is kept independent of the HTTP server so
//! the verifier can be tested with in-process requests.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, NaiveDateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub const S3_SERVICE: &str = "s3";
pub const SIGV4_TERMINATOR: &str = "aws4_request";
pub const MAX_CLOCK_SKEW_MS: i64 = 15 * 60 * 1000;
pub const MAX_PRESIGNED_EXPIRES: i64 = 7 * 24 * 60 * 60;
pub const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
pub const STREAMING_PAYLOAD: &str = "STREAMING-AWS4-HMAC-SHA256-PAYLOAD";
pub const STREAMING_PAYLOAD_TRAILER: &str = "STREAMING-AWS4-HMAC-SHA256-PAYLOAD-TRAILER";
pub const STREAMING_UNSIGNED_PAYLOAD_TRAILER: &str = "STREAMING-UNSIGNED-PAYLOAD-TRAILER";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl Credentials {
    pub fn new(access_key_id: impl Into<String>, secret_access_key: impl Into<String>) -> Self {
        Self {
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

impl HeaderEntry {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryEntry {
    pub name: String,
    pub value: String,
}

impl QueryEntry {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialScope {
    pub date: String,
    pub region: String,
    pub service: String,
}

impl CredentialScope {
    pub fn as_string(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.date, self.region, self.service, SIGV4_TERMINATOR
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigV4Failure {
    Missing,
    Malformed(String),
    UnsupportedAlgorithm,
    UnknownAccessKey,
    ScopeMismatch(String),
    ClockSkew,
    Expired,
    SignatureMismatch,
}

impl SigV4Failure {
    pub fn is_presigned(&self) -> bool {
        matches!(
            self,
            Self::Expired | Self::Malformed(_) | Self::ScopeMismatch(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRequest {
    pub scope: CredentialScope,
    pub timestamp_ms: i64,
    pub signature: String,
    pub signed_headers: Vec<String>,
    pub presigned: bool,
    pub payload_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedRequest {
    pub query: Vec<QueryEntry>,
    pub scope: CredentialScope,
    pub signed_headers: Vec<String>,
    pub amz_date: String,
    pub signature: String,
}

fn hmac_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts every key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    let mut digest = Sha256::new();
    digest.update(data.as_ref());
    sha256_hex_digest(digest.finalize())
}

pub(crate) fn sha256_hex_digest(digest: impl AsRef<[u8]>) -> String {
    hex_lower(digest.as_ref())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_upper(byte: u8) -> [char; 3] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    [
        '%',
        HEX[(byte >> 4) as usize] as char,
        HEX[(byte & 0x0f) as usize] as char,
    ]
}

fn is_unreserved(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~')
}

/// RFC 3986 encoding used by SigV4.  Slash is encoded here; `canonical_uri`
/// splits path segments before calling it so separators remain separators.
pub fn uri_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if is_unreserved(*byte) {
            output.push(*byte as char);
        } else {
            output.extend(hex_upper(*byte));
        }
    }
    output
}

pub fn canonical_uri(path: &str) -> String {
    let absolute = if path.is_empty() {
        "/"
    } else if path.starts_with('/') {
        path
    } else {
        // Keep this owned only for the unusual relative caller case.
        return format!(
            "/{}",
            path.split('/')
                .map(uri_encode)
                .collect::<Vec<_>>()
                .join("/")
        );
    };
    absolute
        .split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/")
}

pub fn canonical_query(query: &[QueryEntry]) -> String {
    let mut encoded = query
        .iter()
        .map(|entry| (uri_encode(&entry.name), uri_encode(&entry.value)))
        .collect::<Vec<_>>();
    encoded.sort();
    encoded
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn canonical_signed_headers(names: &[String]) -> Vec<String> {
    let mut names = names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn normalize_header_value(value: &str) -> String {
    let mut output = String::new();
    let mut pending_space = false;
    for byte in value.bytes() {
        if byte == b' ' || byte == b'\t' {
            pending_space = true;
        } else {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            output.push(byte as char);
        }
    }
    output
}

pub fn canonical_headers(headers: &[HeaderEntry], signed_headers: &[String]) -> String {
    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for header in headers {
        values
            .entry(header.name.to_ascii_lowercase())
            .or_default()
            .push(normalize_header_value(&header.value));
    }
    let mut output = String::new();
    for name in canonical_signed_headers(signed_headers) {
        output.push_str(&name);
        output.push(':');
        if let Some(items) = values.get(&name) {
            output.push_str(&items.join(","));
        }
        output.push('\n');
    }
    output
}

pub fn canonical_request(
    method: &str,
    path: &str,
    query: &[QueryEntry],
    headers: &[HeaderEntry],
    signed_headers: &[String],
    payload_hash: &str,
) -> String {
    let signed = canonical_signed_headers(signed_headers);
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        canonical_uri(path),
        canonical_query(query),
        canonical_headers(headers, &signed),
        signed.join(";"),
        payload_hash
    )
}

pub fn string_to_sign(amz_date: &str, scope: &CredentialScope, canonical: &str) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        SIGV4_ALGORITHM,
        amz_date,
        scope.as_string(),
        sha256_hex(canonical.as_bytes())
    )
}

pub fn signing_key(secret: &str, scope: &CredentialScope) -> Vec<u8> {
    let date = hmac_bytes(format!("AWS4{secret}").as_bytes(), scope.date.as_bytes());
    let region = hmac_bytes(&date, scope.region.as_bytes());
    let service = hmac_bytes(&region, scope.service.as_bytes());
    hmac_bytes(&service, SIGV4_TERMINATOR.as_bytes())
}

/// Sign one `aws-chunked` payload in a streaming SigV4 signature chain.
pub fn sign_chunk(
    secret: &str,
    scope: &CredentialScope,
    amz_date: &str,
    previous_signature: &str,
    payload_hash: &str,
) -> String {
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256-PAYLOAD\n{amz_date}\n{}\n{previous_signature}\n{}\n{payload_hash}",
        scope.as_string(),
        EMPTY_PAYLOAD_SHA256,
    );
    hex_lower(&hmac_bytes(
        &signing_key(secret, scope),
        string_to_sign.as_bytes(),
    ))
}

/// Sign the trailing-header block in an `aws-chunked` SigV4 chain.
pub fn sign_trailer(
    secret: &str,
    scope: &CredentialScope,
    amz_date: &str,
    previous_signature: &str,
    trailer_hash: &str,
) -> String {
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256-TRAILER\n{amz_date}\n{}\n{previous_signature}\n{trailer_hash}",
        scope.as_string(),
    );
    hex_lower(&hmac_bytes(
        &signing_key(secret, scope),
        string_to_sign.as_bytes(),
    ))
}

pub fn signature_of(
    secret: &str,
    scope: &CredentialScope,
    amz_date: &str,
    canonical: &str,
) -> String {
    let to_sign = string_to_sign(amz_date, scope, canonical);
    hex_lower(&hmac_bytes(&signing_key(secret, scope), to_sign.as_bytes()))
}

pub fn format_amz_date(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch"))
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

pub fn parse_amz_date(value: &str) -> Option<i64> {
    NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|date| date.and_utc().timestamp_millis())
}

fn header_values<'a>(headers: &'a [HeaderEntry], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
        .collect()
}

pub fn header_value(headers: &[HeaderEntry], name: &str) -> Option<String> {
    header_values(headers, name)
        .into_iter()
        .next()
        .map(str::to_owned)
}

pub fn header_list(headers: &[HeaderEntry], name: &str) -> Option<String> {
    let values = header_values(headers, name);
    (!values.is_empty()).then(|| values.join(", "))
}

fn parse_scope(value: &str) -> Option<CredentialScope> {
    let mut parts = value.split('/');
    let scope = CredentialScope {
        date: parts.next()?.to_owned(),
        region: parts.next()?.to_owned(),
        service: parts.next()?.to_owned(),
    };
    if parts.next()? != SIGV4_TERMINATOR || parts.next().is_some() {
        return None;
    }
    if scope.date.len() != 8
        || !scope.date.bytes().all(|byte| byte.is_ascii_digit())
        || scope.region.is_empty()
        || scope.service.is_empty()
    {
        return None;
    }
    Some(scope)
}

#[derive(Debug)]
struct ParsedAuthorization {
    scope: CredentialScope,
    access_key_id: String,
    signed_headers: Vec<String>,
    signature: String,
}

fn parse_authorization(value: &str) -> Result<ParsedAuthorization, SigV4Failure> {
    let (algorithm, rest) = value
        .split_once(' ')
        .ok_or_else(|| SigV4Failure::Malformed("missing authorization fields".to_owned()))?;
    if algorithm != SIGV4_ALGORITHM {
        return Err(SigV4Failure::UnsupportedAlgorithm);
    }
    let mut values = HashMap::new();
    for part in rest.split(',') {
        let (name, value) = part
            .trim()
            .split_once('=')
            .ok_or_else(|| SigV4Failure::Malformed("invalid authorization pair".to_owned()))?;
        values.insert(name, value);
    }
    let credential = values
        .get("Credential")
        .ok_or_else(|| SigV4Failure::Malformed("missing Credential".to_owned()))?;
    let mut credential_parts = credential.splitn(2, '/');
    let access_key_id = credential_parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SigV4Failure::Malformed("missing access key".to_owned()))?;
    let scope = parse_scope(
        credential_parts
            .next()
            .ok_or_else(|| SigV4Failure::Malformed("missing credential scope".to_owned()))?,
    )
    .ok_or_else(|| SigV4Failure::Malformed("invalid credential scope".to_owned()))?;
    let signed_headers = values
        .get("SignedHeaders")
        .ok_or_else(|| SigV4Failure::Malformed("missing SignedHeaders".to_owned()))?
        .split(';')
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if signed_headers.is_empty() {
        return Err(SigV4Failure::Malformed("empty SignedHeaders".to_owned()));
    }
    let signature = values
        .get("Signature")
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| SigV4Failure::Malformed("invalid signature".to_owned()))?;
    Ok(ParsedAuthorization {
        scope,
        access_key_id: access_key_id.to_owned(),
        signed_headers,
        signature: (*signature).to_owned(),
    })
}

fn query_value(query: &[QueryEntry], name: &str) -> Option<String> {
    query
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(name))
        .map(|entry| entry.value.clone())
}

fn has_query(query: &[QueryEntry], name: &str) -> bool {
    query
        .iter()
        .any(|entry| entry.name.eq_ignore_ascii_case(name))
}

fn parse_presigned(
    query: &[QueryEntry],
) -> Result<(ParsedAuthorization, String, i64), SigV4Failure> {
    let algorithm = query_value(query, "X-Amz-Algorithm").ok_or(SigV4Failure::Missing)?;
    if algorithm != SIGV4_ALGORITHM {
        return Err(SigV4Failure::UnsupportedAlgorithm);
    }
    let credential = query_value(query, "X-Amz-Credential")
        .ok_or_else(|| SigV4Failure::Malformed("missing X-Amz-Credential".to_owned()))?;
    let mut credential_parts = credential.splitn(2, '/');
    let access_key_id = credential_parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SigV4Failure::Malformed("missing access key".to_owned()))?;
    let scope = parse_scope(
        credential_parts
            .next()
            .ok_or_else(|| SigV4Failure::Malformed("missing credential scope".to_owned()))?,
    )
    .ok_or_else(|| SigV4Failure::Malformed("invalid credential scope".to_owned()))?;
    let amz_date = query_value(query, "X-Amz-Date")
        .ok_or_else(|| SigV4Failure::Malformed("missing X-Amz-Date".to_owned()))?;
    let timestamp_ms = parse_amz_date(&amz_date)
        .ok_or_else(|| SigV4Failure::Malformed("invalid X-Amz-Date".to_owned()))?;
    let expires_text = query_value(query, "X-Amz-Expires")
        .ok_or_else(|| SigV4Failure::Malformed("missing X-Amz-Expires".to_owned()))?;
    if expires_text.is_empty() || !expires_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SigV4Failure::Malformed("invalid X-Amz-Expires".to_owned()));
    }
    let expires = expires_text
        .parse::<i64>()
        .map_err(|_| SigV4Failure::Malformed("invalid X-Amz-Expires".to_owned()))?;
    if !(1..=MAX_PRESIGNED_EXPIRES).contains(&expires) {
        return Err(SigV4Failure::Malformed("invalid X-Amz-Expires".to_owned()));
    }
    let signed_headers = query_value(query, "X-Amz-SignedHeaders")
        .ok_or_else(|| SigV4Failure::Malformed("missing X-Amz-SignedHeaders".to_owned()))?
        .split(';')
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if signed_headers.is_empty() {
        return Err(SigV4Failure::Malformed(
            "empty X-Amz-SignedHeaders".to_owned(),
        ));
    }
    let signature = query_value(query, "X-Amz-Signature")
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| SigV4Failure::Malformed("missing X-Amz-Signature".to_owned()))?;
    Ok((
        ParsedAuthorization {
            scope,
            access_key_id: access_key_id.to_owned(),
            signed_headers,
            signature,
        },
        amz_date,
        timestamp_ms.saturating_add(expires.saturating_mul(1000)),
    ))
}

fn remove_signature(query: &[QueryEntry]) -> Vec<QueryEntry> {
    query
        .iter()
        .filter(|entry| !entry.name.eq_ignore_ascii_case("X-Amz-Signature"))
        .cloned()
        .collect()
}

pub struct VerifyRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [QueryEntry],
    pub headers: &'a [HeaderEntry],
    pub body: &'a [u8],
    pub credentials: &'a Credentials,
    pub expected_region: Option<&'a str>,
    pub now_ms: i64,
}

pub struct SignRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [QueryEntry],
    pub headers: &'a [HeaderEntry],
    pub signed_headers: &'a [String],
    pub credentials: &'a Credentials,
    pub region: &'a str,
    pub timestamp_ms: i64,
    pub payload_hash: &'a str,
}

pub struct PresignRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [QueryEntry],
    pub headers: &'a [HeaderEntry],
    pub signed_headers: &'a [String],
    pub credentials: &'a Credentials,
    pub region: &'a str,
    pub timestamp_ms: i64,
    pub expires: i64,
    pub payload_hash: Option<&'a str>,
}

/// Verify either Authorization-header or presigned-query SigV4.
pub fn verify_request(input: VerifyRequest<'_>) -> Result<VerifiedRequest, SigV4Failure> {
    verify_request_inner(input, true)
}

/// Verify the SigV4 envelope without hashing the request body.
///
/// This is the verification mode used by the streaming HTTP boundary.  The
/// canonical request still includes the payload hash or sentinel supplied by
/// the client. The request stream handler verifies ordinary hex payload hashes
/// incrementally and the streaming payload decoder verifies aws-chunked
/// signatures. This mode must not consume or buffer a body before the driver
/// can receive its first chunk.
pub fn verify_request_without_body(
    input: VerifyRequest<'_>,
) -> Result<VerifiedRequest, SigV4Failure> {
    verify_request_inner(input, false)
}

fn verify_request_inner(
    input: VerifyRequest<'_>,
    verify_body: bool,
) -> Result<VerifiedRequest, SigV4Failure> {
    let presigned =
        has_query(input.query, "X-Amz-Algorithm") || has_query(input.query, "X-Amz-Signature");
    let (parsed, amz_date, expiration_ms) = if presigned {
        let (parsed, date, expiration) = parse_presigned(input.query)?;
        (parsed, date, Some(expiration))
    } else {
        let authorization =
            header_value(input.headers, "authorization").ok_or(SigV4Failure::Missing)?;
        let parsed = parse_authorization(&authorization)?;
        let date = header_value(input.headers, "x-amz-date")
            .ok_or_else(|| SigV4Failure::Malformed("missing x-amz-date".to_owned()))?;
        (parsed, date, None)
    };
    if parsed.access_key_id != input.credentials.access_key_id {
        return Err(SigV4Failure::UnknownAccessKey);
    }
    if parsed.scope.service != S3_SERVICE
        || parsed.scope.date != amz_date.get(..8).unwrap_or_default()
        || input
            .expected_region
            .is_some_and(|region| parsed.scope.region != region)
    {
        return Err(SigV4Failure::ScopeMismatch(parsed.scope.as_string()));
    }
    let timestamp_ms = parse_amz_date(&amz_date)
        .ok_or_else(|| SigV4Failure::Malformed("invalid request timestamp".to_owned()))?;
    if let Some(expiration_ms) = expiration_ms {
        if timestamp_ms - input.now_ms > MAX_CLOCK_SKEW_MS {
            return Err(SigV4Failure::ClockSkew);
        }
        if input.now_ms > expiration_ms {
            return Err(SigV4Failure::Expired);
        }
    } else if (input.now_ms - timestamp_ms).abs() > MAX_CLOCK_SKEW_MS {
        return Err(SigV4Failure::ClockSkew);
    }
    let payload_hash = if presigned {
        header_value(input.headers, "x-amz-content-sha256")
            .unwrap_or_else(|| UNSIGNED_PAYLOAD.to_owned())
    } else {
        header_value(input.headers, "x-amz-content-sha256")
            .ok_or_else(|| SigV4Failure::Malformed("missing x-amz-content-sha256".to_owned()))?
    };
    if verify_body
        && payload_hash != UNSIGNED_PAYLOAD
        && payload_hash != STREAMING_PAYLOAD
        && payload_hash != STREAMING_PAYLOAD_TRAILER
        && payload_hash != STREAMING_UNSIGNED_PAYLOAD_TRAILER
    {
        let actual = sha256_hex(input.body);
        if payload_hash != actual {
            return Err(SigV4Failure::SignatureMismatch);
        }
    }
    let canonical_query = if presigned {
        remove_signature(input.query)
    } else {
        input.query.to_vec()
    };
    let canonical = canonical_request(
        input.method,
        input.path,
        &canonical_query,
        input.headers,
        &parsed.signed_headers,
        &payload_hash,
    );
    let expected = signature_of(
        &input.credentials.secret_access_key,
        &parsed.scope,
        &amz_date,
        &canonical,
    );
    if expected
        .as_bytes()
        .ct_eq(parsed.signature.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(SigV4Failure::SignatureMismatch);
    }
    Ok(VerifiedRequest {
        scope: parsed.scope,
        timestamp_ms,
        signature: parsed.signature,
        signed_headers: parsed.signed_headers,
        presigned,
        payload_hash,
    })
}

/// Produce an Authorization header for a request.  The caller supplies the
/// headers that will actually be sent; `host` and `x-amz-date` must therefore
/// be present in `headers` when they are included in `signed_headers`.
pub fn sign_request(input: SignRequest<'_>) -> String {
    let amz_date = format_amz_date(input.timestamp_ms);
    let scope = CredentialScope {
        date: amz_date[..8].to_owned(),
        region: input.region.to_owned(),
        service: S3_SERVICE.to_owned(),
    };
    let canonical = canonical_request(
        input.method,
        input.path,
        input.query,
        input.headers,
        input.signed_headers,
        input.payload_hash,
    );
    let signature = signature_of(
        &input.credentials.secret_access_key,
        &scope,
        &amz_date,
        &canonical,
    );
    format!(
        "{SIGV4_ALGORITHM} Credential={}/{}, SignedHeaders={}, Signature={signature}",
        input.credentials.access_key_id,
        scope.as_string(),
        canonical_signed_headers(input.signed_headers).join(";")
    )
}

/// Produce the query parameters for a SigV4 presigned request.
///
/// The returned parameters are decoded name/value pairs. Pass them through
/// [`canonical_query`] when constructing an origin-form request target; the
/// canonical encoder supplies the required RFC 3986 escaping and ordering.
pub fn presign_request(input: PresignRequest<'_>) -> Result<PresignedRequest, SigV4Failure> {
    if !(1..=MAX_PRESIGNED_EXPIRES).contains(&input.expires) {
        return Err(SigV4Failure::Malformed("invalid X-Amz-Expires".to_owned()));
    }
    let amz_date = format_amz_date(input.timestamp_ms);
    let scope = CredentialScope {
        date: amz_date[..8].to_owned(),
        region: input.region.to_owned(),
        service: S3_SERVICE.to_owned(),
    };
    let signed_headers = canonical_signed_headers(input.signed_headers);
    let mut full_query = input.query.to_vec();
    full_query.extend([
        QueryEntry::new("X-Amz-Algorithm", SIGV4_ALGORITHM),
        QueryEntry::new(
            "X-Amz-Credential",
            format!("{}/{}", input.credentials.access_key_id, scope.as_string()),
        ),
        QueryEntry::new("X-Amz-Date", amz_date.clone()),
        QueryEntry::new("X-Amz-Expires", input.expires.to_string()),
        QueryEntry::new("X-Amz-SignedHeaders", signed_headers.join(";")),
    ]);
    let payload_hash = input.payload_hash.unwrap_or(UNSIGNED_PAYLOAD);
    let canonical = canonical_request(
        input.method,
        input.path,
        &full_query,
        input.headers,
        &signed_headers,
        payload_hash,
    );
    let signature = signature_of(
        &input.credentials.secret_access_key,
        &scope,
        &amz_date,
        &canonical,
    );
    full_query.push(QueryEntry::new("X-Amz-Signature", signature.clone()));
    Ok(PresignedRequest {
        query: full_query,
        scope,
        signed_headers,
        amz_date,
        signature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_s3_paths_once_and_keeps_slashes() {
        assert_eq!(canonical_uri("//a b//"), "//a%20b//");
        assert_eq!(canonical_uri("emoji/😀"), "/emoji/%F0%9F%98%80");
    }

    #[test]
    fn official_basic_vector_signature() {
        let credentials =
            Credentials::new("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        let scope = CredentialScope {
            date: "20150830".to_owned(),
            region: "us-east-1".to_owned(),
            service: "service".to_owned(),
        };
        let headers = vec![
            HeaderEntry::new("Host", "example.amazonaws.com"),
            HeaderEntry::new("X-Amz-Date", "20150830T123600Z"),
        ];
        let signed = vec!["host".to_owned(), "x-amz-date".to_owned()];
        let canonical = canonical_request("GET", "/", &[], &headers, &signed, EMPTY_PAYLOAD_SHA256);
        assert_eq!(
            signature_of(
                &credentials.secret_access_key,
                &scope,
                "20150830T123600Z",
                &canonical,
            ),
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }
}
