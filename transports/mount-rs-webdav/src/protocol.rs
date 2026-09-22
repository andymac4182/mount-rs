//! WebDAV request parsing and response documents.
//!
//! This module deliberately has no listener.  It is the request/reply layer
//! used by both the in-process tests and the HTTP server.  The vocabulary and
//! edge cases follow RFC 4918 and the mountx WebDAV oracle pinned in the crate
//! README.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use httpdate::{fmt_http_date, parse_http_date};
use mount_rs_core::{ErrorCode, FileHandle, FsError, Stats};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use sha2::{Digest, Sha256};
use url::Url;

use crate::constants::{
    DAV_NS, DEFAULT_MAX_REQUEST_BYTES, MAX_XML_DEPTH, MAX_XML_ELEMENTS, XML_CONTENT_TYPE,
    status_for_error, status_line, status_text,
};

/// A normalized HTTP request head.  Header names are lowercase and values are
/// already combined by the transport boundary.
#[derive(Debug, Clone, Default)]
pub struct WebdavRequestHead {
    pub method: String,
    pub target: String,
    pub headers: BTreeMap<String, String>,
}

/// A file body that is read positionally by the HTTP writer.
pub struct FileBody {
    pub handle: Arc<dyn FileHandle>,
    pub start: u64,
    pub length: u64,
    pub chunk_size: usize,
}

/// A response body.  `File` avoids making the session allocate a whole GET.
pub enum WebdavBody {
    Bytes(Vec<u8>),
    File(FileBody),
}

impl WebdavBody {
    /// Drain a body for a session-level test or a small embedding.
    pub async fn into_bytes(self) -> Result<Vec<u8>, FsError> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::File(file) => {
                let mut result = Vec::with_capacity(file.length.min(usize::MAX as u64) as usize);
                let mut position = file.start;
                let end = file.start.saturating_add(file.length);
                let mut buffer = vec![0_u8; file.chunk_size.max(1)];
                let outcome = async {
                    while position < end {
                        let wanted = (end - position).min(buffer.len() as u64) as usize;
                        let count = file
                            .handle
                            .read(&mut buffer[..wanted], Some(position))
                            .await?;
                        if count > wanted {
                            return Err(FsError::new(ErrorCode::Eio)
                                .with_syscall("read")
                                .with_message("driver returned more bytes than requested"));
                        }
                        if count == 0 {
                            return Err(FsError::new(ErrorCode::Eio)
                                .with_syscall("read")
                                .with_message("short WebDAV response body"));
                        }
                        result.extend_from_slice(&buffer[..count]);
                        position += count as u64;
                    }
                    Ok::<(), FsError>(())
                }
                .await;
                let close = file.handle.close().await;
                outcome.and(close.map(|_| result))
            }
        }
    }
}

/// A reply before it reaches a socket.
pub struct WebdavResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Option<WebdavBody>,
}

impl WebdavResponse {
    pub fn empty(status: u16) -> Self {
        let mut headers = BTreeMap::new();
        headers.insert("content-length".to_owned(), "0".to_owned());
        Self {
            status,
            headers,
            body: None,
        }
    }
}

/// A protocol refusal, including its RFC 4918 precondition element when one
/// is useful to the client.
#[derive(Debug, Clone)]
pub struct DavFault {
    pub status: u16,
    pub condition: Option<String>,
    pub hrefs: Vec<String>,
    pub headers: BTreeMap<String, String>,
    pub message: String,
}

impl DavFault {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            condition: None,
            hrefs: Vec::new(),
            headers: BTreeMap::new(),
            message: status_line(status),
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn with_condition(mut self, condition: impl Into<String>, hrefs: Vec<String>) -> Self {
        self.condition = Some(condition.into());
        self.hrefs = hrefs;
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

impl fmt::Display for DavFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DavFault {}

/// Errors that the session turns into exactly one WebDAV reply.
#[derive(Debug, Clone)]
pub enum WebdavError {
    Fault(DavFault),
    Filesystem(FsError),
    Body(String),
}

impl fmt::Display for WebdavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fault(error) => error.fmt(f),
            Self::Filesystem(error) => error.fmt(f),
            Self::Body(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for WebdavError {}

impl From<DavFault> for WebdavError {
    fn from(value: DavFault) -> Self {
        Self::Fault(value)
    }
}

impl From<FsError> for WebdavError {
    fn from(value: FsError) -> Self {
        Self::Filesystem(value)
    }
}

pub fn refuse(status: u16) -> DavFault {
    DavFault::new(status)
}

pub fn status_of_error(error: &WebdavError) -> u16 {
    match error {
        WebdavError::Fault(error) => error.status,
        WebdavError::Filesystem(error) => status_for_error(error.code),
        WebdavError::Body(_) => 500,
    }
}

pub fn fault_response(error: &WebdavError) -> WebdavResponse {
    let status = status_of_error(error);
    let (condition, hrefs, extra) = match error {
        WebdavError::Fault(fault) => (
            fault.condition.as_deref(),
            fault.hrefs.as_slice(),
            &fault.headers,
        ),
        _ => (None, &[][..], &BTreeMap::new()),
    };
    if let Some(condition) = condition {
        xml_body(
            status,
            &encode_error_document(condition, hrefs),
            extra.clone(),
        )
    } else {
        let mut headers = extra.clone();
        headers.insert("content-length".to_owned(), "0".to_owned());
        WebdavResponse {
            status,
            headers,
            body: None,
        }
    }
}

pub fn xml_body(status: u16, document: &str, extra: BTreeMap<String, String>) -> WebdavResponse {
    let bytes = document.as_bytes().to_vec();
    let mut headers = extra;
    headers.insert("content-type".to_owned(), XML_CONTENT_TYPE.to_owned());
    headers.insert("content-length".to_owned(), bytes.len().to_string());
    WebdavResponse {
        status,
        headers,
        body: Some(WebdavBody::Bytes(bytes)),
    }
}

// ---------------------------------------------------------------------------
// Paths and HTTP headers
// ---------------------------------------------------------------------------

const URI_COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'=')
    .add(b'`')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Percent-decode one request-target segment, rejecting malformed UTF-8,
/// encoded separators, and NULs.  `..` is normalized by the core path helper.
pub fn parse_target_path(target: &str) -> Result<String, DavFault> {
    let target = target.split_once('?').map_or(target, |(path, _)| path);
    let target = target.split_once('#').map_or(target, |(path, _)| path);
    if !target.starts_with('/') {
        return Err(refuse(400).with_message("the request target is not an absolute path"));
    }
    let mut decoded = Vec::new();
    for segment in target.split('/').filter(|segment| !segment.is_empty()) {
        let bytes = segment.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                if index + 2 >= bytes.len()
                    || !bytes[index + 1].is_ascii_hexdigit()
                    || !bytes[index + 2].is_ascii_hexdigit()
                {
                    return Err(
                        refuse(400).with_message("the request target has a malformed escape")
                    );
                }
                index += 3;
            } else {
                index += 1;
            }
        }
        let value = percent_decode_str(segment)
            .decode_utf8()
            .map_err(|_| refuse(400).with_message("the request target is not valid UTF-8"))?;
        if value.contains('/') || value.contains('\0') {
            return Err(refuse(400).with_message("the request target names no resource"));
        }
        decoded.push(value.into_owned());
    }
    Ok(mount_rs_core::path::normalize_path(&format!(
        "/{}",
        decoded.join("/")
    )))
}

pub fn href_of(path: &str, collection: bool) -> String {
    let normalized = mount_rs_core::path::normalize_path(path);
    let mut href = String::from("/");
    let parts = mount_rs_core::path::split_path(&normalized);
    href.push_str(
        &parts
            .iter()
            .map(|part| utf8_percent_encode(part, URI_COMPONENT).to_string())
            .collect::<Vec<_>>()
            .join("/"),
    );
    if collection && href != "/" {
        href.push('/');
    }
    href
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Zero,
    One,
    Infinity,
}

pub fn parse_depth(value: Option<&str>, fallback: Depth) -> Option<Depth> {
    match value.map(str::trim).map(|value| value.to_ascii_lowercase()) {
        None => Some(fallback),
        Some(value) if value == "0" => Some(Depth::Zero),
        Some(value) if value == "1" => Some(Depth::One),
        Some(value) if value == "infinity" => Some(Depth::Infinity),
        Some(_) => None,
    }
}

pub fn parse_overwrite(value: Option<&str>) -> Option<bool> {
    match value.map(str::trim).map(|value| value.to_ascii_uppercase()) {
        None => Some(true),
        Some(value) if value == "T" => Some(true),
        Some(value) if value == "F" => Some(false),
        Some(_) => None,
    }
}

pub fn parse_destination(value: Option<&str>, host: Option<&str>) -> Result<String, DavFault> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| refuse(400).with_message("the Destination header is required"))?;
    if value.starts_with('/') {
        return parse_target_path(value);
    }
    let url = Url::parse(value)
        .map_err(|_| refuse(400).with_message("the Destination header is not a URI"))?;
    let host_matches = host.is_some_and(|host| authority_matches(&url, host));
    if url.host_str().is_none() || !host_matches {
        return Err(refuse(502).with_message("the Destination is not on this server"));
    }
    parse_target_path(url.path())
}

fn authority_parts(value: &str) -> Option<(String, Option<u16>)> {
    let value = value.trim();
    if value.starts_with('[') {
        let end = value.find(']')?;
        let host = &value[1..end];
        let suffix = &value[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':')?.parse().ok()?)
        };
        return Some((host.to_ascii_lowercase(), port));
    }
    if value.matches(':').count() > 1 {
        return Some((value.to_ascii_lowercase(), None));
    }
    if let Some((host, port)) = value.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return Some((host.to_ascii_lowercase(), Some(port)));
    }
    Some((value.to_ascii_lowercase(), None))
}

fn authority_matches(url: &Url, host: &str) -> bool {
    let Some((candidate_host, candidate_port)) = authority_parts(host) else {
        return false;
    };
    let Some(url_host) = url.host_str().and_then(authority_parts) else {
        return false;
    };
    if !candidate_host.eq_ignore_ascii_case(&url_host.0) {
        return false;
    }
    let default_port = match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    candidate_port.or(default_port) == url.port().or(default_port)
}

pub fn parse_timeout(value: Option<&str>) -> Option<LockTimeout> {
    let first = value?.split(',').next()?.trim();
    if first.eq_ignore_ascii_case("infinite") {
        return Some(LockTimeout::Infinite);
    }
    first
        .strip_prefix("Second-")
        .or_else(|| first.strip_prefix("second-"))
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .map(LockTimeout::Seconds)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockTimeout {
    Seconds(u64),
    Infinite,
}

pub fn parse_lock_token(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (value.starts_with('<')
        && value.ends_with('>')
        && value.len() > 2
        && !value[1..value.len() - 1].contains(['<', '>']))
    .then(|| value[1..value.len() - 1].to_owned())
}

pub fn format_lock_token(token: &str) -> String {
    format!("<{token}>")
}

// ---------------------------------------------------------------------------
// If header and validators
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfCondition {
    pub negated: bool,
    pub token: Option<String>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfList {
    pub resource: Option<String>,
    pub foreign: bool,
    pub conditions: Vec<IfCondition>,
}

pub fn parse_if(value: &str, host: Option<&str>) -> Option<Vec<IfList>> {
    let mut lists = Vec::new();
    let mut tag: Option<(Option<String>, bool)> = None;
    let mut at = 0;
    let bytes = value.as_bytes();
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at] == b'<' {
            let end = value[at + 1..].find('>')? + at + 1;
            let raw = &value[at + 1..end];
            tag = Some(resource_tag(raw, host));
            at = end + 1;
            continue;
        }
        if bytes[at] != b'(' {
            return None;
        }
        let (conditions, next) = parse_if_list(value, at)?;
        lists.push(IfList {
            resource: tag.as_ref().and_then(|tag| tag.0.clone()),
            foreign: tag.as_ref().is_some_and(|tag| tag.1),
            conditions,
        });
        at = next;
    }
    (!lists.is_empty()).then_some(lists)
}

fn resource_tag(reference: &str, host: Option<&str>) -> (Option<String>, bool) {
    if reference.starts_with('/') {
        return match parse_target_path(reference) {
            Ok(path) => (Some(path), false),
            Err(_) => (None, true),
        };
    }
    let Ok(url) = Url::parse(reference) else {
        return (None, true);
    };
    let local = host.is_some_and(|host| authority_matches(&url, host));
    if !local {
        return (None, true);
    }
    match parse_target_path(url.path()) {
        Ok(path) => (Some(path), false),
        Err(_) => (None, true),
    }
}

fn parse_if_list(value: &str, start: usize) -> Option<(Vec<IfCondition>, usize)> {
    let mut conditions = Vec::new();
    let mut at = start + 1;
    let mut negated = false;
    while at < value.len() {
        let byte = value.as_bytes()[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if byte == b')' {
            return (!conditions.is_empty()).then_some((conditions, at + 1));
        }
        if value[at..].len() >= 3 && value[at..at + 3].eq_ignore_ascii_case("Not") {
            negated = true;
            at += 3;
            continue;
        }
        if byte == b'<' {
            let end = value[at + 1..].find('>')? + at + 1;
            conditions.push(IfCondition {
                negated,
                token: Some(value[at + 1..end].trim().to_owned()),
                etag: None,
            });
            negated = false;
            at = end + 1;
            continue;
        }
        if byte == b'[' {
            let mut quote = false;
            let mut end = None;
            for index in at + 1..value.len() {
                let current = value.as_bytes()[index];
                if current == b'"' {
                    quote = !quote;
                } else if current == b']' && !quote {
                    end = Some(index);
                    break;
                }
            }
            let end = end?;
            conditions.push(IfCondition {
                negated,
                token: None,
                etag: Some(value[at + 1..end].trim().to_owned()),
            });
            negated = false;
            at = end + 1;
            continue;
        }
        return None;
    }
    None
}

pub fn submitted_tokens(lists: &[IfList]) -> Vec<String> {
    let mut tokens = Vec::new();
    for condition in lists.iter().flat_map(|list| list.conditions.iter()) {
        if !condition.negated
            && let Some(token) = &condition.token
            && !tokens.iter().any(|submitted| submitted == token)
        {
            tokens.push(token.clone());
        }
    }
    tokens
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeSpec {
    Full,
    Range { start: u64, end: u64, length: u64 },
    Unsatisfiable,
}

pub fn parse_range(value: Option<&str>, size: u64) -> RangeSpec {
    let Some(value) = value else {
        return RangeSpec::Full;
    };
    let Some((unit, spec)) = value.split_once('=') else {
        return RangeSpec::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return RangeSpec::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeSpec::Full;
    };
    if first.trim().is_empty() {
        let Some(suffix) = range_number(last) else {
            return RangeSpec::Full;
        };
        if suffix == 0 || size == 0 {
            return RangeSpec::Unsatisfiable;
        }
        let start = size.saturating_sub(suffix);
        return RangeSpec::Range {
            start,
            end: size - 1,
            length: size - start,
        };
    }
    let Some(start) = range_number(first) else {
        return RangeSpec::Full;
    };
    if last.trim().is_empty() {
        if start >= size {
            return RangeSpec::Unsatisfiable;
        }
        return RangeSpec::Range {
            start,
            end: size - 1,
            length: size - start,
        };
    }
    let Some(requested_end) = range_number(last) else {
        return RangeSpec::Full;
    };
    if requested_end < start {
        return RangeSpec::Full;
    }
    if start >= size {
        return RangeSpec::Unsatisfiable;
    }
    let end = requested_end.min(size - 1);
    RangeSpec::Range {
        start,
        end,
        length: end - start + 1,
    }
}

fn range_number(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(value.parse().unwrap_or(u64::MAX))
}

pub fn format_etag(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value.to_owned()
    } else {
        format!("\"{value}\"")
    }
}

pub fn resource_etag(stats: &Stats) -> String {
    let mut digest = Sha256::new();
    digest.update(format!(
        "{}:{}:{}:{}",
        stats.dev, stats.ino, stats.size, stats.mtime_ms
    ));
    let hex = format!("{:x}", digest.finalize());
    format_etag(&hex[..32])
}

fn parse_etag(value: &str) -> (String, bool) {
    let weak = value.trim_start().starts_with("W/");
    let value = if weak {
        value.trim_start()[2..].trim()
    } else {
        value.trim()
    };
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    (value.to_owned(), weak)
}

fn etag_matches(value: &str, etag: &str, strong: bool) -> bool {
    if value.trim() == "*" {
        return true;
    }
    let (target, target_weak) = parse_etag(etag);
    value
        .split(',')
        .map(|part| {
            let (candidate, weak) = parse_etag(part);
            (candidate, weak)
        })
        .any(|(candidate, weak)| candidate == target && (!strong || (!weak && !target_weak)))
}

fn ms_to_system_time(ms: i64) -> SystemTime {
    if ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms as u64)
    } else {
        UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
    }
}

pub fn format_http_date_ms(ms: i64) -> String {
    fmt_http_date(ms_to_system_time(ms))
}

pub fn parse_http_date_ms(value: &str) -> Option<i64> {
    let time = parse_http_date(value).ok()?;
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).ok(),
        Err(error) => i64::try_from(error.duration().as_millis())
            .ok()
            .map(|value| -value),
    }
}

pub fn format_iso_date_ms(ms: i64) -> String {
    // Civil-date conversion from Unix days, avoiding a platform date library.
    let seconds = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let day = doy - (153 * mp + 2).div_euclid(5) + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    let hour = day_seconds / 3600;
    let minute = (day_seconds % 3600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionalOutcome {
    pub status: u16,
}

pub fn evaluate_conditionals(
    headers: &BTreeMap<String, String>,
    stats: Option<&Stats>,
    method: &str,
) -> ConditionalOutcome {
    let Some(stats) = stats else {
        if headers.contains_key("if-match") {
            return ConditionalOutcome { status: 412 };
        }
        return ConditionalOutcome { status: 200 };
    };
    let etag = resource_etag(stats);
    if let Some(value) = headers.get("if-match") {
        if !etag_matches(value, &etag, true) {
            return ConditionalOutcome { status: 412 };
        }
    } else if let Some(value) = headers.get("if-unmodified-since")
        && let Some(date) = parse_http_date_ms(value)
        && stats.mtime_ms.div_euclid(1000) > date.div_euclid(1000)
    {
        return ConditionalOutcome { status: 412 };
    }
    if let Some(value) = headers.get("if-none-match") {
        if etag_matches(value, &etag, false) {
            return ConditionalOutcome {
                status: if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD")
                {
                    304
                } else {
                    412
                },
            };
        }
        return ConditionalOutcome { status: 200 };
    }
    if (method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD"))
        && let Some(value) = headers.get("if-modified-since")
        && let Some(date) = parse_http_date_ms(value)
        && stats.mtime_ms.div_euclid(1000) <= date.div_euclid(1000)
    {
        return ConditionalOutcome { status: 304 };
    }
    ConditionalOutcome { status: 200 }
}

// ---------------------------------------------------------------------------
// XML tree, bounded parsing, and WebDAV documents
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlNode {
    pub name: String,
    pub ns: String,
    pub text: String,
    pub children: Vec<XmlNode>,
}

impl XmlNode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ns: String::new(),
            text: String::new(),
            children: Vec::new(),
        }
    }

    pub fn dav(name: impl Into<String>) -> Self {
        Self {
            ns: DAV_NS.to_owned(),
            ..Self::new(name)
        }
    }
}

#[derive(Debug)]
struct XmlFrame {
    qname: String,
    namespaces: HashMap<String, String>,
    node: XmlNode,
}

fn xml_fault(message: impl Into<String>) -> DavFault {
    refuse(400).with_message(format!("the XML body is not usable: {}", message.into()))
}

fn qname_name(raw: &[u8]) -> Result<(String, String), DavFault> {
    let raw = std::str::from_utf8(raw).map_err(|_| xml_fault("invalid element name"))?;
    if let Some((prefix, local)) = raw.split_once(':') {
        Ok((prefix.to_owned(), local.to_owned()))
    } else {
        Ok((String::new(), raw.to_owned()))
    }
}

fn start_node(
    start: &BytesStart<'_>,
    inherited: &HashMap<String, String>,
    reader: &Reader<&[u8]>,
) -> Result<(String, HashMap<String, String>, XmlNode), DavFault> {
    let mut namespaces = inherited.clone();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| xml_fault(error.to_string()))?;
        let key = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|_| xml_fault("invalid attribute name"))?;
        if key == "xmlns" {
            let value = attribute
                .decode_and_unescape_value(reader.decoder())
                .map_err(|error| xml_fault(error.to_string()))?;
            namespaces.insert(String::new(), value.into_owned());
        } else if let Some(prefix) = key.strip_prefix("xmlns:") {
            let value = attribute
                .decode_and_unescape_value(reader.decoder())
                .map_err(|error| xml_fault(error.to_string()))?;
            namespaces.insert(prefix.to_owned(), value.into_owned());
        }
    }
    let qname = String::from_utf8(start.name().as_ref().to_vec())
        .map_err(|_| xml_fault("invalid element name"))?;
    let (prefix, local) = qname_name(qname.as_bytes())?;
    let ns = namespaces.get(&prefix).cloned().unwrap_or_default();
    Ok((
        qname,
        namespaces,
        XmlNode {
            name: local,
            ns,
            text: String::new(),
            children: Vec::new(),
        },
    ))
}

fn is_xml_character(code: u32) -> bool {
    if code < 0x20 {
        return matches!(code, 0x09 | 0x0a | 0x0d);
    }
    if code < 0x7f {
        return true;
    }
    if code <= 0x9f {
        return false;
    }
    (code & 0xfffe) != 0xfffe
}

fn decode_xml_reference(reference: &[u8]) -> Result<String, DavFault> {
    let reference = std::str::from_utf8(reference)
        .map_err(|_| xml_fault("XML entity reference is not valid UTF-8"))?;
    let character = match reference {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let (radix, digits) = if let Some(digits) = reference.strip_prefix("#x") {
                (16, digits)
            } else if let Some(digits) = reference.strip_prefix("#X") {
                (16, digits)
            } else if let Some(digits) = reference.strip_prefix('#') {
                (10, digits)
            } else {
                return Err(xml_fault(
                    "XML entities must be one of the five predefined references",
                ));
            };
            if digits.is_empty() || digits.len() > 10 {
                return Err(xml_fault("XML character reference has invalid length"));
            }
            let code = u32::from_str_radix(digits, radix)
                .map_err(|_| xml_fault("XML character reference has invalid digits"))?;
            if !is_xml_character(code) {
                return Err(xml_fault(
                    "XML character reference names an unsupported character",
                ));
            }
            char::from_u32(code)
                .ok_or_else(|| xml_fault("XML character reference names an invalid character"))?
        }
    };
    Ok(character.to_string())
}

pub fn parse_xml(body: &[u8], max_bytes: usize) -> Result<XmlNode, DavFault> {
    if body.len() > max_bytes {
        return Err(refuse(413).with_message("the XML body exceeds its byte budget"));
    }
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack: Vec<XmlFrame> = Vec::new();
    let mut root: Option<XmlNode> = None;
    let mut elements = 0usize;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_fault(error.to_string()))?;
        match event {
            Event::Start(start) => {
                elements += 1;
                if elements > MAX_XML_ELEMENTS {
                    return Err(xml_fault("too many XML elements"));
                }
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(xml_fault("XML nesting is too deep"));
                }
                let inherited = stack
                    .last()
                    .map(|frame| &frame.namespaces)
                    .cloned()
                    .unwrap_or_default();
                let (qname, namespaces, node) = start_node(&start, &inherited, &reader)?;
                stack.push(XmlFrame {
                    qname,
                    namespaces,
                    node,
                });
            }
            Event::Empty(start) => {
                elements += 1;
                if elements > MAX_XML_ELEMENTS {
                    return Err(xml_fault("too many XML elements"));
                }
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(xml_fault("XML nesting is too deep"));
                }
                let inherited = stack
                    .last()
                    .map(|frame| &frame.namespaces)
                    .cloned()
                    .unwrap_or_default();
                let (_, _, node) = start_node(&start, &inherited, &reader)?;
                if let Some(parent) = stack.last_mut() {
                    parent.node.children.push(node);
                } else if root.is_none() {
                    root = Some(node);
                } else {
                    return Err(xml_fault("multiple XML roots"));
                }
            }
            Event::End(end) => {
                let frame = stack.pop().ok_or_else(|| xml_fault("unexpected XML end"))?;
                let actual = String::from_utf8(end.name().as_ref().to_vec())
                    .map_err(|_| xml_fault("invalid XML end name"))?;
                if frame.qname != actual {
                    return Err(xml_fault("mismatched XML element"));
                }
                if let Some(parent) = stack.last_mut() {
                    parent.node.children.push(frame.node);
                } else if root.is_none() {
                    root = Some(frame.node);
                } else {
                    return Err(xml_fault("multiple XML roots"));
                }
            }
            Event::Text(text) => {
                let text = text
                    .decode()
                    .map_err(|error| xml_fault(error.to_string()))?;
                let text = quick_xml::escape::unescape(&text)
                    .map_err(|error| xml_fault(error.to_string()))?;
                if let Some(frame) = stack.last_mut() {
                    frame.node.text.push_str(&text);
                } else if !text.trim().is_empty() {
                    return Err(xml_fault("text outside the XML root"));
                }
            }
            Event::CData(text) => {
                let text = std::str::from_utf8(text.as_ref())
                    .map_err(|_| xml_fault("invalid XML CDATA"))?;
                if let Some(frame) = stack.last_mut() {
                    frame.node.text.push_str(text);
                }
            }
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            Event::DocType(_) => {
                return Err(xml_fault("DOCTYPE is not accepted"));
            }
            Event::GeneralRef(reference) => {
                let text = decode_xml_reference(reference.as_ref())?;
                if let Some(frame) = stack.last_mut() {
                    frame.node.text.push_str(&text);
                } else if !text.trim().is_empty() {
                    return Err(xml_fault("text outside the XML root"));
                }
            }
            Event::Eof => break,
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return Err(xml_fault("unterminated XML element"));
    }
    root.ok_or_else(|| xml_fault("missing XML root"))
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_node(node: &XmlNode, inherited_ns: &str) -> String {
    let declaration = if node.ns != inherited_ns {
        format!(" xmlns=\"{}\"", escape_xml(&node.ns))
    } else {
        String::new()
    };
    let mut result = format!("<{}{declaration}>", node.name);
    result.push_str(&escape_xml(&node.text));
    for child in &node.children {
        result.push_str(&render_node(child, &node.ns));
    }
    result.push_str("</");
    result.push_str(&node.name);
    result.push('>');
    result
}

pub fn xml_document(root: &XmlNode) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}",
        render_node(root, "")
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavPropertyName {
    pub ns: String,
    pub name: String,
}

pub fn dav_property(name: &str) -> DavPropertyName {
    DavPropertyName {
        ns: DAV_NS.to_owned(),
        name: name.to_owned(),
    }
}

pub fn same_property_name(left: &DavPropertyName, right: &DavPropertyName) -> bool {
    left == right
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropfindRequest {
    Allprop,
    Propname,
    Prop(Vec<DavPropertyName>),
}

pub fn parse_propfind(body: &[u8], max_bytes: usize) -> Result<PropfindRequest, DavFault> {
    if body.is_empty() {
        return Ok(PropfindRequest::Allprop);
    }
    let root = parse_xml(body, max_bytes)?;
    if root.name != "propfind" {
        return Err(refuse(400).with_message("expected a propfind XML document"));
    }
    let mut names = Vec::new();
    let mut saw_prop = false;
    for child in &root.children {
        match child.name.as_str() {
            "allprop" => return Ok(PropfindRequest::Allprop),
            "propname" => return Ok(PropfindRequest::Propname),
            "prop" => {
                saw_prop = true;
                for property in &child.children {
                    let name = DavPropertyName {
                        ns: property.ns.clone(),
                        name: property.name.clone(),
                    };
                    if !names.iter().any(|seen| same_property_name(seen, &name)) {
                        names.push(name);
                    }
                }
            }
            _ => {}
        }
    }
    if !saw_prop {
        return Err(refuse(400).with_message("a propfind body must name a property form"));
    }
    Ok(PropfindRequest::Prop(names))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProppatchSet {
    pub name: DavPropertyName,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProppatchRequest {
    pub set: Vec<ProppatchSet>,
    pub remove: Vec<DavPropertyName>,
    pub instructions: Vec<ProppatchInstruction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProppatchInstruction {
    Set(ProppatchSet),
    Remove(DavPropertyName),
}

pub fn parse_proppatch(body: &[u8], max_bytes: usize) -> Result<ProppatchRequest, DavFault> {
    let root = parse_xml(body, max_bytes)?;
    if root.name != "propertyupdate" {
        return Err(refuse(400).with_message("expected a propertyupdate XML document"));
    }
    let mut set = Vec::new();
    let mut remove = Vec::new();
    let mut instructions = Vec::new();
    for instruction in &root.children {
        let is_set = instruction.name == "set";
        if !is_set && instruction.name != "remove" {
            continue;
        }
        for prop in instruction
            .children
            .iter()
            .filter(|child| child.name == "prop")
        {
            for property in &prop.children {
                let name = DavPropertyName {
                    ns: property.ns.clone(),
                    name: property.name.clone(),
                };
                if is_set {
                    let instruction = ProppatchSet {
                        name,
                        text: property.text.clone(),
                    };
                    set.push(instruction.clone());
                    instructions.push(ProppatchInstruction::Set(instruction));
                } else {
                    remove.push(name.clone());
                    instructions.push(ProppatchInstruction::Remove(name));
                }
            }
        }
    }
    if set.is_empty() && remove.is_empty() {
        return Err(refuse(400).with_message("a propertyupdate body must name a property"));
    }
    Ok(ProppatchRequest {
        set,
        remove,
        instructions,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockInfoRequest {
    pub exclusive: bool,
    pub owner: Option<XmlNode>,
}

pub fn parse_lock_info(body: &[u8], max_bytes: usize) -> Result<Option<LockInfoRequest>, DavFault> {
    if body.is_empty() {
        return Ok(None);
    }
    let root = parse_xml(body, max_bytes)?;
    if root.name != "lockinfo" {
        return Err(refuse(400).with_message("expected a lockinfo XML document"));
    }
    let mut exclusive = None;
    let mut write = false;
    let mut owner = None;
    for child in &root.children {
        match child.name.as_str() {
            "lockscope" => {
                exclusive = if child.children.iter().any(|scope| scope.name == "exclusive") {
                    Some(true)
                } else if child.children.iter().any(|scope| scope.name == "shared") {
                    Some(false)
                } else {
                    None
                };
            }
            "locktype" => write = child.children.iter().any(|item| item.name == "write"),
            "owner" => owner = Some(child.clone()),
            _ => {}
        }
    }
    let Some(exclusive) = exclusive else {
        return Err(refuse(400).with_message("lockinfo needs exclusive or shared lockscope"));
    };
    if !write {
        return Err(refuse(400).with_message("write is the only supported lock type"));
    }
    Ok(Some(LockInfoRequest { exclusive, owner }))
}

#[derive(Debug, Clone)]
pub struct Propstat {
    pub status: u16,
    pub props: Vec<XmlNode>,
    pub condition: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MultistatusEntry {
    pub href: String,
    pub propstat: Vec<Propstat>,
    pub status: Option<u16>,
}

pub fn encode_multistatus(entries: &[MultistatusEntry]) -> String {
    let mut root = XmlNode::dav("multistatus");
    for entry in entries {
        let mut response = XmlNode::dav("response");
        response.children.push(XmlNode {
            name: "href".to_owned(),
            ns: DAV_NS.to_owned(),
            text: entry.href.clone(),
            children: Vec::new(),
        });
        for propstat in &entry.propstat {
            let mut node = XmlNode::dav("propstat");
            let mut prop = XmlNode::dav("prop");
            prop.children.extend(propstat.props.iter().cloned());
            node.children.push(prop);
            node.children.push(XmlNode {
                name: "status".to_owned(),
                ns: DAV_NS.to_owned(),
                text: status_line(propstat.status),
                children: Vec::new(),
            });
            if let Some(condition) = &propstat.condition {
                let mut error = XmlNode::dav("error");
                error.children.push(XmlNode::dav(condition));
                node.children.push(error);
            }
            response.children.push(node);
        }
        if let Some(status) = entry.status {
            response.children.push(XmlNode {
                name: "status".to_owned(),
                ns: DAV_NS.to_owned(),
                text: status_line(status),
                children: Vec::new(),
            });
        }
        root.children.push(response);
    }
    xml_document(&root)
}

pub fn encode_error_document(condition: &str, hrefs: &[String]) -> String {
    let mut root = XmlNode::dav("error");
    let mut node = XmlNode::dav(condition);
    for href in hrefs {
        node.children.push(XmlNode {
            name: "href".to_owned(),
            ns: DAV_NS.to_owned(),
            text: href.clone(),
            children: Vec::new(),
        });
    }
    root.children.push(node);
    xml_document(&root)
}

pub fn supported_lock_node() -> XmlNode {
    let mut root = XmlNode::dav("supportedlock");
    for scope in ["exclusive", "shared"] {
        let mut entry = XmlNode::dav("lockentry");
        let mut lockscope = XmlNode::dav("lockscope");
        lockscope.children.push(XmlNode::dav(scope));
        let mut locktype = XmlNode::dav("locktype");
        locktype.children.push(XmlNode::dav("write"));
        entry.children.extend([lockscope, locktype]);
        root.children.push(entry);
    }
    root
}

pub fn active_lock_node(lock: &crate::locks::DavLock, now: i64) -> XmlNode {
    let mut node = XmlNode::dav("activelock");
    let mut scope = XmlNode::dav("lockscope");
    scope.children.push(XmlNode::dav(if lock.exclusive {
        "exclusive"
    } else {
        "shared"
    }));
    let mut locktype = XmlNode::dav("locktype");
    locktype.children.push(XmlNode::dav("write"));
    node.children.push(scope);
    node.children.push(locktype);
    node.children.push(XmlNode {
        name: "depth".to_owned(),
        ns: DAV_NS.to_owned(),
        text: lock.depth.to_string(),
        children: Vec::new(),
    });
    if let Some(owner) = &lock.owner {
        node.children.push(owner.clone());
    }
    node.children.push(XmlNode {
        name: "timeout".to_owned(),
        ns: DAV_NS.to_owned(),
        text: format!(
            "Second-{}",
            crate::locks::DavLockTable::remaining(lock, now)
        ),
        children: Vec::new(),
    });
    let mut token = XmlNode::dav("locktoken");
    token.children.push(XmlNode {
        name: "href".to_owned(),
        ns: DAV_NS.to_owned(),
        text: lock.token.clone(),
        children: Vec::new(),
    });
    node.children.push(token);
    let mut root = XmlNode::dav("lockroot");
    root.children.push(XmlNode {
        name: "href".to_owned(),
        ns: DAV_NS.to_owned(),
        text: href_of(&lock.path, lock.collection),
        children: Vec::new(),
    });
    node.children.push(root);
    node
}

pub fn lock_discovery_node(locks: &[crate::locks::DavLock], now: i64) -> XmlNode {
    let mut root = XmlNode::dav("lockdiscovery");
    root.children
        .extend(locks.iter().map(|lock| active_lock_node(lock, now)));
    root
}

pub fn encode_lock_response(lock: &crate::locks::DavLock, now: i64) -> String {
    let mut root = XmlNode::dav("prop");
    root.children
        .push(lock_discovery_node(std::slice::from_ref(lock), now));
    xml_document(&root)
}

pub const ALLPROP_NAMES: &[&str] = &[
    "creationdate",
    "displayname",
    "getcontentlength",
    "getcontenttype",
    "getetag",
    "getlastmodified",
    "resourcetype",
    "supportedlock",
    "lockdiscovery",
];

pub const QUOTA_NAMES: &[&str] = &["quota-available-bytes", "quota-used-bytes"];

/// Constant-time comparison for the Basic credential strings without storing
/// credentials in a process-global or platform-specific authentication API.
pub fn basic_authorization(header: &str, username: &str, password: &str) -> bool {
    let header = header.trim();
    let Some(encoded) = header.strip_prefix("Basic") else {
        return false;
    };
    if !encoded.starts_with(' ') {
        return false;
    }
    let encoded = encoded.trim_start_matches(' ');
    if encoded.is_empty()
        || encoded.chars().any(char::is_whitespace)
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return false;
    }
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let canonical = base64::engine::general_purpose::STANDARD
        .encode(&decoded)
        .trim_end_matches('=')
        .to_owned();
    if canonical != encoded.trim_end_matches('=') || !decoded.contains(&b':') {
        return false;
    }
    let expected = format!("{username}:{password}");
    let left = Sha256::digest(decoded);
    let right = Sha256::digest(expected.as_bytes());
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |different, (left, right)| different | (left ^ right))
        == 0
}

/// Evaluate a parsed WebDAV `If` condition against a path's current lock/ETag
/// state.  The session supplies the state lookup because it needs the driver.
pub fn condition_matches(condition: &IfCondition, tokens: &[String], etag: Option<&str>) -> bool {
    let matched = if let Some(token) = &condition.token {
        tokens.iter().any(|candidate| candidate == token)
    } else if let Some(value) = &condition.etag {
        etag.is_some_and(|etag| etag_matches(value, etag, false))
    } else {
        false
    };
    if condition.negated { !matched } else { matched }
}

pub fn default_request_limit() -> usize {
    DEFAULT_MAX_REQUEST_BYTES
}

/// The status reason helper is public for differential fixtures and callers
/// that need to reproduce `<status>` values.
pub fn reason_phrase(status: u16) -> &'static str {
    status_text(status).unwrap_or("")
}

// Kept as a named alias so downstream code can use the same shape as the
// upstream request-body collector without depending on the HTTP implementation.
pub fn collect_body(body: &[u8], limit: usize) -> Result<Vec<u8>, WebdavError> {
    if body.len() > limit {
        return Err(refuse(413)
            .with_message(format!("the request body is over the {limit}-byte budget"))
            .into());
    }
    Ok(body.to_vec())
}
