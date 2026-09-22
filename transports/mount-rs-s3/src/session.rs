//! In-process S3 request session.
//!
//! The session is intentionally independent of HTTP. It maps one parsed S3
//! request to one response, owns no listener, and uses the shared
//! mount-rs-core::FsDriver contract for all storage.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_core::Stream;
use md5::{Digest as Md5Digest, Md5};
use mount_rs_core::{
    FileType, FsDriver, MkdirOptions, Stats,
    path::{dirname, normalize_path},
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc, oneshot};

use crate::protocol::{
    self, ByteRange, ListObjectsXml, ListPartsXml, ListedObject, ListedPart, MAX_PART_SIZE,
    MAX_XML_BYTES, MIN_PART_SIZE, MULTIPART_PREFIX, ObjectTarget, Operation, S3Error, S3Failure,
    S3Response, S3Result, STREAMING_STAGING_PREFIX, compare_utf8, conditional_match,
    content_encoding_chunked, decode_continuation_token, delete_result_xml,
    encode_continuation_token, error_response, header_md5, initiate_multipart_xml, is_staging_key,
    list_buckets_xml, list_objects_xml, list_parts_xml, parse_complete_document,
    parse_delete_document, parse_meta_mtime, parse_object_key, parse_request_target, s3_error,
    unquote_etag, xml_response,
};
use crate::sigv4::{self, Credentials, HeaderEntry, SigV4Failure, header_list, header_value};

const DEFAULT_BUCKET: &str = "mountx";
const DEFAULT_READ_CHUNK: usize = 128 * 1024;
const DEFAULT_MAX_BODY: usize = 512 * 1024 * 1024;
const DEFAULT_MULTIPART_STAGING_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
const DEFAULT_MULTIPART_STAGING_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MANIFEST_NAME: &str = "upload.json";
// The marker is created with exclusive-open so Complete and Abort have one
// filesystem-visible terminal winner even when different session instances
// share the same driver.
const FINALIZATION_MARKER: &str = ".finalizing";

struct ListObjectsRequest<'a> {
    bucket: &'a str,
    driver: Arc<dyn FsDriver>,
    prefix: &'a str,
    delimiter: Option<&'a str>,
    max_keys: usize,
    continuation: Option<&'a str>,
    start_after: Option<&'a str>,
    fetch_owner: bool,
    encoding_url: bool,
}

struct UploadBody<'a> {
    head: &'a S3RequestHead,
    body: &'a [u8],
    verified: Option<&'a sigv4::VerifiedRequest>,
}

struct StreamUploadBody<'a> {
    head: &'a S3RequestHead,
    body: S3RequestBody,
    verified: Option<&'a sigv4::VerifiedRequest>,
}

struct StreamWriteRequest<'a> {
    driver: &'a Arc<dyn FsDriver>,
    path: &'a str,
    headers: &'a [HeaderEntry],
    body: S3RequestBody,
    verified: Option<&'a sigv4::VerifiedRequest>,
    credentials: Option<&'a Credentials>,
    max_body_bytes: usize,
    exclusive: bool,
    create_parent: bool,
    cleanup_on_error: bool,
    staging_quota: Option<StagingQuota>,
}

#[derive(Debug, Clone, Copy)]
struct StagingQuota {
    limit: u64,
    used: u64,
    replaced: u64,
}

impl StagingQuota {
    fn allows(self, position: u64, additional: u64) -> bool {
        self.used
            .saturating_sub(self.replaced)
            .saturating_add(position)
            .saturating_add(additional)
            <= self.limit
    }
}

struct RequestTicketGuard {
    inflight: Arc<StdMutex<HashSet<u64>>>,
    ticket: Option<u64>,
}

impl RequestTicketGuard {
    fn new(inflight: Arc<StdMutex<HashSet<u64>>>, ticket: Option<u64>) -> Self {
        Self { inflight, ticket }
    }

    fn disarm(&mut self) {
        self.ticket = None;
    }
}

impl Drop for RequestTicketGuard {
    fn drop(&mut self) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.remove(&ticket);
        }
    }
}

struct ConditionalPutLockRegistry {
    locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ConditionalPutLockRegistry {
    fn new() -> Self {
        Self {
            locks: StdMutex::new(HashMap::new()),
        }
    }

    async fn acquire(self: &Arc<Self>, key: String) -> ConditionalPutLockGuard {
        let registration = {
            let mut locks = self.locks.lock().expect("conditional PUT lock registry");
            let lock = locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone();
            ConditionalPutLockRegistration {
                registry: Arc::clone(self),
                key: key.clone(),
                lock,
                active: true,
            }
        };
        let guard = Arc::clone(&registration.lock).lock_owned().await;
        let registry = Arc::clone(&registration.registry);
        let guard_key = registration.key.clone();
        let lock = Arc::clone(&registration.lock);
        registration.disarm();
        ConditionalPutLockGuard {
            registry,
            key: guard_key,
            lock,
            guard: Some(guard),
        }
    }

    fn remove_if_unused(&self, key: &str, lock: &Arc<Mutex<()>>) {
        let Ok(mut locks) = self.locks.lock() else {
            return;
        };
        if Arc::strong_count(lock) == 2
            && locks
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, lock))
        {
            locks.remove(key);
        }
    }
}

struct ConditionalPutLockRegistration {
    registry: Arc<ConditionalPutLockRegistry>,
    key: String,
    lock: Arc<Mutex<()>>,
    active: bool,
}

impl ConditionalPutLockRegistration {
    fn disarm(mut self) {
        self.active = false;
    }
}

impl Drop for ConditionalPutLockRegistration {
    fn drop(&mut self) {
        if self.active {
            self.registry.remove_if_unused(&self.key, &self.lock);
        }
    }
}

struct ConditionalPutLockGuard {
    registry: Arc<ConditionalPutLockRegistry>,
    key: String,
    lock: Arc<Mutex<()>>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for ConditionalPutLockGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.registry.remove_if_unused(&self.key, &self.lock);
    }
}

/// Remove a private staging path if the owning request is cancelled before it
/// reaches its normal async cleanup. `Drop` cannot await, so cancellation
/// schedules the unlink on the current Tokio runtime and ignores a missing
/// path or a driver that has already removed it.
struct StagedPathCleanup {
    driver: Arc<dyn FsDriver>,
    path: String,
    armed: bool,
}

impl StagedPathCleanup {
    fn new(driver: Arc<dyn FsDriver>, path: impl Into<String>) -> Self {
        Self {
            driver,
            path: path.into(),
            armed: true,
        }
    }

    async fn cleanup_now(&mut self) {
        if self.armed {
            let _ = self.driver.unlink(&self.path).await;
            self.armed = false;
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedPathCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let driver = Arc::clone(&self.driver);
        let path = self.path.clone();
        runtime.spawn(async move {
            let _ = driver.unlink(&path).await;
        });
    }
}

/// Optional impure and diagnostic hooks for an S3 session.
///
/// The TypeScript session exposes these as `now`, `requestId`, `onError`, and
/// `onAssertion`. Rust callers can inject the same boundaries without making
/// the storage or protocol code depend on a particular clock, logger, or
/// runtime callback mechanism.
pub type S3NowHook = Arc<dyn Fn() -> Option<i64> + Send + Sync + 'static>;
pub type S3RequestIdHook = Arc<dyn Fn() -> Option<String> + Send + Sync + 'static>;
pub type S3ErrorHook = Arc<dyn Fn(String, Option<S3RequestHead>) + Send + Sync + 'static>;
pub type S3AssertionHook = Arc<dyn Fn(String) + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct S3SessionHooks {
    /// Return the current epoch time in milliseconds, or `None` to use the
    /// system clock for this call.
    pub now_ms: Option<S3NowHook>,
    /// Return the client-visible request id, or `None` to use the session's
    /// process-local fallback id.
    pub request_id: Option<S3RequestIdHook>,
    /// Observe a request that produced an S3 error reply.
    pub on_error: Option<S3ErrorHook>,
    /// Observe a debug assertion failure.
    pub on_assertion: Option<S3AssertionHook>,
}

impl fmt::Debug for S3SessionHooks {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("S3SessionHooks")
            .field("now_ms", &self.now_ms.is_some())
            .field("request_id", &self.request_id.is_some())
            .field("on_error", &self.on_error.is_some())
            .field("on_assertion", &self.on_assertion.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct S3SessionOptions {
    pub credentials: Option<Credentials>,
    pub region: Option<String>,
    pub max_body_bytes: usize,
    pub max_xml_bytes: usize,
    pub read_chunk_bytes: usize,
    pub multipart_staging_ttl_ms: i64,
    pub multipart_staging_max_bytes: u64,
    pub debug: bool,
    pub hooks: S3SessionHooks,
}

impl Default for S3SessionOptions {
    fn default() -> Self {
        Self {
            credentials: None,
            region: None,
            max_body_bytes: DEFAULT_MAX_BODY,
            max_xml_bytes: MAX_XML_BYTES,
            read_chunk_bytes: DEFAULT_READ_CHUNK,
            multipart_staging_ttl_ms: DEFAULT_MULTIPART_STAGING_TTL_MS,
            multipart_staging_max_bytes: DEFAULT_MULTIPART_STAGING_MAX_BYTES,
            debug: true,
            hooks: S3SessionHooks::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct S3RequestHead {
    pub method: String,
    pub target: String,
    pub headers: Vec<HeaderEntry>,
}

impl S3RequestHead {
    pub fn new(method: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            target: target.into(),
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push(HeaderEntry::new(name, value));
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct S3Request {
    pub head: S3RequestHead,
    pub body: Vec<u8>,
}

pub type S3RequestBody = Pin<Box<dyn Stream<Item = Result<Vec<u8>, String>> + Send>>;
pub type S3ResponseBodyStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, std::io::Error>> + Send>>;

fn add_stream_bytes(counter: &AtomicU64, bytes: usize) {
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(bytes))
    });
}

struct CountingRequestBody {
    inner: S3RequestBody,
    bytes: Arc<AtomicU64>,
}

impl Stream for CountingRequestBody {
    type Item = Result<Vec<u8>, String>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(chunk))) => {
                add_stream_bytes(&this.bytes, chunk.len());
                Poll::Ready(Some(Ok(chunk)))
            }
            other => other,
        }
    }
}

struct CountingResponseBody {
    inner: S3ResponseBodyStream,
    bytes: Arc<AtomicU64>,
}

impl Stream for CountingResponseBody {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(chunk))) => {
                add_stream_bytes(&this.bytes, chunk.len());
                Poll::Ready(Some(Ok(chunk)))
            }
            other => other,
        }
    }
}

pub enum S3StreamBody {
    Bytes(Vec<u8>),
    Stream(S3ResponseBodyStream),
}

pub struct S3StreamResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<S3StreamBody>,
}

impl From<S3Response> for S3StreamResponse {
    fn from(response: S3Response) -> Self {
        Self {
            status: response.status,
            headers: response.headers,
            body: Some(S3StreamBody::Bytes(response.body)),
        }
    }
}

impl S3Request {
    pub fn new(
        method: impl Into<String>,
        target: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        let body = body.into();
        let mut head = S3RequestHead::new(method, target);
        head = head.with_header("content-length", body.len().to_string());
        Self { head, body }
    }
}

#[derive(Debug, Clone, Default)]
pub struct S3SessionStats {
    pub requests: u64,
    pub replies: u64,
    pub errors: u64,
    pub operations: BTreeMap<String, u64>,
    pub assertions: u64,
    /// Total request handling time in whole milliseconds.
    pub duration_ms_total: u64,
    /// Maximum request handling time in whole milliseconds.
    pub duration_ms_max: u64,
    /// Bytes consumed from buffered and streaming request bodies. For a
    /// streaming body this includes the chunks consumed before an error or
    /// client disconnect.
    pub request_bytes: u64,
    /// Bytes delivered from buffered and streaming response bodies. For a
    /// streaming body this includes the chunks delivered before an error or
    /// client disconnect.
    pub response_bytes: u64,
    /// Error counts by a bounded operational class. The map can never contain
    /// user-controlled labels or an unbounded S3 error-code cardinality.
    pub error_classes: BTreeMap<S3ErrorClass, u64>,
}

/// Stable, bounded classes for S3 operational error metrics.
///
/// The gateway deliberately does not expose raw request paths, object keys, or
/// provider error messages as metric labels. Applications can map these
/// classes to their own exporter or alerting system without creating a
/// high-cardinality series for every object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum S3ErrorClass {
    Authentication,
    ConditionalConflict,
    Throttled,
    Client,
    Server,
}

fn classify_error(error: &S3Error) -> S3ErrorClass {
    match error.code.as_str() {
        "AccessDenied"
        | "AuthorizationHeaderMalformed"
        | "AuthorizationQueryParametersError"
        | "InvalidAccessKeyId"
        | "RequestTimeTooSkewed"
        | "SignatureDoesNotMatch" => S3ErrorClass::Authentication,
        "OperationAborted" | "PreconditionFailed" => S3ErrorClass::ConditionalConflict,
        "ServiceUnavailable" | "SlowDown" => S3ErrorClass::Throttled,
        _ if error.status >= 500 => S3ErrorClass::Server,
        _ => S3ErrorClass::Client,
    }
}

#[derive(Clone)]
pub struct S3Session {
    pub buckets: Arc<BTreeMap<String, Arc<dyn FsDriver>>>,
    pub options: S3SessionOptions,
    stats: Arc<Mutex<S3SessionStats>>,
    stream_request_bytes: Arc<AtomicU64>,
    stream_response_bytes: Arc<AtomicU64>,
    assertions: Arc<StdMutex<Vec<String>>>,
    inflight: Arc<StdMutex<HashSet<u64>>>,
    next_ticket: Arc<std::sync::atomic::AtomicU64>,
    next_request_id: Arc<AtomicU64>,
    conditional_put_locks: Arc<ConditionalPutLockRegistry>,
}

impl S3Session {
    pub fn new<D>(driver: D) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::new_with_options(driver, S3SessionOptions::default())
    }

    pub fn new_with_options<D>(driver: D, options: S3SessionOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        let mut buckets = BTreeMap::new();
        buckets.insert(
            DEFAULT_BUCKET.to_owned(),
            Arc::new(driver) as Arc<dyn FsDriver>,
        );
        Self::from_buckets_with_options(buckets, options)
    }

    pub fn from_buckets(buckets: impl IntoIterator<Item = (String, Arc<dyn FsDriver>)>) -> Self {
        Self::from_buckets_with_options(buckets, S3SessionOptions::default())
    }

    pub fn from_buckets_with_options(
        buckets: impl IntoIterator<Item = (String, Arc<dyn FsDriver>)>,
        mut options: S3SessionOptions,
    ) -> Self {
        if options.multipart_staging_ttl_ms <= 0 {
            options.multipart_staging_ttl_ms = DEFAULT_MULTIPART_STAGING_TTL_MS;
        }
        if options.multipart_staging_max_bytes == 0 {
            options.multipart_staging_max_bytes = DEFAULT_MULTIPART_STAGING_MAX_BYTES;
        }
        Self {
            buckets: Arc::new(buckets.into_iter().collect()),
            options,
            stats: Arc::new(Mutex::new(S3SessionStats::default())),
            stream_request_bytes: Arc::new(AtomicU64::new(0)),
            stream_response_bytes: Arc::new(AtomicU64::new(0)),
            assertions: Arc::new(StdMutex::new(Vec::new())),
            inflight: Arc::new(StdMutex::new(HashSet::new())),
            next_ticket: Arc::new(AtomicU64::new(1)),
            next_request_id: Arc::new(AtomicU64::new(1)),
            conditional_put_locks: Arc::new(ConditionalPutLockRegistry::new()),
        }
    }

    pub async fn stats(&self) -> S3SessionStats {
        let mut snapshot = self.stats.lock().await.clone();
        snapshot.request_bytes = snapshot
            .request_bytes
            .saturating_add(self.stream_request_bytes.load(Ordering::Relaxed));
        snapshot.response_bytes = snapshot
            .response_bytes
            .saturating_add(self.stream_response_bytes.load(Ordering::Relaxed));
        snapshot
    }

    pub fn assertions(&self) -> Vec<String> {
        self.assertions
            .lock()
            .map(|assertions| assertions.clone())
            .unwrap_or_default()
    }

    pub fn bucket_names(&self) -> Vec<String> {
        let mut names = self.buckets.keys().cloned().collect::<Vec<_>>();
        names.sort_by(|left, right| compare_utf8(left, right));
        names
    }

    pub async fn handle(&self, request: S3Request) -> S3Response {
        self.handle_request(request.head, request.body).await
    }

    /// Answer exactly one in-process request. Errors are converted to an S3
    /// response rather than escaping to the HTTP layer.
    pub async fn handle_request(&self, head: S3RequestHead, body: Vec<u8>) -> S3Response {
        let started = Instant::now();
        {
            let mut stats = self.stats.lock().await;
            stats.requests += 1;
        }
        let ticket = self.begin_request();
        let mut ticket_guard = RequestTicketGuard::new(Arc::clone(&self.inflight), ticket);
        let request_id = self.next_request_id();
        let result = self.dispatch(&head, &body).await;
        let (response, error_class) = match result {
            Ok(mut response) => {
                response
                    .headers
                    .push(("x-amz-request-id".to_owned(), request_id));
                if head.method.eq_ignore_ascii_case("HEAD") {
                    response.body.clear();
                }
                (response, None)
            }
            Err(error) => {
                self.report_error(&error, &head);
                let s3_error = error.error();
                let error_class = classify_error(&s3_error);
                (
                    error_response(&s3_error, &request_id, error.resource()),
                    Some(error_class),
                )
            }
        };
        self.finish_request(ticket, &head).await;
        ticket_guard.disarm();
        self.record_response_stats(
            started,
            body.len() as u64,
            response.body.len() as u64,
            response.status,
            error_class,
        )
        .await;
        response
    }

    /// Answer one HTTP request while consuming its body incrementally.
    ///
    /// The in-process [`handle_request`] API intentionally remains a simple
    /// byte-buffer API.  The Axum boundary uses this sibling entry point so
    /// uploads are handed to the driver as the HTTP body arrives and object
    /// downloads are pulled from the driver only as Axum asks for more data.
    pub async fn handle_request_stream(
        &self,
        head: S3RequestHead,
        body: S3RequestBody,
    ) -> S3StreamResponse {
        let started = Instant::now();
        {
            let mut stats = self.stats.lock().await;
            stats.requests += 1;
        }
        let ticket = self.begin_request();
        let mut ticket_guard = RequestTicketGuard::new(Arc::clone(&self.inflight), ticket);
        let request_id = self.next_request_id();
        let body = Box::pin(CountingRequestBody {
            inner: body,
            bytes: Arc::clone(&self.stream_request_bytes),
        });
        let result = self.dispatch_stream(&head, body).await;
        let (mut response, error_class) = match result {
            Ok(mut response) => {
                response
                    .headers
                    .push(("x-amz-request-id".to_owned(), request_id));
                if head.method.eq_ignore_ascii_case("HEAD") {
                    response.body = None;
                }
                (response, None)
            }
            Err(error) => {
                self.report_error(&error, &head);
                let s3_error = error.error();
                let error_class = classify_error(&s3_error);
                (
                    S3StreamResponse::from(error_response(
                        &s3_error,
                        &request_id,
                        error.resource(),
                    )),
                    Some(error_class),
                )
            }
        };
        response.body = match response.body.take() {
            Some(S3StreamBody::Stream(stream)) => {
                Some(S3StreamBody::Stream(Box::pin(CountingResponseBody {
                    inner: stream,
                    bytes: Arc::clone(&self.stream_response_bytes),
                })))
            }
            body => body,
        };
        self.finish_request(ticket, &head).await;
        ticket_guard.disarm();
        let response_bytes = match &response.body {
            Some(S3StreamBody::Bytes(bytes)) => bytes.len() as u64,
            Some(S3StreamBody::Stream(_)) | None => 0,
        };
        self.record_response_stats(started, 0, response_bytes, response.status, error_class)
            .await;
        // Keep the response owned by this function until after statistics are
        // updated.  This also makes the error path mirror handle_request.
        response
    }

    fn begin_request(&self) -> Option<u64> {
        if !self.options.debug {
            return None;
        }
        let ticket = self
            .next_ticket
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.insert(ticket);
        }
        Some(ticket)
    }

    async fn finish_request(&self, ticket: Option<u64>, head: &S3RequestHead) {
        let Some(ticket) = ticket else {
            return;
        };
        let removed = self
            .inflight
            .lock()
            .map(|mut inflight| inflight.remove(&ticket))
            .unwrap_or(false);
        if !removed {
            let message = format!("{} {} was answered twice", head.method, head.target);
            if let Ok(mut assertions) = self.assertions.lock() {
                assertions.push(message.clone());
            }
            if let Some(hook) = &self.options.hooks.on_assertion {
                hook(message);
            }
            let mut stats = self.stats.lock().await;
            stats.assertions += 1;
        }
    }

    fn current_now_ms(&self) -> i64 {
        self.options
            .hooks
            .now_ms
            .as_ref()
            .and_then(|hook| hook())
            .unwrap_or_else(now_ms)
    }

    fn next_request_id(&self) -> String {
        self.options
            .hooks
            .request_id
            .as_ref()
            .and_then(|hook| hook())
            .filter(|request_id| !request_id.is_empty())
            .unwrap_or_else(|| {
                let request_id = self
                    .next_request_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                format!("mountx-{request_id:016x}")
            })
    }

    fn report_error(&self, error: &S3Failure, head: &S3RequestHead) {
        if let Some(hook) = &self.options.hooks.on_error {
            hook(error.to_string(), Some(head.clone()));
        }
    }

    async fn record_response_stats(
        &self,
        started: Instant,
        request_bytes: u64,
        response_bytes: u64,
        status: u16,
        error_class: Option<S3ErrorClass>,
    ) {
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let mut stats = self.stats.lock().await;
        stats.replies += 1;
        stats.duration_ms_total = stats.duration_ms_total.saturating_add(elapsed_ms);
        stats.duration_ms_max = stats.duration_ms_max.max(elapsed_ms);
        stats.request_bytes = stats.request_bytes.saturating_add(request_bytes);
        stats.response_bytes = stats.response_bytes.saturating_add(response_bytes);
        if status >= 400 {
            stats.errors += 1;
        }
        if let Some(error_class) = error_class {
            *stats.error_classes.entry(error_class).or_default() += 1;
        }
    }

    async fn dispatch_stream(
        &self,
        head: &S3RequestHead,
        mut body: S3RequestBody,
    ) -> S3Result<S3StreamResponse> {
        let target = parse_request_target(&head.target)?;
        let verified = self
            .authorize_stream(head, &target)
            .map_err(|error| error.with_resource(target.path.clone()))?;
        let operation = protocol::route_request(&head.method, &target, &head.headers)
            .map_err(|error| error.with_resource(target.path.clone()))?;
        let streamable = matches!(
            &operation,
            Operation::GetObject(_)
                | Operation::HeadObject(_)
                | Operation::PutObject(_)
                | Operation::UploadPart { .. }
        );
        if !streamable {
            let buffered = collect_request_body(&mut body, self.options.max_body_bytes).await?;
            return self
                .dispatch(head, &buffered)
                .await
                .map(S3StreamResponse::from);
        }

        self.count_operation(operation_name(&operation)).await;
        let bucket_name = operation_bucket(&operation);
        let driver = if let Some(bucket) = bucket_name {
            self.buckets
                .get(bucket)
                .ok_or_else(|| S3Failure::s3("NoSuchBucket").with_resource(target.path.clone()))?
                .clone()
        } else {
            self.buckets
                .values()
                .next()
                .cloned()
                .ok_or_else(|| S3Failure::s3("NoSuchBucket"))?
        };
        match operation {
            Operation::GetObject(target) => {
                self.get_object_stream(driver, &target, head, false).await
            }
            Operation::HeadObject(target) => {
                self.get_object_stream(driver, &target, head, true).await
            }
            Operation::PutObject(target) => {
                self.put_object_stream(driver, &target, head, body, verified.as_ref())
                    .await
            }
            Operation::UploadPart {
                target,
                upload_id,
                part_number,
            } => {
                self.upload_part_stream(
                    driver,
                    &target,
                    &upload_id,
                    part_number,
                    StreamUploadBody {
                        head,
                        body,
                        verified: verified.as_ref(),
                    },
                )
                .await
            }
            _ => unreachable!("streamability was checked above"),
        }
    }

    async fn dispatch(&self, head: &S3RequestHead, body: &[u8]) -> S3Result<S3Response> {
        if body.len() > self.options.max_body_bytes {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let target = parse_request_target(&head.target)?;
        let verified = self
            .authorize(head, &target, body)
            .map_err(|error| error.with_resource(target.path.clone()))?;
        let operation = protocol::route_request(&head.method, &target, &head.headers)
            .map_err(|error| error.with_resource(target.path.clone()))?;
        self.count_operation(operation_name(&operation)).await;
        if matches!(&operation, Operation::DeleteObject(target) if target.path.is_empty()) {
            return Ok(S3Response::empty(204));
        }
        let bucket_name = operation_bucket(&operation);
        let driver = if let Some(bucket) = bucket_name {
            self.buckets
                .get(bucket)
                .ok_or_else(|| S3Failure::s3("NoSuchBucket").with_resource(target.path.clone()))?
                .clone()
        } else {
            self.buckets
                .values()
                .next()
                .cloned()
                .ok_or_else(|| S3Failure::s3("NoSuchBucket"))?
        };
        match operation {
            Operation::ListBuckets => self.list_buckets().await,
            Operation::HeadBucket { .. } => Ok(S3Response::empty(200).header(
                "x-amz-bucket-region",
                self.options
                    .region
                    .clone()
                    .unwrap_or_else(|| "us-east-1".to_owned()),
            )),
            Operation::ListObjectsV2 {
                bucket,
                prefix,
                delimiter,
                max_keys,
                continuation_token,
                start_after,
                fetch_owner,
                encoding_type_url,
            } => {
                self.list_objects(ListObjectsRequest {
                    bucket: &bucket,
                    driver,
                    prefix: &prefix,
                    delimiter: delimiter.as_deref(),
                    max_keys,
                    continuation: continuation_token.as_deref(),
                    start_after: start_after.as_deref(),
                    fetch_owner,
                    encoding_url: encoding_type_url,
                })
                .await
            }
            Operation::GetObject(target) => self.get_object(driver, &target, head, false).await,
            Operation::HeadObject(target) => self.get_object(driver, &target, head, true).await,
            Operation::PutObject(target) => {
                self.put_object(driver, &target, head, body, verified.as_ref())
                    .await
            }
            Operation::DeleteObject(target) => self.delete_object(driver, &target).await,
            Operation::DeleteObjects { .. } => {
                self.delete_objects(driver, head, body, verified.as_ref())
                    .await
            }
            Operation::CopyObject {
                destination,
                source,
                replace_metadata,
            } => {
                self.copy_object(driver, &destination, &source, replace_metadata, head)
                    .await
            }
            Operation::CreateMultipartUpload(target) => {
                self.create_multipart(driver, &target, head).await
            }
            Operation::UploadPart {
                target,
                upload_id,
                part_number,
            } => {
                self.upload_part(
                    driver,
                    &target,
                    &upload_id,
                    part_number,
                    UploadBody {
                        head,
                        body,
                        verified: verified.as_ref(),
                    },
                )
                .await
            }
            Operation::CompleteMultipartUpload { target, upload_id } => {
                self.complete_multipart(
                    driver,
                    &target,
                    &upload_id,
                    UploadBody {
                        head,
                        body,
                        verified: verified.as_ref(),
                    },
                )
                .await
            }
            Operation::AbortMultipartUpload { target, upload_id } => {
                self.abort_multipart(driver, &target, &upload_id).await
            }
            Operation::ListParts {
                target,
                upload_id,
                max_parts,
                marker,
            } => {
                self.list_parts(driver, &target, &upload_id, max_parts, marker)
                    .await
            }
        }
    }

    fn authorize(
        &self,
        head: &S3RequestHead,
        target: &protocol::ParsedTarget,
        body: &[u8],
    ) -> S3Result<Option<sigv4::VerifiedRequest>> {
        let Some(credentials) = &self.options.credentials else {
            return Ok(None);
        };
        sigv4::verify_request(sigv4::VerifyRequest {
            method: &head.method,
            path: &target.path,
            query: &target.query,
            headers: &head.headers,
            body,
            credentials,
            expected_region: self.options.region.as_deref(),
            now_ms: self.current_now_ms(),
        })
        .map(Some)
        .map_err(|failure| {
            S3Failure::S3(sigv4_error(
                failure,
                target
                    .query
                    .iter()
                    .any(|entry| entry.name.starts_with("X-Amz-")),
            ))
        })
    }

    fn authorize_stream(
        &self,
        head: &S3RequestHead,
        target: &protocol::ParsedTarget,
    ) -> S3Result<Option<sigv4::VerifiedRequest>> {
        let Some(credentials) = &self.options.credentials else {
            return Ok(None);
        };
        sigv4::verify_request_without_body(sigv4::VerifyRequest {
            method: &head.method,
            path: &target.path,
            query: &target.query,
            headers: &head.headers,
            body: &[],
            credentials,
            expected_region: self.options.region.as_deref(),
            now_ms: self.current_now_ms(),
        })
        .map(Some)
        .map_err(|failure| {
            S3Failure::S3(sigv4_error(
                failure,
                target
                    .query
                    .iter()
                    .any(|entry| entry.name.starts_with("X-Amz-")),
            ))
        })
    }

    async fn count_operation(&self, operation: &str) {
        let mut stats = self.stats.lock().await;
        *stats.operations.entry(operation.to_owned()).or_default() += 1;
    }

    async fn acquire_conditional_put_lock(
        &self,
        target: &ObjectTarget,
        headers: &[HeaderEntry],
    ) -> Option<ConditionalPutLockGuard> {
        if !has_put_conditionals(headers) {
            return None;
        }
        let key = format!("{}\0{}", target.bucket, target.path);
        Some(self.conditional_put_locks.acquire(key).await)
    }

    async fn list_buckets(&self) -> S3Result<S3Response> {
        let mut buckets = Vec::new();
        for name in self.bucket_names() {
            let date = match self
                .buckets
                .get(&name)
                .expect("bucket name came from map")
                .stat("/")
                .await
            {
                Ok(stats) => format_iso_date(stats.mtime_ms),
                Err(_) => format_iso_date(0),
            };
            buckets.push((name, date));
        }
        Ok(xml_response(200, list_buckets_xml(&buckets), ""))
    }

    async fn get_object(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
        is_head: bool,
    ) -> S3Result<S3Response> {
        let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
        let is_directory = stats.file_type() == FileType::Directory;
        if target.directory != is_directory || (!target.directory && !stats.is_file()) {
            return Err(S3Failure::s3("NoSuchKey"));
        }
        let etag = object_etag(&stats);
        if let Some(status) = evaluate_get_conditionals(&stats, &etag, &head.headers, &head.method)
        {
            let mut response = S3Response::empty(status);
            response
                .headers
                .extend(object_conditional_headers(&etag, stats.mtime_ms));
            return Ok(response);
        }
        let mut selected_range = None;
        if let Some(raw_range) = header_value(&head.headers, "range") {
            let range = protocol::parse_range(&raw_range, stats.size);
            if let Some(if_range) = header_value(&head.headers, "if-range") {
                if if_range_matches(&if_range, &etag, stats.mtime_ms) {
                    selected_range = range;
                }
            } else {
                selected_range = range;
            }
            if range.is_none() && header_value(&head.headers, "if-range").is_none() {
                return Ok(S3Response::empty(416)
                    .header("content-range", format!("bytes */{}", stats.size)));
            }
        }
        let status = if selected_range.is_some() { 206 } else { 200 };
        let mut response = S3Response::empty(status);
        response.headers.extend(protocol::object_headers(
            &etag,
            if target.directory { 0 } else { stats.size },
            stats.mtime_ms,
            selected_range,
            stats.size,
        ));
        if !is_head && !target.directory {
            response.body = read_bytes_range(
                driver,
                &target.path,
                selected_range,
                stats.size,
                self.options.read_chunk_bytes,
            )
            .await?;
        }
        Ok(response)
    }

    async fn get_object_stream(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
        is_head: bool,
    ) -> S3Result<S3StreamResponse> {
        let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
        let is_directory = stats.file_type() == FileType::Directory;
        if target.directory != is_directory || (!target.directory && !stats.is_file()) {
            return Err(S3Failure::s3("NoSuchKey"));
        }
        let etag = object_etag(&stats);
        if let Some(status) = evaluate_get_conditionals(&stats, &etag, &head.headers, &head.method)
        {
            return Ok(S3StreamResponse {
                status,
                headers: object_conditional_headers(&etag, stats.mtime_ms),
                body: None,
            });
        }
        let mut selected_range = None;
        if let Some(raw_range) = header_value(&head.headers, "range") {
            let range = protocol::parse_range(&raw_range, stats.size);
            if let Some(if_range) = header_value(&head.headers, "if-range") {
                if if_range_matches(&if_range, &etag, stats.mtime_ms) {
                    selected_range = range;
                }
            } else {
                selected_range = range;
            }
            if range.is_none() && header_value(&head.headers, "if-range").is_none() {
                return Ok(S3StreamResponse {
                    status: 416,
                    headers: vec![(
                        "content-range".to_owned(),
                        format!("bytes */{}", stats.size),
                    )],
                    body: None,
                });
            }
        }
        let status = if selected_range.is_some() { 206 } else { 200 };
        let mut response = S3StreamResponse {
            status,
            headers: protocol::object_headers(
                &etag,
                if target.directory { 0 } else { stats.size },
                stats.mtime_ms,
                selected_range,
                stats.size,
            ),
            body: None,
        };
        if !is_head && !target.directory && stats.size > 0 {
            let range_size = selected_range.map_or(stats.size, |range| range.end - range.start + 1);
            let start = selected_range.map_or(0, |range| range.start);
            response.body = Some(S3StreamBody::Stream(stream_file(
                driver,
                &target.path,
                start,
                range_size,
                self.options.read_chunk_bytes,
            )));
        }
        Ok(response)
    }

    async fn put_object(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
        body: &[u8],
        verified: Option<&sigv4::VerifiedRequest>,
    ) -> S3Result<S3Response> {
        if !aws_chunked_body(&head.headers) {
            validate_declared_length(&head.headers, body.len())?;
        }
        let _conditional_lock = self
            .acquire_conditional_put_lock(target, &head.headers)
            .await;
        let existing = driver.stat(&target.path).await.ok();
        check_put_conditionals(existing.as_ref(), &head.headers)?;
        let body = decode_request_body(
            &head.headers,
            body,
            self.options.max_body_bytes,
            verified,
            self.options.credentials.as_ref(),
        )?;
        let requested_mtime =
            parse_meta_mtime(header_value(&head.headers, "x-amz-meta-mtime").as_deref());
        if target.directory {
            if !body.is_empty() {
                return Err(S3Failure::s3("InvalidRequest"));
            }
            driver
                .mkdir(
                    &target.path,
                    MkdirOptions {
                        recursive: true,
                        mode: Some(0o777),
                    },
                )
                .await
                .map_err(S3Failure::Fs)?;
            if let Some(mtime) = requested_mtime {
                apply_mtime(&driver, &target.path, mtime).await?;
            }
            durability_barrier(&driver).await?;
            let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
            return Ok(S3Response::empty(200)
                .header("etag", protocol::etag_header(&object_etag(&stats)))
                .header("content-length", "0"));
        }
        ensure_parent(&driver, &target.path).await?;
        write_bytes(
            &driver,
            &target.path,
            &body,
            existing.is_none() && check_create_only(&head.headers),
        )
        .await?;
        if let Some(mtime) = requested_mtime {
            apply_mtime(&driver, &target.path, mtime).await?;
        }
        durability_barrier(&driver).await?;
        let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
        Ok(S3Response::empty(200)
            .header("etag", protocol::etag_header(&object_etag(&stats)))
            .header("content-length", "0"))
    }

    async fn put_object_stream(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
        body: S3RequestBody,
        verified: Option<&sigv4::VerifiedRequest>,
    ) -> S3Result<S3StreamResponse> {
        let _conditional_lock = self
            .acquire_conditional_put_lock(target, &head.headers)
            .await;
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        let existing = driver.stat(&target.path).await.ok();
        check_put_conditionals(existing.as_ref(), &head.headers)?;
        let requested_mtime =
            parse_meta_mtime(header_value(&head.headers, "x-amz-meta-mtime").as_deref());
        if target.directory {
            consume_stream_body(
                body,
                &head.headers,
                self.options.max_body_bytes,
                verified,
                self.options.credentials.as_ref(),
            )
            .await?;
            driver
                .mkdir(
                    &target.path,
                    MkdirOptions {
                        recursive: true,
                        mode: Some(0o777),
                    },
                )
                .await
                .map_err(S3Failure::Fs)?;
            if let Some(mtime) = requested_mtime {
                apply_mtime(&driver, &target.path, mtime).await?;
            }
            durability_barrier(&driver).await?;
            let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
            return Ok(S3StreamResponse::from(
                S3Response::empty(200)
                    .header("etag", protocol::etag_header(&object_etag(&stats)))
                    .header("content-length", "0"),
            ));
        }
        require_atomic_rename(&driver)?;
        // Stage at the private root, but create the destination hierarchy
        // before the final rename. A rename cannot create a missing parent and
        // nested object keys are valid S3 keys.
        ensure_parent(&driver, &target.path).await?;
        let staging_quota =
            staging_quota(&driver, self.options.multipart_staging_max_bytes, None).await?;
        let exclusive = existing.is_none() && check_create_only(&head.headers);
        let staging_path = format!("/{STREAMING_STAGING_PREFIX}{}", new_upload_id());
        let mut cleanup = StagedPathCleanup::new(Arc::clone(&driver), staging_path.clone());
        let write_result = write_stream_body(StreamWriteRequest {
            driver: &driver,
            path: &staging_path,
            headers: &head.headers,
            body,
            verified,
            credentials: self.options.credentials.as_ref(),
            max_body_bytes: self.options.max_body_bytes,
            exclusive: false,
            create_parent: true,
            cleanup_on_error: true,
            staging_quota: Some(staging_quota),
        })
        .await;
        if let Err(error) = write_result {
            cleanup.cleanup_now().await;
            return Err(error);
        }
        if exclusive {
            match driver.stat(&target.path).await {
                Ok(_) => {
                    cleanup.cleanup_now().await;
                    return Err(S3Failure::s3("PreconditionFailed"));
                }
                Err(error) if error.code == mount_rs_core::ErrorCode::Enoent => {}
                Err(error) => {
                    cleanup.cleanup_now().await;
                    return Err(S3Failure::Fs(error));
                }
            }
        }
        if let Err(error) = driver.rename(&staging_path, &target.path).await {
            cleanup.cleanup_now().await;
            return Err(S3Failure::Fs(error));
        }
        cleanup.disarm();
        if let Some(mtime) = requested_mtime {
            apply_mtime(&driver, &target.path, mtime).await?;
        }
        durability_barrier(&driver).await?;
        let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
        Ok(S3StreamResponse::from(
            S3Response::empty(200)
                .header("etag", protocol::etag_header(&object_etag(&stats)))
                .header("content-length", "0"),
        ))
    }

    async fn delete_object(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
    ) -> S3Result<S3Response> {
        let stats = match driver.stat(&target.path).await {
            Ok(stats) => stats,
            Err(error)
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                return Ok(S3Response::empty(204));
            }
            Err(error) => return Err(S3Failure::Fs(error)),
        };
        let is_directory = stats.file_type() == FileType::Directory;
        if target.directory {
            if !is_directory {
                return Ok(S3Response::empty(204));
            }
            driver.rmdir(&target.path).await.map_err(S3Failure::Fs)?;
        } else {
            if is_directory {
                return Ok(S3Response::empty(204));
            }
            driver.unlink(&target.path).await.map_err(S3Failure::Fs)?;
        }
        durability_barrier(&driver).await?;
        Ok(S3Response::empty(204))
    }

    async fn delete_objects(
        &self,
        driver: Arc<dyn FsDriver>,
        head: &S3RequestHead,
        body: &[u8],
        verified: Option<&sigv4::VerifiedRequest>,
    ) -> S3Result<S3Response> {
        if !aws_chunked_body(&head.headers) {
            validate_declared_length(&head.headers, body.len())?;
        }
        let body = decode_request_body(
            &head.headers,
            body,
            self.options.max_body_bytes,
            verified,
            self.options.credentials.as_ref(),
        )?;
        if let Some(content_md5) = header_md5(&head.headers) {
            let digest = Md5::digest(&body);
            let expected = BASE64.encode(digest);
            if expected != content_md5 {
                return Err(S3Failure::s3("BadDigest"));
            }
        }
        let (keys, quiet) = parse_delete_document(&body, self.options.max_xml_bytes)?;
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        let mut deleted = Vec::new();
        let mut errors = Vec::new();
        for key in keys {
            if is_staging_key(&key) {
                match delete_staging_key(&driver, &key).await {
                    Ok(()) => deleted.push(key),
                    Err(error) => {
                        self.report_error(&error, head);
                        errors.push((key, error.error()));
                    }
                }
                continue;
            }
            let target = match parse_object_key("", &key) {
                Ok(target) => target,
                Err(error) => {
                    errors.push((key, error.error()));
                    continue;
                }
            };
            match self.delete_object(driver.clone(), &target).await {
                Ok(_) => deleted.push(key),
                Err(error) => {
                    self.report_error(&error, head);
                    errors.push((key, error.error()));
                }
            }
        }
        if !deleted.is_empty() {
            durability_barrier(&driver).await?;
        }
        Ok(xml_response(
            200,
            delete_result_xml(&deleted, &errors, quiet),
            "",
        ))
    }

    async fn copy_object(
        &self,
        destination_driver: Arc<dyn FsDriver>,
        destination: &ObjectTarget,
        source: &ObjectTarget,
        replace_metadata: bool,
        head: &S3RequestHead,
    ) -> S3Result<S3Response> {
        if destination.directory {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        let source_driver = self
            .buckets
            .get(&source.bucket)
            .ok_or_else(|| S3Failure::s3("NoSuchBucket"))?
            .clone();
        let source_stats = source_driver
            .stat(&source.path)
            .await
            .map_err(S3Failure::Fs)?;
        if !source_stats.is_file() || source.directory {
            return Err(S3Failure::s3("NoSuchKey"));
        }
        let source_etag = object_etag(&source_stats);
        check_copy_conditionals(&source_stats, &source_etag, &head.headers)?;
        let same_object = source.bucket == destination.bucket && source.path == destination.path;
        if same_object && !replace_metadata {
            return Err(S3Failure::S3(protocol::error_with_message(
                "InvalidRequest",
                "This copy request is illegal because you are trying to copy an object to itself without changing the metadata.",
            )));
        }
        require_atomic_rename(&destination_driver)?;
        ensure_parent(&destination_driver, &destination.path).await?;
        let mtime = if replace_metadata {
            parse_meta_mtime(header_value(&head.headers, "x-amz-meta-mtime").as_deref())
                .unwrap_or_else(|| self.current_now_ms())
        } else {
            source_stats.mtime_ms
        };
        let staging_path = format!("/{STREAMING_STAGING_PREFIX}{}", new_upload_id());
        let mut cleanup =
            StagedPathCleanup::new(Arc::clone(&destination_driver), staging_path.clone());
        let copy_result: S3Result<()> = async {
            let destination_handle = destination_driver
                .open(&staging_path, "w", 0o666)
                .await
                .map_err(S3Failure::Fs)?;
            let mut destination_position = 0_u64;
            let copy_result = copy_file_between_drivers(
                &source_driver,
                &source.path,
                &destination_handle,
                source_stats.size,
                self.options.read_chunk_bytes,
                &mut destination_position,
            )
            .await;
            let close_result = destination_handle.close().await.map_err(S3Failure::Fs);
            copy_result.and(close_result)?;
            apply_mtime(&destination_driver, &staging_path, mtime).await?;
            destination_driver
                .rename(&staging_path, &destination.path)
                .await
                .map_err(S3Failure::Fs)
        }
        .await;
        if let Err(error) = copy_result {
            cleanup.cleanup_now().await;
            return Err(error);
        }
        cleanup.disarm();
        durability_barrier(&destination_driver).await?;
        let destination_stats = destination_driver
            .stat(&destination.path)
            .await
            .map_err(S3Failure::Fs)?;
        Ok(xml_response(
            200,
            protocol::copy_object_xml(
                &object_etag(&destination_stats),
                &format_iso_date(destination_stats.mtime_ms),
            ),
            "",
        ))
    }

    async fn list_objects(&self, request: ListObjectsRequest<'_>) -> S3Result<S3Response> {
        let ListObjectsRequest {
            bucket,
            driver,
            prefix,
            delimiter,
            max_keys,
            continuation,
            start_after,
            fetch_owner,
            encoding_url,
        } = request;
        if !listable_prefix(prefix) {
            return Ok(xml_response(
                200,
                list_objects_xml(ListObjectsXml {
                    bucket,
                    prefix,
                    delimiter,
                    continuation,
                    next: None,
                    start_after,
                    max_keys,
                    key_count: 0,
                    truncated: false,
                    encoding_url,
                    objects: &[],
                    common_prefixes: &[],
                }),
                "",
            ));
        }
        let after = if let Some(token) = continuation {
            decode_continuation_token(token).ok_or_else(|| {
                S3Failure::S3(protocol::error_with_message(
                    "InvalidArgument",
                    "The continuation token provided is incorrect",
                ))
            })?
        } else {
            start_after.unwrap_or_default().to_owned()
        };
        let (page, truncated) =
            collect_listing_page(driver.clone(), prefix, &after, delimiter, max_keys).await?;
        let next = truncated
            .then(|| {
                page.last()
                    .map(|candidate| encode_continuation_token(candidate.key()))
            })
            .flatten();
        let mut objects = Vec::new();
        let mut prefixes = Vec::new();
        for candidate in page {
            match candidate {
                ListCandidate::Prefix(prefix) => prefixes.push(prefix),
                ListCandidate::Object { key, stats, .. } => objects.push(ListedObject {
                    key,
                    last_modified: format_iso_date(stats.mtime_ms),
                    etag: object_etag(&stats),
                    size: if stats.is_directory() { 0 } else { stats.size },
                    owner: fetch_owner,
                }),
            }
        }
        Ok(xml_response(
            200,
            list_objects_xml(ListObjectsXml {
                bucket,
                prefix,
                delimiter,
                continuation,
                next: next.as_deref(),
                start_after,
                max_keys,
                key_count: objects.len() + prefixes.len(),
                truncated,
                encoding_url,
                objects: &objects,
                common_prefixes: &prefixes,
            }),
            "",
        ))
    }

    async fn create_multipart(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
    ) -> S3Result<S3Response> {
        if target.directory {
            return Err(S3Failure::S3(protocol::error_with_message(
                "InvalidRequest",
                "A key ending in / names a directory and cannot be uploaded in parts.",
            )));
        }
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        ensure_staging_capacity(&driver, self.options.multipart_staging_max_bytes, None, 0).await?;
        let upload_id = new_upload_id();
        let directory = upload_directory(&upload_id);
        driver
            .mkdir(
                &directory,
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o700),
                },
            )
            .await
            .map_err(S3Failure::Fs)?;
        let manifest = UploadManifest {
            key: target.key.clone(),
            mtime_ms: parse_meta_mtime(header_value(&head.headers, "x-amz-meta-mtime").as_deref()),
        };
        let bytes = serde_json::to_vec(&manifest)
            .map_err(|error| S3Failure::Internal(error.to_string()))?;
        write_bytes(
            &driver,
            &format!("{directory}/{MANIFEST_NAME}"),
            &bytes,
            false,
        )
        .await?;
        if let Err(error) =
            ensure_staging_capacity(&driver, self.options.multipart_staging_max_bytes, None, 0)
                .await
        {
            let _ = remove_tree(&driver, &directory).await;
            return Err(error);
        }
        durability_barrier(&driver).await?;
        Ok(xml_response(
            200,
            initiate_multipart_xml(&target.bucket, &target.key, &upload_id),
            "",
        ))
    }

    async fn upload_part(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
        part_number: u32,
        request: UploadBody<'_>,
    ) -> S3Result<S3Response> {
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        let _manifest = read_manifest(&driver, upload_id, &target.key).await?;
        ensure_upload_not_finalizing(&driver, upload_id).await?;
        if !aws_chunked_body(&request.head.headers) {
            validate_declared_length(&request.head.headers, request.body.len())?;
        }
        let body = decode_request_body(
            &request.head.headers,
            request.body,
            self.options.max_body_bytes,
            request.verified,
            self.options.credentials.as_ref(),
        )?;
        if body.len() as u64 > MAX_PART_SIZE {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let path = part_path(upload_id, part_number);
        require_atomic_rename(&driver)?;
        ensure_staging_capacity(
            &driver,
            self.options.multipart_staging_max_bytes,
            None,
            body.len() as u64,
        )
        .await?;
        ensure_upload_not_finalizing(&driver, upload_id).await?;
        let staging_path = part_staging_path(upload_id);
        let mut cleanup = StagedPathCleanup::new(Arc::clone(&driver), staging_path.clone());
        let result: S3Result<()> = async {
            write_bytes(&driver, &staging_path, &body, true).await?;
            ensure_upload_not_finalizing(&driver, upload_id).await?;
            driver
                .rename(&staging_path, &path)
                .await
                .map_err(S3Failure::Fs)
        }
        .await;
        if let Err(error) = result {
            cleanup.cleanup_now().await;
            return Err(match error {
                S3Failure::Fs(ref fs_error)
                    if matches!(
                        fs_error.code,
                        mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                    ) =>
                {
                    S3Failure::s3("NoSuchUpload")
                }
                other => other,
            });
        }
        cleanup.disarm();
        durability_barrier(&driver).await?;
        let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
        Ok(S3Response::empty(200)
            .header("etag", protocol::etag_header(&object_etag(&stats)))
            .header("content-length", "0"))
    }

    async fn upload_part_stream(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
        part_number: u32,
        request: StreamUploadBody<'_>,
    ) -> S3Result<S3StreamResponse> {
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        let _manifest = read_manifest(&driver, upload_id, &target.key).await?;
        ensure_upload_not_finalizing(&driver, upload_id).await?;
        let path = part_path(upload_id, part_number);
        require_atomic_rename(&driver)?;
        let staging_path = part_staging_path(upload_id);
        let staging_quota =
            staging_quota(&driver, self.options.multipart_staging_max_bytes, None).await?;
        let max_part_bytes = self
            .options
            .max_body_bytes
            .min(usize::try_from(MAX_PART_SIZE).unwrap_or(usize::MAX));
        let mut cleanup = StagedPathCleanup::new(Arc::clone(&driver), staging_path.clone());
        let result: S3Result<()> = async {
            write_stream_body(StreamWriteRequest {
                driver: &driver,
                path: &staging_path,
                headers: &request.head.headers,
                body: request.body,
                verified: request.verified,
                credentials: self.options.credentials.as_ref(),
                max_body_bytes: max_part_bytes,
                exclusive: true,
                create_parent: false,
                cleanup_on_error: true,
                staging_quota: Some(staging_quota),
            })
            .await?;
            ensure_upload_not_finalizing(&driver, upload_id).await?;
            driver
                .rename(&staging_path, &path)
                .await
                .map_err(S3Failure::Fs)
        }
        .await;
        if let Err(error) = result {
            cleanup.cleanup_now().await;
            return Err(match error {
                S3Failure::Fs(ref fs_error)
                    if matches!(
                        fs_error.code,
                        mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                    ) =>
                {
                    S3Failure::s3("NoSuchUpload")
                }
                other => other,
            });
        }
        cleanup.disarm();
        durability_barrier(&driver).await?;
        let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
        Ok(S3StreamResponse::from(
            S3Response::empty(200)
                .header("etag", protocol::etag_header(&object_etag(&stats)))
                .header("content-length", "0"),
        ))
    }

    async fn complete_multipart(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
        request: UploadBody<'_>,
    ) -> S3Result<S3Response> {
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        if !aws_chunked_body(&request.head.headers) {
            validate_declared_length(&request.head.headers, request.body.len())?;
        }
        let body = decode_request_body(
            &request.head.headers,
            request.body,
            self.options.max_body_bytes,
            request.verified,
            self.options.credentials.as_ref(),
        )?;
        let requested = parse_complete_document(&body, self.options.max_xml_bytes)?;
        // Validate before creating the destination's parent. The TypeScript
        // gateway creates that parent lazily only once assembly is ready, so
        // malformed or invalid completes do not leave empty object dirs.
        let preflight_parts = multipart_parts(&driver, upload_id, &requested).await?;
        let preflight_size = multipart_parts_size(&preflight_parts);
        ensure_staging_capacity(
            &driver,
            self.options.multipart_staging_max_bytes,
            None,
            preflight_size,
        )
        .await?;
        require_atomic_rename(&driver)?;
        ensure_parent(&driver, &target.path).await?;
        // Reserve the completion inode before claiming the persisted terminal
        // marker. S3 ETags include the object inode for parity with the
        // TypeScript gateway; creating the marker first would shift the
        // completed object's identity without changing its bytes or mtime.
        let staging_path = format!(
            "{}/complete-{}",
            upload_directory(upload_id),
            new_upload_id()
        );
        let mut staging_cleanup = StagedPathCleanup::new(Arc::clone(&driver), staging_path.clone());
        let reservation =
            driver
                .open(&staging_path, "wx", 0o666)
                .await
                .map_err(|error| match error.code {
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir => {
                        S3Failure::s3("NoSuchUpload")
                    }
                    _ => S3Failure::Fs(error),
                })?;
        if let Err(error) = reservation.close().await {
            staging_cleanup.cleanup_now().await;
            return Err(S3Failure::Fs(error));
        }
        let marker = match claim_multipart_finalization(&driver, upload_id, &target.key).await {
            Ok(marker) => marker,
            Err(error) => {
                staging_cleanup.cleanup_now().await;
                return Err(error);
            }
        };
        let mut marker_cleanup = StagedPathCleanup::new(Arc::clone(&driver), marker.clone());
        let result: S3Result<S3Response> = async {
            let manifest = read_manifest(&driver, upload_id, &target.key).await?;
            let parts = multipart_parts(&driver, upload_id, &requested).await?;
            let assembled_size = multipart_parts_size(&parts);
            ensure_staging_capacity(
                &driver,
                self.options.multipart_staging_max_bytes,
                None,
                assembled_size,
            )
            .await?;
            require_atomic_rename(&driver)?;
            // Assemble through a private staging file instead of collecting all
            // parts into one Vec. This bounds memory by read_chunk_bytes and keeps
            // the existing destination unchanged if a part read or metadata update
            // fails before the final rename.
            let assemble_result: S3Result<()> = async {
                let destination = driver
                    .open(&staging_path, "r+", 0o666)
                    .await
                    .map_err(S3Failure::Fs)?;
                let mut position = 0_u64;
                let copy_result: S3Result<()> = async {
                    for (path, size) in &parts {
                        copy_file_between_drivers(
                            &driver,
                            path,
                            &destination,
                            *size,
                            self.options.read_chunk_bytes,
                            &mut position,
                        )
                        .await?;
                    }
                    Ok(())
                }
                .await;
                let close_result = destination.close().await.map_err(S3Failure::Fs);
                copy_result.and(close_result)?;
                if let Some(mtime) = manifest.mtime_ms {
                    apply_mtime(&driver, &staging_path, mtime).await?;
                }
                driver
                    .rename(&staging_path, &target.path)
                    .await
                    .map_err(S3Failure::Fs)
            }
            .await;
            if let Err(error) = assemble_result {
                staging_cleanup.cleanup_now().await;
                return Err(error);
            }
            staging_cleanup.disarm();
            let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
            remove_tree(&driver, &upload_directory(upload_id)).await?;
            durability_barrier(&driver).await?;
            let location = object_location(request.head, &target.bucket, &target.key);
            Ok(xml_response(
                200,
                protocol::complete_multipart_xml(
                    &location,
                    &target.bucket,
                    &target.key,
                    &object_etag(&stats),
                ),
                "",
            ))
        }
        .await;
        if result.is_err() {
            marker_cleanup.cleanup_now().await;
            staging_cleanup.cleanup_now().await;
        } else {
            marker_cleanup.disarm();
        }
        result
    }

    async fn abort_multipart(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
    ) -> S3Result<S3Response> {
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        let marker = claim_multipart_finalization(&driver, upload_id, &target.key).await?;
        let mut marker_cleanup = StagedPathCleanup::new(Arc::clone(&driver), marker);
        if let Err(error) = remove_tree(&driver, &upload_directory(upload_id)).await {
            marker_cleanup.cleanup_now().await;
            return Err(error);
        }
        marker_cleanup.disarm();
        durability_barrier(&driver).await?;
        Ok(S3Response::empty(204))
    }

    async fn list_parts(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
        max_parts: usize,
        marker: u32,
    ) -> S3Result<S3Response> {
        reap_staging(
            &driver,
            self.options.multipart_staging_ttl_ms,
            self.current_now_ms(),
        )
        .await?;
        let _ = read_manifest(&driver, upload_id, &target.key).await?;
        let directory = upload_directory(upload_id);
        let mut parts = Vec::new();
        for entry in driver.readdir(&directory).await.map_err(S3Failure::Fs)? {
            let Some(number) = entry
                .name
                .strip_prefix("part-")
                .and_then(|number| number.parse::<u32>().ok())
            else {
                continue;
            };
            if number <= marker || number > 10_000 || !entry.is_file() {
                continue;
            }
            let path = format!("{directory}/{}", entry.name);
            if let Ok(stats) = driver.stat(&path).await {
                parts.push(ListedPart {
                    number,
                    modified: format_iso_date(stats.mtime_ms),
                    etag: object_etag(&stats),
                    size: stats.size,
                });
            }
        }
        parts.sort_by_key(|part| part.number);
        let truncated = parts.len() > max_parts;
        let page = parts.into_iter().take(max_parts).collect::<Vec<_>>();
        let next = truncated
            .then(|| page.last().map(|part| part.number))
            .flatten();
        Ok(xml_response(
            200,
            list_parts_xml(ListPartsXml {
                bucket: &target.bucket,
                key: &target.key,
                upload_id,
                marker,
                max_parts,
                truncated,
                next_marker: next,
                parts: &page,
            }),
            "",
        ))
    }

    pub async fn close(&self) -> S3Result<()> {
        for driver in self.buckets.values() {
            let root = format!("/{MULTIPART_PREFIX}");
            let entries = match driver.readdir(&root).await {
                Ok(entries) => entries,
                Err(error)
                    if matches!(
                        error.code,
                        mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(S3Failure::Fs(error)),
            };
            for entry in entries {
                let path = format!("{root}/{}", entry.name);
                if entry.is_directory() {
                    remove_tree(driver, &path).await?;
                } else {
                    let _ = driver.unlink(&path).await;
                }
            }
            let _ = driver.rmdir(&root).await;
            durability_barrier(driver).await?;
        }
        Ok(())
    }
}

async fn next_request_body(body: &mut S3RequestBody) -> Option<Result<Vec<u8>, String>> {
    poll_fn(|context| body.as_mut().poll_next(context)).await
}

async fn collect_request_body(body: &mut S3RequestBody, max_bytes: usize) -> S3Result<Vec<u8>> {
    let mut output = Vec::new();
    while let Some(chunk) = next_request_body(body).await {
        let chunk = chunk.map_err(|_| S3Failure::s3("IncompleteBody"))?;
        if output.len().saturating_add(chunk.len()) > max_bytes {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

#[derive(Debug)]
enum ListCandidate {
    Prefix(String),
    Object { key: String, stats: Stats },
}

impl ListCandidate {
    fn key(&self) -> &str {
        match self {
            Self::Prefix(value) => value,
            Self::Object { key, .. } => key,
        }
    }
}

struct ListingPageState {
    prefix: String,
    after: String,
    delimiter: Option<String>,
    limit: usize,
    candidates: Vec<ListCandidate>,
    seen_prefixes: HashSet<String>,
}

type ListingPageResult = (Vec<ListCandidate>, bool);
type ListingPageFuture<'a> = Pin<Box<dyn Future<Output = S3Result<ListingPageResult>> + Send + 'a>>;

#[derive(Debug, Serialize, Deserialize)]
struct UploadManifest {
    key: String,
    mtime_ms: Option<i64>,
}

fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::ListBuckets => "ListBuckets",
        Operation::HeadBucket { .. } => "HeadBucket",
        Operation::ListObjectsV2 { .. } => "ListObjectsV2",
        Operation::GetObject(_) => "GetObject",
        Operation::HeadObject(_) => "HeadObject",
        Operation::PutObject(_) => "PutObject",
        Operation::DeleteObject(_) => "DeleteObject",
        Operation::DeleteObjects { .. } => "DeleteObjects",
        Operation::CopyObject { .. } => "CopyObject",
        Operation::CreateMultipartUpload(_) => "CreateMultipartUpload",
        Operation::UploadPart { .. } => "UploadPart",
        Operation::CompleteMultipartUpload { .. } => "CompleteMultipartUpload",
        Operation::AbortMultipartUpload { .. } => "AbortMultipartUpload",
        Operation::ListParts { .. } => "ListParts",
    }
}

fn operation_bucket(operation: &Operation) -> Option<&str> {
    match operation {
        Operation::ListBuckets => None,
        Operation::HeadBucket { bucket }
        | Operation::ListObjectsV2 { bucket, .. }
        | Operation::DeleteObjects { bucket } => Some(bucket),
        Operation::GetObject(target)
        | Operation::HeadObject(target)
        | Operation::PutObject(target)
        | Operation::DeleteObject(target)
        | Operation::CreateMultipartUpload(target)
        | Operation::UploadPart { target, .. }
        | Operation::CompleteMultipartUpload { target, .. }
        | Operation::AbortMultipartUpload { target, .. }
        | Operation::ListParts { target, .. } => Some(&target.bucket),
        Operation::CopyObject { destination, .. } => Some(&destination.bucket),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn format_iso_date(timestamp_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn object_etag(stats: &Stats) -> String {
    let input = format!(
        "{}:{}:{}:{}",
        stats.dev, stats.ino, stats.size, stats.mtime_ms
    );
    let digest = Sha256::digest(input.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}-1", &hex[..32])
}

fn object_location(head: &S3RequestHead, bucket: &str, key: &str) -> String {
    let encoded_bucket = sigv4::uri_encode(bucket);
    let encoded_key = key
        .split('/')
        .map(sigv4::uri_encode)
        .collect::<Vec<_>>()
        .join("/");
    let path = format!("/{encoded_bucket}/{encoded_key}");
    match header_value(&head.headers, "host") {
        Some(host) if !host.is_empty() => format!("http://{host}{path}"),
        _ => path,
    }
}

fn object_conditional_headers(etag: &str, mtime_ms: i64) -> Vec<(String, String)> {
    vec![
        ("etag".to_owned(), protocol::etag_header(etag)),
        (
            "last-modified".to_owned(),
            protocol::format_http_date(mtime_ms),
        ),
        (
            "x-amz-meta-mtime".to_owned(),
            protocol::format_meta_mtime(mtime_ms),
        ),
    ]
}

fn upload_directory(upload_id: &str) -> String {
    format!("/{MULTIPART_PREFIX}/{upload_id}")
}

fn part_path(upload_id: &str, part_number: u32) -> String {
    format!("{}/part-{part_number}", upload_directory(upload_id))
}

fn part_staging_path(upload_id: &str) -> String {
    format!("{}/.part-{}", upload_directory(upload_id), new_upload_id())
}

fn new_upload_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_upload_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn enforce_staging_quota(
    quota: Option<StagingQuota>,
    position: u64,
    additional: u64,
) -> S3Result<()> {
    if quota.is_some_and(|quota| !quota.allows(position, additional)) {
        return Err(S3Failure::s3("SlowDown"));
    }
    Ok(())
}

async fn staging_quota(
    driver: &Arc<dyn FsDriver>,
    limit: u64,
    replacement_path: Option<&str>,
) -> S3Result<StagingQuota> {
    let used = staging_usage_bytes(driver).await?;
    let replaced = match replacement_path {
        Some(path) => match driver.stat(path).await {
            Ok(stats) if stats.is_file() => stats.size,
            Ok(_) => 0,
            Err(error)
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                0
            }
            Err(error) => return Err(S3Failure::Fs(error)),
        },
        None => 0,
    };
    let quota = StagingQuota {
        limit,
        used,
        replaced,
    };
    enforce_staging_quota(Some(quota), 0, 0)?;
    Ok(quota)
}

async fn ensure_staging_capacity(
    driver: &Arc<dyn FsDriver>,
    limit: u64,
    replacement_path: Option<&str>,
    incoming: u64,
) -> S3Result<()> {
    let quota = staging_quota(driver, limit, replacement_path).await?;
    enforce_staging_quota(Some(quota), 0, incoming)
}

async fn staging_usage_bytes(driver: &Arc<dyn FsDriver>) -> S3Result<u64> {
    let mut total = sum_staging_tree(driver, &format!("/{MULTIPART_PREFIX}")).await?;
    let entries = match driver.readdir("/").await {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.code,
                mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
            ) =>
        {
            return Ok(total);
        }
        Err(error) => return Err(S3Failure::Fs(error)),
    };
    for entry in entries {
        if !entry.is_file() || !entry.name.starts_with(STREAMING_STAGING_PREFIX) {
            continue;
        }
        let path = format!("/{}", entry.name);
        let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
        total = total
            .checked_add(stats.size)
            .ok_or_else(|| S3Failure::s3("SlowDown"))?;
    }
    Ok(total)
}

async fn reap_staging(driver: &Arc<dyn FsDriver>, ttl_ms: i64, now: i64) -> S3Result<()> {
    // Drivers without timestamp support cannot distinguish an active upload
    // from an expired one. Keep the quota and explicit DeleteObjects cleanup
    // guarantees, but never reap by guessing from an unavailable mtime.
    if !driver.capabilities().times {
        return Ok(());
    }
    let expired = |mtime_ms: i64| mtime_ms > 0 && now.saturating_sub(mtime_ms) >= ttl_ms;
    let multipart_root = format!("/{MULTIPART_PREFIX}");
    let entries = match driver.readdir(&multipart_root).await {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.code,
                mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
            ) =>
        {
            Vec::new()
        }
        Err(error) => return Err(S3Failure::Fs(error)),
    };
    for entry in entries {
        if !entry.is_directory() {
            continue;
        }
        let path = format!("{multipart_root}/{}", entry.name);
        let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
        if expired(stats.mtime_ms) {
            remove_tree(driver, &path).await?;
        }
    }

    let entries = match driver.readdir("/").await {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.code,
                mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(S3Failure::Fs(error)),
    };
    for entry in entries {
        if !entry.is_file() || !entry.name.starts_with(STREAMING_STAGING_PREFIX) {
            continue;
        }
        let path = format!("/{}", entry.name);
        let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
        if expired(stats.mtime_ms) {
            match driver.unlink(&path).await {
                Ok(()) => {}
                Err(error) if error.code == mount_rs_core::ErrorCode::Enoent => {}
                Err(error) => return Err(S3Failure::Fs(error)),
            }
        }
    }
    Ok(())
}

async fn delete_staging_key(driver: &Arc<dyn FsDriver>, key: &str) -> S3Result<()> {
    let multipart_prefix = format!("{MULTIPART_PREFIX}/");
    if let Some(suffix) = key.strip_prefix(&multipart_prefix) {
        let mut components = suffix.split('/');
        let upload_id = components.next().unwrap_or_default();
        if !is_upload_id(upload_id) {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        match (components.next(), components.next()) {
            (None, None) => {}
            (Some(MANIFEST_NAME), None) => {}
            (Some(part), None)
                if part
                    .strip_prefix("part-")
                    .and_then(|number| number.parse::<u32>().ok())
                    .is_some_and(|number| (1..=10_000).contains(&number)) => {}
            _ => return Err(S3Failure::s3("InvalidRequest")),
        }
        return remove_tree(driver, &upload_directory(upload_id)).await;
    }
    if key.starts_with(STREAMING_STAGING_PREFIX)
        && key.len() > STREAMING_STAGING_PREFIX.len()
        && !key.contains('/')
    {
        let path = format!("/{key}");
        match driver.unlink(&path).await {
            Ok(()) => return Ok(()),
            Err(error) if error.code == mount_rs_core::ErrorCode::Enoent => return Ok(()),
            Err(error) => return Err(S3Failure::Fs(error)),
        }
    }
    Err(S3Failure::s3("InvalidRequest"))
}

fn sum_staging_tree<'a>(
    driver: &'a Arc<dyn FsDriver>,
    path: &'a str,
) -> Pin<Box<dyn Future<Output = S3Result<u64>> + Send + 'a>> {
    Box::pin(async move {
        let entries = match driver.readdir(path).await {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                return Ok(0);
            }
            Err(error) => return Err(S3Failure::Fs(error)),
        };
        let mut total = 0_u64;
        for entry in entries {
            if !entry.is_file() && !entry.is_directory() {
                continue;
            }
            let child = format!(
                "{}/{}",
                normalize_path(path).trim_end_matches('/'),
                entry.name
            );
            let size = if entry.is_directory() {
                sum_staging_tree(driver, &child).await?
            } else {
                driver.stat(&child).await.map_err(S3Failure::Fs)?.size
            };
            total = total
                .checked_add(size)
                .ok_or_else(|| S3Failure::s3("SlowDown"))?;
        }
        Ok(total)
    })
}

async fn ensure_parent(driver: &Arc<dyn FsDriver>, path: &str) -> S3Result<()> {
    let parent = dirname(path);
    if parent == "/" {
        // The root already exists. Avoid requiring an otherwise optional
        // mkdir implementation just to publish an object at the bucket root.
        return Ok(());
    }
    driver
        .mkdir(
            &parent,
            MkdirOptions {
                recursive: true,
                mode: Some(0o777),
            },
        )
        .await
        .map(|_| ())
        .map_err(S3Failure::Fs)
}

async fn write_bytes(
    driver: &Arc<dyn FsDriver>,
    path: &str,
    bytes: &[u8],
    exclusive: bool,
) -> S3Result<()> {
    let flags = if exclusive { "wx" } else { "w" };
    let handle = driver
        .open(path, flags, 0o666)
        .await
        .map_err(S3Failure::Fs)?;
    let mut offset = 0_u64;
    while offset < bytes.len() as u64 {
        let written = handle
            .write(&bytes[offset as usize..], Some(offset))
            .await
            .map_err(S3Failure::Fs)?;
        if written == 0 {
            let _ = handle.close().await;
            return Err(S3Failure::s3("InternalError"));
        }
        offset += written as u64;
    }
    handle.close().await.map_err(S3Failure::Fs)?;
    Ok(())
}

async fn open_stream_handle(
    driver: &Arc<dyn FsDriver>,
    path: &str,
    exclusive: bool,
    create_parent: bool,
) -> S3Result<Arc<dyn mount_rs_core::FileHandle>> {
    let flags = if exclusive { "wx" } else { "w" };
    match driver.open(path, flags, 0o666).await {
        Ok(handle) => Ok(handle),
        Err(error)
            if create_parent
                && matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
        {
            ensure_parent(driver, path).await?;
            driver.open(path, flags, 0o666).await.map_err(S3Failure::Fs)
        }
        Err(error) => Err(S3Failure::Fs(error)),
    }
}

async fn write_stream_chunk(
    handle: &Arc<dyn mount_rs_core::FileHandle>,
    position: &mut u64,
    bytes: &[u8],
) -> S3Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let written = handle
            .write(&bytes[offset..], Some(*position))
            .await
            .map_err(S3Failure::Fs)?;
        if written == 0 {
            return Err(S3Failure::s3("InternalError"));
        }
        if written > bytes.len() - offset {
            return Err(S3Failure::s3("InternalError"));
        }
        offset += written;
        *position += written as u64;
    }
    Ok(())
}

async fn copy_file_between_drivers(
    source_driver: &Arc<dyn FsDriver>,
    source_path: &str,
    destination: &Arc<dyn mount_rs_core::FileHandle>,
    size: u64,
    chunk_bytes: usize,
    destination_position: &mut u64,
) -> S3Result<()> {
    let source = source_driver
        .open(source_path, "r", 0)
        .await
        .map_err(S3Failure::Fs)?;
    let mut buffer = vec![0_u8; chunk_bytes.max(1)];
    let mut source_position = 0_u64;
    let copy_result: S3Result<()> = async {
        while source_position < size {
            let requested = (size - source_position).min(buffer.len() as u64) as usize;
            let count = source
                .read(&mut buffer[..requested], Some(source_position))
                .await
                .map_err(S3Failure::Fs)?;
            if count == 0 {
                return Err(S3Failure::s3("InternalError"));
            }
            if count > requested {
                return Err(S3Failure::s3("InternalError"));
            }
            write_stream_chunk(destination, destination_position, &buffer[..count]).await?;
            source_position += count as u64;
        }
        Ok(())
    }
    .await;
    let close_result = source.close().await.map_err(S3Failure::Fs);
    copy_result.and(close_result)
}

fn require_atomic_rename(driver: &Arc<dyn FsDriver>) -> S3Result<()> {
    if driver.capabilities().atomic_rename {
        Ok(())
    } else {
        Err(S3Failure::s3("NotImplemented"))
    }
}

/// Await the driver's persistence barrier before acknowledging a mutation.
/// Volatile drivers intentionally remain unchanged; drivers that advertise
/// durable writes must implement `syncfs` or return an explicit error rather
/// than allowing S3 to report a false durable success.
async fn durability_barrier(driver: &Arc<dyn FsDriver>) -> S3Result<()> {
    if driver.capabilities().durable_writes {
        driver.syncfs().await.map_err(S3Failure::Fs)?;
    }
    Ok(())
}

struct StreamingPayloadHash {
    expected: String,
    digest: Sha256,
}

impl StreamingPayloadHash {
    fn new(headers: &[HeaderEntry]) -> S3Result<Option<Self>> {
        let Some(value) = header_value(headers, "x-amz-content-sha256") else {
            return Ok(None);
        };
        if matches!(
            value.as_str(),
            sigv4::UNSIGNED_PAYLOAD
                | sigv4::STREAMING_PAYLOAD
                | sigv4::STREAMING_PAYLOAD_TRAILER
                | sigv4::STREAMING_UNSIGNED_PAYLOAD_TRAILER
        ) {
            return Ok(None);
        }
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(S3Failure::s3("SignatureDoesNotMatch"));
        }
        Ok(Some(Self {
            expected: value.to_ascii_lowercase(),
            digest: Sha256::new(),
        }))
    }

    fn update(&mut self, bytes: &[u8]) {
        self.digest.update(bytes);
    }

    fn verify(self) -> S3Result<()> {
        let actual = sigv4::sha256_hex_digest(self.digest.finalize());
        if actual
            .as_bytes()
            .ct_eq(self.expected.as_bytes())
            .unwrap_u8()
            != 1
        {
            return Err(S3Failure::s3("SignatureDoesNotMatch"));
        }
        Ok(())
    }
}

async fn write_stream_body(request: StreamWriteRequest<'_>) -> S3Result<u64> {
    let StreamWriteRequest {
        driver,
        path,
        headers,
        mut body,
        verified,
        credentials,
        max_body_bytes,
        exclusive,
        create_parent,
        cleanup_on_error,
        staging_quota,
    } = request;
    let chunked = aws_chunked_body(headers);
    let declared_length = if chunked {
        None
    } else {
        let raw = header_value(headers, "content-length")
            .ok_or_else(|| S3Failure::s3("MissingContentLength"))?;
        let declared =
            parse_declared_length(&raw).ok_or_else(|| S3Failure::s3("InvalidArgument"))?;
        if declared > max_body_bytes as u64 {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        Some(declared)
    };
    let mut decoder = StreamingBodyDecoder::new(headers, max_body_bytes, verified, credentials)?;
    let mut payload_hash = StreamingPayloadHash::new(headers)?;
    let mut handle: Option<Arc<dyn mount_rs_core::FileHandle>> = None;
    let mut position = 0_u64;
    let operation: S3Result<()> = async {
        while let Some(chunk) = next_request_body(&mut body).await {
            let chunk = chunk.map_err(|_| S3Failure::s3("IncompleteBody"))?;
            let payloads = if let Some(decoder) = decoder.as_mut() {
                decoder.feed(&chunk)?
            } else {
                if declared_length
                    .is_some_and(|length| position.saturating_add(chunk.len() as u64) > length)
                {
                    return Err(S3Failure::s3("IncompleteBody"));
                }
                vec![chunk]
            };
            for payload in payloads {
                if let Some(payload_hash) = payload_hash.as_mut() {
                    payload_hash.update(&payload);
                }
                if payload.is_empty() {
                    continue;
                }
                enforce_staging_quota(staging_quota, position, payload.len() as u64)?;
                if handle.is_none() {
                    handle =
                        Some(open_stream_handle(driver, path, exclusive, create_parent).await?);
                }
                write_stream_chunk(
                    handle.as_ref().expect("stream handle opened above"),
                    &mut position,
                    &payload,
                )
                .await?;
            }
        }
        if let Some(decoder) = decoder.as_mut() {
            for payload in decoder.finish()? {
                if let Some(payload_hash) = payload_hash.as_mut() {
                    payload_hash.update(&payload);
                }
                if payload.is_empty() {
                    continue;
                }
                enforce_staging_quota(staging_quota, position, payload.len() as u64)?;
                if handle.is_none() {
                    handle =
                        Some(open_stream_handle(driver, path, exclusive, create_parent).await?);
                }
                write_stream_chunk(
                    handle.as_ref().expect("stream handle opened above"),
                    &mut position,
                    &payload,
                )
                .await?;
            }
        } else if declared_length != Some(position) {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        if let Some(payload_hash) = payload_hash {
            payload_hash.verify()?;
        }
        if handle.is_none() {
            handle = Some(open_stream_handle(driver, path, exclusive, create_parent).await?);
        }
        Ok(())
    }
    .await;
    let opened = handle.is_some();
    let close_result = if let Some(handle) = handle {
        handle.close().await.map_err(S3Failure::Fs)
    } else {
        Ok(())
    };
    let result = operation.and(close_result).map(|()| position);
    if result.is_err() && cleanup_on_error && opened {
        let _ = driver.unlink(path).await;
    }
    result
}

async fn consume_stream_body(
    mut body: S3RequestBody,
    headers: &[HeaderEntry],
    max_body_bytes: usize,
    verified: Option<&sigv4::VerifiedRequest>,
    credentials: Option<&Credentials>,
) -> S3Result<()> {
    let chunked = aws_chunked_body(headers);
    let declared_length = if chunked {
        None
    } else {
        let raw = header_value(headers, "content-length")
            .ok_or_else(|| S3Failure::s3("MissingContentLength"))?;
        Some(parse_declared_length(&raw).ok_or_else(|| S3Failure::s3("InvalidArgument"))?)
    };
    let mut decoder = StreamingBodyDecoder::new(headers, max_body_bytes, verified, credentials)?;
    let mut payload_hash = StreamingPayloadHash::new(headers)?;
    let mut received = 0_u64;
    while let Some(chunk) = next_request_body(&mut body).await {
        let chunk = chunk.map_err(|_| S3Failure::s3("IncompleteBody"))?;
        let payloads = if let Some(decoder) = decoder.as_mut() {
            decoder.feed(&chunk)?
        } else {
            received = received.saturating_add(chunk.len() as u64);
            if received > max_body_bytes as u64 {
                return Err(S3Failure::s3("EntityTooLarge"));
            }
            vec![chunk]
        };
        for payload in &payloads {
            if let Some(payload_hash) = payload_hash.as_mut() {
                payload_hash.update(payload);
            }
        }
        if payloads.iter().any(|payload| !payload.is_empty()) {
            return Err(S3Failure::s3("InvalidRequest"));
        }
    }
    if let Some(decoder) = decoder.as_mut() {
        let payloads = decoder.finish()?;
        for payload in &payloads {
            if let Some(payload_hash) = payload_hash.as_mut() {
                payload_hash.update(payload);
            }
        }
        if payloads.iter().any(|payload| !payload.is_empty()) {
            return Err(S3Failure::s3("InvalidRequest"));
        }
    } else if declared_length != Some(received) {
        return Err(S3Failure::s3("IncompleteBody"));
    }
    if let Some(payload_hash) = payload_hash {
        payload_hash.verify()?;
    }
    Ok(())
}

async fn read_bytes_range(
    driver: Arc<dyn FsDriver>,
    path: &str,
    range: Option<ByteRange>,
    size: u64,
    chunk_bytes: usize,
) -> S3Result<Vec<u8>> {
    let handle = driver.open(path, "r", 0).await.map_err(S3Failure::Fs)?;
    let start = range.map_or(0, |range| range.start);
    let end = range.map_or(size.saturating_sub(1), |range| range.end);
    let expected = if size == 0 || end < start {
        0
    } else {
        end - start + 1
    };
    let mut output = Vec::with_capacity(expected.min(usize::MAX as u64) as usize);
    let mut position = start;
    while position <= end && expected > 0 {
        let remaining = (end - position + 1).min(chunk_bytes.max(1) as u64);
        let mut buffer = vec![0_u8; remaining as usize];
        let count = handle
            .read(&mut buffer, Some(position))
            .await
            .map_err(S3Failure::Fs)?;
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
        position += count as u64;
    }
    handle.close().await.map_err(S3Failure::Fs)?;
    Ok(output)
}

struct FileBodyStream {
    receiver: mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl Stream for FileBodyStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(context)
    }
}

impl Drop for FileBodyStream {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

fn stream_file(
    driver: Arc<dyn FsDriver>,
    path: &str,
    start: u64,
    length: u64,
    chunk_bytes: usize,
) -> S3ResponseBodyStream {
    let (sender, receiver) = mpsc::channel(1);
    let (cancel, mut cancelled) = oneshot::channel();
    let path = path.to_owned();
    tokio::spawn(async move {
        let handle = match driver.open(&path, "r", 0).await {
            Ok(handle) => handle,
            Err(error) => {
                let _ = sender
                    .send(Err(std::io::Error::other(error.to_string())))
                    .await;
                return;
            }
        };
        let mut position = start;
        let mut remaining = length;
        while remaining > 0 {
            let read_length = remaining.min(chunk_bytes.max(1) as u64) as usize;
            let mut buffer = vec![0_u8; read_length];
            let result = tokio::select! {
                _ = &mut cancelled => None,
                result = handle.read(&mut buffer, Some(position)) => Some(result),
            };
            let Some(result) = result else {
                break;
            };
            let count = match result {
                Ok(count) => count,
                Err(error) => {
                    let _ = sender
                        .send(Err(std::io::Error::other(error.to_string())))
                        .await;
                    break;
                }
            };
            if count == 0 {
                break;
            }
            if count > buffer.len() {
                let _ = sender
                    .send(Err(std::io::Error::other(
                        "file driver returned more bytes than requested",
                    )))
                    .await;
                break;
            }
            buffer.truncate(count);
            position += count as u64;
            remaining -= count as u64;
            if sender.send(Ok(buffer)).await.is_err() {
                break;
            }
        }
        let _ = handle.close().await;
    });
    Box::pin(FileBodyStream {
        receiver,
        cancel: Some(cancel),
    })
}

async fn apply_mtime(driver: &Arc<dyn FsDriver>, path: &str, mtime_ms: i64) -> S3Result<()> {
    if driver.capabilities().times {
        driver
            .utimes(path, mtime_ms, mtime_ms)
            .await
            .map_err(S3Failure::Fs)?;
    }
    Ok(())
}

fn remove_tree<'a>(
    driver: &'a Arc<dyn FsDriver>,
    path: &'a str,
) -> Pin<Box<dyn Future<Output = S3Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let entries = match driver.readdir(path).await {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(S3Failure::Fs(error)),
        };
        for entry in entries {
            let child = format!(
                "{}/{}",
                normalize_path(path).trim_end_matches('/'),
                entry.name
            );
            if entry.is_directory() {
                remove_tree(driver, &child).await?;
            } else {
                match driver.unlink(&child).await {
                    Ok(()) => {}
                    Err(error) if matches!(error.code, mount_rs_core::ErrorCode::Enoent) => {}
                    Err(error) => return Err(S3Failure::Fs(error)),
                }
            }
        }
        match driver.rmdir(path).await {
            Ok(()) => Ok(()),
            Err(error) if matches!(error.code, mount_rs_core::ErrorCode::Enoent) => Ok(()),
            Err(error) => Err(S3Failure::Fs(error)),
        }
    })
}

fn collect_listing_page<'a>(
    driver: Arc<dyn FsDriver>,
    prefix: &'a str,
    after: &'a str,
    delimiter: Option<&'a str>,
    max_keys: usize,
) -> ListingPageFuture<'a> {
    Box::pin(async move {
        if max_keys == 0 {
            return Ok((Vec::new(), false));
        }
        let mut state = ListingPageState {
            prefix: prefix.to_owned(),
            after: after.to_owned(),
            delimiter: delimiter.map(str::to_owned),
            limit: max_keys.saturating_add(1),
            candidates: Vec::new(),
            seen_prefixes: HashSet::new(),
        };
        walk_listing_directory(driver, "/".to_owned(), String::new(), &mut state).await?;
        let truncated = state.candidates.len() > max_keys;
        state.candidates.truncate(max_keys);
        Ok((state.candidates, truncated))
    })
}

fn walk_listing_directory<'a>(
    driver: Arc<dyn FsDriver>,
    path: String,
    key: String,
    state: &'a mut ListingPageState,
) -> Pin<Box<dyn Future<Output = S3Result<()>> + Send + 'a>> {
    Box::pin(async move {
        if state.candidates.len() >= state.limit {
            return Ok(());
        }
        let mut entries = driver.readdir(&path).await.map_err(S3Failure::Fs)?;
        entries.retain(|entry| {
            entry.file_type == FileType::File || entry.file_type == FileType::Directory
        });
        if path == "/" {
            entries.retain(|entry| {
                entry.name != MULTIPART_PREFIX && !entry.name.starts_with(STREAMING_STAGING_PREFIX)
            });
        }
        entries.sort_by(|left, right| {
            let left_key = listing_entry_key(&key, left);
            let right_key = listing_entry_key(&key, right);
            compare_utf8(&left_key, &right_key)
        });
        if entries.is_empty() {
            if !key.is_empty() {
                append_listing_entry(&driver, state, key, path).await?;
            }
            return Ok(());
        }
        for entry in entries {
            if state.candidates.len() >= state.limit {
                break;
            }
            let child_path = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{path}/{}", entry.name)
            };
            let child_key = listing_entry_key(&key, &entry);
            if entry.is_directory() {
                if !listing_subtree_matches(&child_key, &state.prefix)
                    || !listing_subtree_follows_after(&child_key, &state.after)
                {
                    continue;
                }
                if let Some(delimiter) = state.delimiter.as_deref()
                    && child_key != state.prefix
                    && child_key.starts_with(&state.prefix)
                {
                    let rest = &child_key[state.prefix.len()..];
                    if let Some(index) = rest.find(delimiter) {
                        let common =
                            format!("{}{}", state.prefix, &rest[..index + delimiter.len()]);
                        if compare_utf8(&common, &state.after) == std::cmp::Ordering::Greater
                            && state.seen_prefixes.insert(common.clone())
                        {
                            state.candidates.push(ListCandidate::Prefix(common));
                        }
                        continue;
                    }
                }
                walk_listing_directory(driver.clone(), child_path, child_key, state).await?;
            } else {
                append_listing_entry(&driver, state, child_key, child_path).await?;
            }
        }
        Ok(())
    })
}

fn listing_entry_key(key: &str, entry: &mount_rs_core::DirEntry) -> String {
    if key.is_empty() {
        if entry.is_directory() {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        }
    } else if entry.is_directory() {
        format!("{key}{}/", entry.name)
    } else {
        format!("{key}{}", entry.name)
    }
}

fn listing_subtree_matches(child_key: &str, prefix: &str) -> bool {
    prefix.is_empty() || child_key.starts_with(prefix) || prefix.starts_with(child_key)
}

fn listing_subtree_follows_after(child_key: &str, after: &str) -> bool {
    after.is_empty()
        || after.starts_with(child_key)
        || compare_utf8(child_key, after) == std::cmp::Ordering::Greater
}

async fn append_listing_entry(
    driver: &Arc<dyn FsDriver>,
    state: &mut ListingPageState,
    key: String,
    path: String,
) -> S3Result<()> {
    if !key.starts_with(&state.prefix)
        || compare_utf8(&key, &state.after) != std::cmp::Ordering::Greater
    {
        return Ok(());
    }
    if let Some(delimiter) = state.delimiter.as_deref() {
        let rest = &key[state.prefix.len()..];
        if let Some(index) = rest.find(delimiter) {
            let common = format!("{}{}", state.prefix, &rest[..index + delimiter.len()]);
            if state.seen_prefixes.insert(common.clone()) {
                state.candidates.push(ListCandidate::Prefix(common));
            }
            return Ok(());
        }
    }
    let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
    state.candidates.push(ListCandidate::Object { key, stats });
    Ok(())
}

fn listable_prefix(prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if prefix.starts_with('/')
        || prefix.contains("//")
        || prefix.contains('\0')
        || prefix.starts_with(&format!("{MULTIPART_PREFIX}/"))
        || prefix.starts_with(STREAMING_STAGING_PREFIX)
    {
        return false;
    }
    let components = prefix.split('/').collect::<Vec<_>>();
    !components[..components.len().saturating_sub(1)]
        .iter()
        .any(|part| *part == "." || *part == "..")
}

fn evaluate_get_conditionals(
    stats: &Stats,
    etag: &str,
    headers: &[HeaderEntry],
    method: &str,
) -> Option<u16> {
    if let Some(value) = header_list(headers, "if-match")
        && !conditional_match(&value, etag)
    {
        return Some(412);
    }
    if let Some(value) = header_value(headers, "if-unmodified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms > value + 999
    {
        return Some(412);
    }
    if let Some(value) = header_list(headers, "if-none-match")
        && conditional_match(&value, etag)
    {
        return Some(
            if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD") {
                304
            } else {
                412
            },
        );
    } else if let Some(value) = header_value(headers, "if-modified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms <= value + 999
    {
        return Some(304);
    }
    None
}

fn if_range_matches(value: &str, etag: &str, mtime_ms: i64) -> bool {
    if value.trim().starts_with('"') {
        return value.trim() == protocol::etag_header(etag);
    }
    protocol::parse_http_date(value).is_some_and(|date| mtime_ms <= date + 999)
}

fn check_put_conditionals(existing: Option<&Stats>, headers: &[HeaderEntry]) -> S3Result<()> {
    if let Some(value) = header_list(headers, "if-match") {
        let Some(existing) = existing else {
            return Err(S3Failure::s3("NoSuchKey"));
        };
        if !conditional_match(&value, &object_etag(existing)) {
            return Err(S3Failure::s3("PreconditionFailed"));
        }
    }
    if let Some(value) = header_list(headers, "if-none-match")
        && let Some(existing) = existing
        && conditional_match(&value, &object_etag(existing))
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_value(headers, "if-unmodified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && existing.is_some_and(|existing| existing.mtime_ms > value + 999)
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    Ok(())
}

fn has_put_conditionals(headers: &[HeaderEntry]) -> bool {
    header_value(headers, "if-match").is_some()
        || header_value(headers, "if-none-match").is_some()
        || header_value(headers, "if-unmodified-since").is_some()
}

fn check_create_only(headers: &[HeaderEntry]) -> bool {
    header_list(headers, "if-none-match").is_some_and(|value| value.trim() == "*")
}

fn check_copy_conditionals(stats: &Stats, etag: &str, headers: &[HeaderEntry]) -> S3Result<()> {
    if let Some(value) = header_list(headers, "x-amz-copy-source-if-match")
        && !conditional_match(&value, etag)
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_list(headers, "x-amz-copy-source-if-none-match")
        && conditional_match(&value, etag)
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_value(headers, "x-amz-copy-source-if-unmodified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms > value + 999
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_value(headers, "x-amz-copy-source-if-modified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms <= value + 999
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    Ok(())
}

const MAX_SAFE_LENGTH: u64 = 9_007_199_254_740_991;

fn aws_chunked_body(headers: &[HeaderEntry]) -> bool {
    let streaming = header_value(headers, "x-amz-content-sha256").unwrap_or_default();
    content_encoding_chunked(headers)
        || matches!(
            streaming.as_str(),
            sigv4::STREAMING_PAYLOAD
                | sigv4::STREAMING_PAYLOAD_TRAILER
                | sigv4::STREAMING_UNSIGNED_PAYLOAD_TRAILER
        )
}

fn parse_declared_length(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = value.parse::<u64>().ok()?;
    (value <= MAX_SAFE_LENGTH).then_some(value)
}

fn validate_declared_length(headers: &[HeaderEntry], actual: usize) -> S3Result<()> {
    let Some(raw) = header_value(headers, "content-length") else {
        return Err(S3Failure::s3("MissingContentLength"));
    };
    let length = parse_declared_length(&raw).ok_or_else(|| S3Failure::s3("InvalidArgument"))?;
    if length != actual as u64 {
        return Err(S3Failure::s3("IncompleteBody"));
    }
    Ok(())
}

fn decode_request_body(
    headers: &[HeaderEntry],
    body: &[u8],
    max_body_bytes: usize,
    verified: Option<&sigv4::VerifiedRequest>,
    credentials: Option<&Credentials>,
) -> S3Result<Vec<u8>> {
    let streaming = header_value(headers, "x-amz-content-sha256").unwrap_or_default();
    let signed_streaming = matches!(
        streaming.as_str(),
        sigv4::STREAMING_PAYLOAD | sigv4::STREAMING_PAYLOAD_TRAILER
    );
    let streaming_sentinel = matches!(
        streaming.as_str(),
        sigv4::STREAMING_PAYLOAD
            | sigv4::STREAMING_PAYLOAD_TRAILER
            | sigv4::STREAMING_UNSIGNED_PAYLOAD_TRAILER
    );
    let chunked = aws_chunked_body(headers);
    let decoded_length = header_value(headers, "x-amz-decoded-content-length")
        .map(|value| parse_declared_length(&value).ok_or_else(|| S3Failure::s3("InvalidArgument")))
        .transpose()?;
    if !chunked {
        if streaming_sentinel {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        return Ok(body.to_vec());
    }
    let trailers = header_value(headers, "x-amz-trailer")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| name.to_ascii_lowercase())
                .fold(Vec::new(), |mut names, name| {
                    if !names.iter().any(|existing| existing == &name) {
                        names.push(name);
                    }
                    names
                })
        })
        .unwrap_or_default();
    let signing = if signed_streaming {
        match (credentials, verified) {
            (Some(credentials), Some(verified)) if !verified.presigned => Some(ChunkSigning {
                credentials,
                verified,
            }),
            _ => None,
        }
    } else {
        None
    };
    decode_aws_chunked(body, max_body_bytes, decoded_length, signing, &trailers)
}

struct ChunkSigning<'a> {
    credentials: &'a Credentials,
    verified: &'a sigv4::VerifiedRequest,
}

const MAX_SIGNED_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHUNK_HEADER_BYTES: usize = 256;
const MAX_TRAILER_BYTES: usize = 16 * 1024;
const TRAILER_SIGNATURE_HEADER: &str = "x-amz-trailer-signature";

fn decode_aws_chunked(
    body: &[u8],
    max_body_bytes: usize,
    decoded_length: Option<u64>,
    signing: Option<ChunkSigning<'_>>,
    declared_trailers: &[String],
) -> S3Result<Vec<u8>> {
    let mut cursor = 0;
    let mut output = Vec::new();
    let mut previous_signature = signing
        .as_ref()
        .map(|chunk| chunk.verified.signature.clone());
    loop {
        let Some(line_offset) = body
            .get(cursor..)
            .and_then(|remaining| remaining.windows(2).position(|pair| pair == b"\r\n"))
        else {
            return Err(S3Failure::s3("IncompleteBody"));
        };
        if line_offset > MAX_CHUNK_HEADER_BYTES {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        let line_end = cursor + line_offset;
        let header = std::str::from_utf8(&body[cursor..line_end])
            .map_err(|_| S3Failure::s3("InvalidRequest"))?;
        let mut extensions = header.split(';');
        let size_text = extensions.next().unwrap_or_default();
        if size_text.is_empty() || size_text.len() > 16 {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        let size =
            u64::from_str_radix(size_text, 16).map_err(|_| S3Failure::s3("InvalidRequest"))?;
        let provided_signature = extensions.find_map(|extension| {
            let (name, value) = extension.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("chunk-signature")
                .then(|| value.trim().to_owned())
        });
        if signing.is_some() {
            let Some(signature) = provided_signature.as_deref() else {
                return Err(S3Failure::s3("SignatureDoesNotMatch"));
            };
            if signature.len() != 64 || !signature.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(S3Failure::s3("InvalidRequest"));
            }
        }
        cursor = line_end + 2;
        if signing.is_some() && size > MAX_SIGNED_CHUNK_BYTES {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        if signing.is_none() && size > MAX_SAFE_LENGTH {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let size_usize = usize::try_from(size).map_err(|_| S3Failure::s3("EntityTooLarge"))?;
        let payload = if size == 0 {
            &[]
        } else {
            if output.len().saturating_add(size_usize) > max_body_bytes {
                return Err(S3Failure::s3("EntityTooLarge"));
            }
            let end = cursor
                .checked_add(size_usize)
                .ok_or_else(|| S3Failure::s3("EntityTooLarge"))?;
            if end.checked_add(2).is_none_or(|end| end > body.len()) {
                return Err(S3Failure::s3("IncompleteBody"));
            }
            let payload = &body[cursor..end];
            if decoded_length
                .is_some_and(|length| output.len().saturating_add(size_usize) as u64 > length)
            {
                return Err(S3Failure::s3("IncompleteBody"));
            }
            payload
        };
        if let Some(signing) = signing.as_ref() {
            let previous = previous_signature
                .as_deref()
                .ok_or_else(|| S3Failure::s3("SignatureDoesNotMatch"))?;
            let expected = chunk_signature(signing, previous, &sigv4::sha256_hex(payload));
            let supplied = provided_signature
                .as_deref()
                .expect("signed chunk signature checked above")
                .to_ascii_lowercase();
            if expected.as_bytes().ct_eq(supplied.as_bytes()).unwrap_u8() != 1 {
                return Err(S3Failure::S3(protocol::error_with_message(
                    "SignatureDoesNotMatch",
                    "chunk signature does not match",
                )));
            }
            previous_signature = Some(expected);
        }
        if size == 0 {
            if decoded_length.is_some_and(|length| output.len() as u64 != length) {
                return Err(S3Failure::s3("IncompleteBody"));
            }
            if declared_trailers.is_empty() {
                if cursor == body.len() || (body.len() - cursor == 2 && &body[cursor..] == b"\r\n")
                {
                    return Ok(output);
                }
                return Err(S3Failure::s3("IncompleteBody"));
            }
            return decode_trailers(
                body,
                cursor,
                output,
                declared_trailers,
                signing.as_ref(),
                previous_signature.as_deref(),
            );
        }
        let end = cursor
            .checked_add(size_usize)
            .ok_or_else(|| S3Failure::s3("EntityTooLarge"))?;
        output.extend_from_slice(payload);
        cursor = end;
        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        cursor += 2;
    }
}

fn decode_trailers(
    body: &[u8],
    mut cursor: usize,
    output: Vec<u8>,
    declared_trailers: &[String],
    signing: Option<&ChunkSigning<'_>>,
    previous_signature: Option<&str>,
) -> S3Result<Vec<u8>> {
    let mut trailer_block = Vec::new();
    let mut seen = Vec::new();
    loop {
        if cursor == body.len() {
            if signing.is_some() {
                return Err(S3Failure::s3("IncompleteBody"));
            }
            ensure_trailers_present(declared_trailers, &seen)?;
            return Ok(output);
        }
        let Some(line_offset) = body
            .get(cursor..)
            .and_then(|remaining| remaining.windows(2).position(|pair| pair == b"\r\n"))
        else {
            if body.len().saturating_sub(cursor)
                > MAX_TRAILER_BYTES.saturating_sub(trailer_block.len())
            {
                return Err(S3Failure::s3("EntityTooLarge"));
            }
            return Err(S3Failure::s3("IncompleteBody"));
        };
        let line_end = cursor + line_offset;
        let raw_line = &body[cursor..line_end];
        let remaining = MAX_TRAILER_BYTES.saturating_sub(trailer_block.len());
        if !raw_line.is_empty() && raw_line.len().saturating_add(1) > remaining {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let line = latin1(raw_line);
        cursor = line_end + 2;
        if line.is_empty() {
            if signing.is_some() {
                return Err(S3Failure::s3("InvalidRequest"));
            }
            ensure_trailers_present(declared_trailers, &seen)?;
            if cursor == body.len() || (body.len() - cursor == 2 && &body[cursor..] == b"\r\n") {
                return Ok(output);
            }
            return Err(S3Failure::s3("IncompleteBody"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| S3Failure::s3("InvalidRequest"))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == TRAILER_SIGNATURE_HEADER {
            let Some(signing) = signing else {
                return Err(S3Failure::s3("InvalidRequest"));
            };
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(S3Failure::s3("InvalidRequest"));
            }
            ensure_trailers_present(declared_trailers, &seen)?;
            let previous =
                previous_signature.ok_or_else(|| S3Failure::s3("SignatureDoesNotMatch"))?;
            let expected = sigv4::sign_trailer(
                &signing.credentials.secret_access_key,
                &signing.verified.scope,
                &sigv4::format_amz_date(signing.verified.timestamp_ms),
                previous,
                &sigv4::sha256_hex(&trailer_block),
            );
            if expected
                .as_bytes()
                .ct_eq(value.to_ascii_lowercase().as_bytes())
                .unwrap_u8()
                != 1
            {
                return Err(S3Failure::S3(protocol::error_with_message(
                    "SignatureDoesNotMatch",
                    "trailer signature does not match",
                )));
            }
            if cursor == body.len() || (body.len() - cursor == 2 && &body[cursor..] == b"\r\n") {
                return Ok(output);
            }
            return Err(S3Failure::s3("IncompleteBody"));
        }
        if name.is_empty()
            || !declared_trailers.iter().any(|declared| declared == &name)
            || seen.iter().any(|existing| existing == &name)
        {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        if trailer_block
            .len()
            .saturating_add(raw_line.len())
            .saturating_add(1)
            > MAX_TRAILER_BYTES
        {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        trailer_block.extend_from_slice(raw_line);
        trailer_block.push(b'\n');
        seen.push(name);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamingDecodeState {
    Header,
    Payload,
    PayloadCr,
    PayloadLf,
    Trailer,
    Epilogue,
    Done,
}

/// Incremental aws-chunked decoder used by the HTTP write path.
///
/// Unsigned payload chunks are released as soon as they arrive. Signed chunks
/// are held only for the duration of their individual signature check, matching
/// mountx's decoder and its eight-megabyte signed-frame bound. Header and
/// trailer buffers are independently bounded so a fragmented peer cannot turn
/// framing into an unbounded allocation.
struct StreamingBodyDecoder<'a> {
    encoded: Vec<u8>,
    state: StreamingDecodeState,
    current_size: u64,
    current_remaining: u64,
    current_signature: Option<String>,
    current_payload: Vec<u8>,
    previous_signature: Option<String>,
    decoded_length: Option<u64>,
    decoded: u64,
    max_body_bytes: usize,
    signing: Option<ChunkSigning<'a>>,
    declared_trailers: Vec<String>,
    trailer_block: Vec<u8>,
    seen_trailers: Vec<String>,
}

impl<'a> StreamingBodyDecoder<'a> {
    fn new(
        headers: &[HeaderEntry],
        max_body_bytes: usize,
        verified: Option<&'a sigv4::VerifiedRequest>,
        credentials: Option<&'a Credentials>,
    ) -> S3Result<Option<Self>> {
        if !aws_chunked_body(headers) {
            return Ok(None);
        }
        let streaming = header_value(headers, "x-amz-content-sha256").unwrap_or_default();
        let signed_streaming = matches!(
            streaming.as_str(),
            sigv4::STREAMING_PAYLOAD | sigv4::STREAMING_PAYLOAD_TRAILER
        );
        let decoded_length = header_value(headers, "x-amz-decoded-content-length")
            .map(|value| {
                parse_declared_length(&value).ok_or_else(|| S3Failure::s3("InvalidArgument"))
            })
            .transpose()?;
        let declared_trailers = header_value(headers, "x-amz-trailer")
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(|name| name.to_ascii_lowercase())
                    .fold(Vec::new(), |mut names, name| {
                        if !names.iter().any(|existing| existing == &name) {
                            names.push(name);
                        }
                        names
                    })
            })
            .unwrap_or_default();
        let signing = if signed_streaming {
            match (credentials, verified) {
                (Some(credentials), Some(verified)) if !verified.presigned => Some(ChunkSigning {
                    credentials,
                    verified,
                }),
                _ => None,
            }
        } else {
            None
        };
        let previous_signature = signing
            .as_ref()
            .map(|chunk| chunk.verified.signature.clone());
        Ok(Some(Self {
            encoded: Vec::new(),
            state: StreamingDecodeState::Header,
            current_size: 0,
            current_remaining: 0,
            current_signature: None,
            current_payload: Vec::new(),
            previous_signature,
            decoded_length,
            decoded: 0,
            max_body_bytes,
            signing,
            declared_trailers,
            trailer_block: Vec::new(),
            seen_trailers: Vec::new(),
        }))
    }

    fn feed(&mut self, bytes: &[u8]) -> S3Result<Vec<Vec<u8>>> {
        if self.state == StreamingDecodeState::Done && !bytes.is_empty() {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        self.encoded.extend_from_slice(bytes);
        let mut output = Vec::new();
        loop {
            let progressed = match self.state {
                StreamingDecodeState::Header => self.consume_header()?,
                StreamingDecodeState::Payload => self.consume_payload(&mut output)?,
                StreamingDecodeState::PayloadCr => self.consume_payload_cr()?,
                StreamingDecodeState::PayloadLf => self.consume_payload_lf(&mut output)?,
                StreamingDecodeState::Trailer => self.consume_trailer()?,
                StreamingDecodeState::Epilogue => self.consume_epilogue()?,
                StreamingDecodeState::Done => {
                    if self.encoded.is_empty() {
                        false
                    } else {
                        return Err(S3Failure::s3("IncompleteBody"));
                    }
                }
            };
            if !progressed {
                break;
            }
        }
        Ok(output)
    }

    fn finish(&mut self) -> S3Result<Vec<Vec<u8>>> {
        if self.state == StreamingDecodeState::Trailer && self.encoded.is_empty() {
            if self.signing.is_some() {
                return Err(S3Failure::s3("IncompleteBody"));
            }
            ensure_trailers_present(&self.declared_trailers, &self.seen_trailers)?;
            self.state = StreamingDecodeState::Epilogue;
        }
        if self.state == StreamingDecodeState::Epilogue && self.encoded.is_empty() {
            self.check_decoded_length()?;
            self.state = StreamingDecodeState::Done;
            return Ok(Vec::new());
        }
        if self.state == StreamingDecodeState::Done && self.encoded.is_empty() {
            return Ok(Vec::new());
        }
        Err(S3Failure::s3("IncompleteBody"))
    }

    fn consume_header(&mut self) -> S3Result<bool> {
        let Some(line_end) = find_crlf(&self.encoded) else {
            if self.encoded.len() > MAX_CHUNK_HEADER_BYTES + 1 {
                return Err(S3Failure::s3("InvalidRequest"));
            }
            return Ok(false);
        };
        if line_end > MAX_CHUNK_HEADER_BYTES {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        let line = self.encoded[..line_end].to_vec();
        self.encoded.drain(..line_end + 2);
        let line = latin1(&line);
        let mut extensions = line.split(';').map(str::to_owned);
        let size_text = extensions.next().unwrap_or_default();
        if size_text.is_empty() || size_text.len() > 16 {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        let size =
            u64::from_str_radix(&size_text, 16).map_err(|_| S3Failure::s3("InvalidRequest"))?;
        let provided_signature = extensions.find_map(|extension| {
            let (name, value) = extension.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("chunk-signature")
                .then(|| value.trim().to_owned())
        });
        if self.signing.is_some() {
            let Some(signature) = provided_signature.as_deref() else {
                return Err(S3Failure::s3("SignatureDoesNotMatch"));
            };
            if signature.len() != 64 || !signature.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(S3Failure::s3("InvalidRequest"));
            }
        }
        if self.signing.is_some() && size > MAX_SIGNED_CHUNK_BYTES {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        if self.signing.is_none() && size > MAX_SAFE_LENGTH {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        self.check_decoded_size(size)?;
        self.current_size = size;
        self.current_remaining = size;
        self.current_signature = provided_signature;
        self.current_payload.clear();
        if self.signing.is_some() && size > 0 {
            self.current_payload = Vec::with_capacity(
                usize::try_from(size).map_err(|_| S3Failure::s3("EntityTooLarge"))?,
            );
        }
        if size == 0 {
            self.finish_chunk(&mut Vec::new())?;
        } else {
            self.state = StreamingDecodeState::Payload;
        }
        Ok(true)
    }

    fn consume_payload(&mut self, output: &mut Vec<Vec<u8>>) -> S3Result<bool> {
        if self.encoded.is_empty() {
            return Ok(false);
        }
        let take = self.current_remaining.min(self.encoded.len() as u64) as usize;
        let bytes = self.encoded.drain(..take).collect::<Vec<_>>();
        if self.signing.is_some() {
            self.current_payload.extend_from_slice(&bytes);
        } else {
            output.push(bytes);
        }
        self.current_remaining -= take as u64;
        self.decoded = self.decoded.saturating_add(take as u64);
        if self.current_remaining == 0 {
            self.state = StreamingDecodeState::PayloadCr;
        }
        Ok(true)
    }

    fn consume_payload_cr(&mut self) -> S3Result<bool> {
        let Some(byte) = self.encoded.first().copied() else {
            return Ok(false);
        };
        if byte != b'\r' {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        self.encoded.remove(0);
        self.state = StreamingDecodeState::PayloadLf;
        Ok(true)
    }

    fn consume_payload_lf(&mut self, output: &mut Vec<Vec<u8>>) -> S3Result<bool> {
        let Some(byte) = self.encoded.first().copied() else {
            return Ok(false);
        };
        if byte != b'\n' {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        self.encoded.remove(0);
        self.finish_chunk(output)?;
        Ok(true)
    }

    fn consume_trailer(&mut self) -> S3Result<bool> {
        let Some(line_end) = find_crlf(&self.encoded) else {
            if self.encoded.len() > MAX_TRAILER_BYTES.saturating_sub(self.trailer_block.len()) + 1 {
                return Err(S3Failure::s3("EntityTooLarge"));
            }
            return Ok(false);
        };
        if line_end > 0
            && line_end.saturating_add(1)
                > MAX_TRAILER_BYTES.saturating_sub(self.trailer_block.len())
        {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let raw_line = self.encoded[..line_end].to_vec();
        self.encoded.drain(..line_end + 2);
        if raw_line.is_empty() {
            if self.signing.is_some() {
                return Err(S3Failure::s3("InvalidRequest"));
            }
            ensure_trailers_present(&self.declared_trailers, &self.seen_trailers)?;
            self.state = StreamingDecodeState::Epilogue;
            return Ok(true);
        }
        let line = latin1(&raw_line);
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| S3Failure::s3("InvalidRequest"))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == TRAILER_SIGNATURE_HEADER {
            let Some(signing) = self.signing.as_ref() else {
                return Err(S3Failure::s3("InvalidRequest"));
            };
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(S3Failure::s3("InvalidRequest"));
            }
            ensure_trailers_present(&self.declared_trailers, &self.seen_trailers)?;
            let previous = self
                .previous_signature
                .as_deref()
                .ok_or_else(|| S3Failure::s3("SignatureDoesNotMatch"))?;
            let expected = sigv4::sign_trailer(
                &signing.credentials.secret_access_key,
                &signing.verified.scope,
                &sigv4::format_amz_date(signing.verified.timestamp_ms),
                previous,
                &sigv4::sha256_hex(&self.trailer_block),
            );
            if expected
                .as_bytes()
                .ct_eq(value.to_ascii_lowercase().as_bytes())
                .unwrap_u8()
                != 1
            {
                return Err(S3Failure::S3(protocol::error_with_message(
                    "SignatureDoesNotMatch",
                    "trailer signature does not match",
                )));
            }
            self.state = StreamingDecodeState::Epilogue;
            return Ok(true);
        }
        if name.is_empty()
            || !self
                .declared_trailers
                .iter()
                .any(|declared| declared == &name)
            || self.seen_trailers.iter().any(|seen| seen == &name)
        {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        if self
            .trailer_block
            .len()
            .saturating_add(raw_line.len())
            .saturating_add(1)
            > MAX_TRAILER_BYTES
        {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        self.trailer_block.extend_from_slice(&raw_line);
        self.trailer_block.push(b'\n');
        self.seen_trailers.push(name);
        Ok(true)
    }

    fn consume_epilogue(&mut self) -> S3Result<bool> {
        if self.encoded.is_empty() {
            return Ok(false);
        }
        if self.encoded.len() > 2 {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        if self.encoded[0] != b'\r' {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        if self.encoded.len() == 1 {
            return Ok(false);
        }
        if self.encoded[1] != b'\n' {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        self.encoded.clear();
        self.check_decoded_length()?;
        self.state = StreamingDecodeState::Done;
        Ok(true)
    }

    fn finish_chunk(&mut self, output: &mut Vec<Vec<u8>>) -> S3Result<()> {
        let terminal = self.current_size == 0;
        if let Some(signing) = self.signing.as_ref() {
            let previous = self
                .previous_signature
                .as_deref()
                .ok_or_else(|| S3Failure::s3("SignatureDoesNotMatch"))?;
            let expected =
                chunk_signature(signing, previous, &sigv4::sha256_hex(&self.current_payload));
            let provided = self
                .current_signature
                .as_deref()
                .expect("signed chunk signature checked above")
                .to_ascii_lowercase();
            if expected.as_bytes().ct_eq(provided.as_bytes()).unwrap_u8() != 1 {
                return Err(S3Failure::S3(protocol::error_with_message(
                    "SignatureDoesNotMatch",
                    "chunk signature does not match",
                )));
            }
            self.previous_signature = Some(expected);
            if !terminal && !self.current_payload.is_empty() {
                output.push(std::mem::take(&mut self.current_payload));
            }
        }
        if terminal {
            self.check_decoded_length()?;
            self.state = if self.declared_trailers.is_empty() {
                StreamingDecodeState::Epilogue
            } else {
                StreamingDecodeState::Trailer
            };
        } else {
            self.state = StreamingDecodeState::Header;
        }
        self.current_size = 0;
        self.current_remaining = 0;
        self.current_signature = None;
        self.current_payload.clear();
        Ok(())
    }

    fn check_decoded_size(&self, additional: u64) -> S3Result<()> {
        let next = self.decoded.saturating_add(additional);
        if next > self.max_body_bytes as u64 {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        if self.decoded_length.is_some_and(|length| next > length) {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        Ok(())
    }

    fn check_decoded_length(&self) -> S3Result<()> {
        if self
            .decoded_length
            .is_some_and(|length| self.decoded != length)
        {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        Ok(())
    }
}

fn find_crlf(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|pair| pair == b"\r\n")
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

fn ensure_trailers_present(declared: &[String], seen: &[String]) -> S3Result<()> {
    if declared
        .iter()
        .any(|name| !seen.iter().any(|seen| seen == name))
    {
        return Err(S3Failure::s3("InvalidRequest"));
    }
    Ok(())
}

fn chunk_signature(signing: &ChunkSigning<'_>, previous: &str, payload_hash: &str) -> String {
    sigv4::sign_chunk(
        &signing.credentials.secret_access_key,
        &signing.verified.scope,
        &sigv4::format_amz_date(signing.verified.timestamp_ms),
        previous,
        payload_hash,
    )
}

async fn multipart_parts(
    driver: &Arc<dyn FsDriver>,
    upload_id: &str,
    requested: &[(u32, String)],
) -> S3Result<Vec<(String, u64)>> {
    let mut previous = 0_u32;
    let mut parts = Vec::with_capacity(requested.len());
    for (index, (number, etag)) in requested.iter().enumerate() {
        if *number <= previous {
            return Err(S3Failure::s3("InvalidPartOrder"));
        }
        previous = *number;
        let path = part_path(upload_id, *number);
        let stats = driver.stat(&path).await.map_err(|error| {
            if matches!(
                error.code,
                mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
            ) {
                S3Failure::s3("InvalidPart")
            } else {
                S3Failure::Fs(error)
            }
        })?;
        if !stats.is_file() || unquote_etag(etag) != object_etag(&stats) {
            return Err(S3Failure::s3("InvalidPart"));
        }
        if index + 1 < requested.len() && stats.size < MIN_PART_SIZE {
            return Err(S3Failure::s3("EntityTooSmall"));
        }
        parts.push((path, stats.size));
    }
    Ok(parts)
}

fn multipart_parts_size(parts: &[(String, u64)]) -> u64 {
    parts
        .iter()
        .fold(0_u64, |total, (_, size)| total.saturating_add(*size))
}

async fn claim_multipart_finalization(
    driver: &Arc<dyn FsDriver>,
    upload_id: &str,
    key: &str,
) -> S3Result<String> {
    let _ = read_manifest(driver, upload_id, key).await?;
    let marker = format!("{}/{FINALIZATION_MARKER}", upload_directory(upload_id));
    let handle = driver.open(&marker, "wx", 0o600).await.map_err(|error| {
        if matches!(
            error.code,
            mount_rs_core::ErrorCode::Eexist
                | mount_rs_core::ErrorCode::Enoent
                | mount_rs_core::ErrorCode::Enotdir
        ) {
            S3Failure::s3("NoSuchUpload")
        } else {
            S3Failure::Fs(error)
        }
    })?;
    handle.close().await.map_err(S3Failure::Fs)?;
    Ok(marker)
}

async fn ensure_upload_not_finalizing(driver: &Arc<dyn FsDriver>, upload_id: &str) -> S3Result<()> {
    let marker = format!("{}/{FINALIZATION_MARKER}", upload_directory(upload_id));
    match driver.stat(&marker).await {
        Ok(_) => Err(S3Failure::s3("NoSuchUpload")),
        Err(error)
            if matches!(
                error.code,
                mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(S3Failure::Fs(error)),
    }
}

async fn read_manifest(
    driver: &Arc<dyn FsDriver>,
    upload_id: &str,
    key: &str,
) -> S3Result<UploadManifest> {
    if !is_upload_id(upload_id) {
        return Err(S3Failure::s3("NoSuchUpload"));
    }
    let path = format!("{}/{MANIFEST_NAME}", upload_directory(upload_id));
    let size = driver
        .stat(&path)
        .await
        .map_err(|error| match error {
            error
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                S3Failure::s3("NoSuchUpload")
            }
            error => S3Failure::Fs(error),
        })?
        .size;
    let bytes = read_bytes_range(driver.clone(), &path, None, size, DEFAULT_READ_CHUNK)
        .await
        .map_err(|error| match error {
            S3Failure::Fs(ref fs_error)
                if matches!(
                    fs_error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                S3Failure::s3("NoSuchUpload")
            }
            other => other,
        })?;
    let manifest: UploadManifest =
        serde_json::from_slice(&bytes).map_err(|_| S3Failure::s3("NoSuchUpload"))?;
    if manifest.key != key {
        return Err(S3Failure::s3("NoSuchUpload"));
    }
    Ok(manifest)
}

fn sigv4_error(failure: SigV4Failure, presigned: bool) -> protocol::S3Error {
    match failure {
        SigV4Failure::Missing => s3_error("AccessDenied"),
        SigV4Failure::Malformed(_) => s3_error(if presigned {
            "AuthorizationQueryParametersError"
        } else {
            "AuthorizationHeaderMalformed"
        }),
        SigV4Failure::UnsupportedAlgorithm => {
            protocol::error_with_message("InvalidRequest", "Please use AWS4-HMAC-SHA256.")
        }
        SigV4Failure::UnknownAccessKey => s3_error("InvalidAccessKeyId"),
        SigV4Failure::ScopeMismatch(_) => s3_error(if presigned {
            "AuthorizationQueryParametersError"
        } else {
            "AuthorizationHeaderMalformed"
        }),
        SigV4Failure::ClockSkew => s3_error("RequestTimeTooSkewed"),
        SigV4Failure::Expired => {
            protocol::error_with_message("AccessDenied", "Request has expired")
        }
        SigV4Failure::SignatureMismatch => s3_error("SignatureDoesNotMatch"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::{ErrorCode, FileHandle, FsError, MemoryFs, Result as FsResult, Stats};

    struct ReturnedWriteCount(usize);

    #[async_trait::async_trait]
    impl FileHandle for ReturnedWriteCount {
        async fn read(&self, _buffer: &mut [u8], _position: Option<u64>) -> FsResult<usize> {
            Err(FsError::new(ErrorCode::Eio))
        }

        async fn write(&self, _buffer: &[u8], _position: Option<u64>) -> FsResult<usize> {
            Ok(self.0)
        }

        async fn stat(&self) -> FsResult<Stats> {
            Err(FsError::new(ErrorCode::Eio))
        }

        async fn truncate(&self, _length: u64) -> FsResult<()> {
            Err(FsError::new(ErrorCode::Eio))
        }

        async fn close(&self) -> FsResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stream_writes_reject_zero_and_oversized_driver_counts() {
        for returned in [0, 4] {
            let handle: Arc<dyn FileHandle> = Arc::new(ReturnedWriteCount(returned));
            let mut position = 0;
            let error = write_stream_chunk(&handle, &mut position, b"abc")
                .await
                .expect_err("invalid write count");
            assert!(matches!(error, S3Failure::S3(ref error) if error.code == "InternalError"));
            assert_eq!(position, 0);
        }
    }

    #[tokio::test]
    async fn session_hooks_control_ids_clock_and_diagnostics() {
        let request_ids = Arc::new(StdMutex::new(Vec::new()));
        let error_reports = Arc::new(StdMutex::new(Vec::new()));
        let assertion_reports = Arc::new(StdMutex::new(Vec::new()));
        let clock_calls = Arc::new(AtomicU64::new(0));

        let request_ids_for_hook = Arc::clone(&request_ids);
        let error_reports_for_hook = Arc::clone(&error_reports);
        let assertion_reports_for_hook = Arc::clone(&assertion_reports);
        let clock_calls_for_hook = Arc::clone(&clock_calls);
        let options = S3SessionOptions {
            hooks: S3SessionHooks {
                now_ms: Some(Arc::new(move || {
                    clock_calls_for_hook.fetch_add(1, Ordering::Relaxed);
                    Some(1_700_000_000_000)
                })),
                request_id: Some(Arc::new(move || {
                    request_ids_for_hook
                        .lock()
                        .expect("request id hook")
                        .push("hooked-request".to_owned());
                    Some("hooked-request".to_owned())
                })),
                on_error: Some(Arc::new(move |message, head| {
                    error_reports_for_hook
                        .lock()
                        .expect("error hook")
                        .push((message, head.map(|head| (head.method, head.target))));
                })),
                on_assertion: Some(Arc::new(move |message| {
                    assertion_reports_for_hook
                        .lock()
                        .expect("assertion hook")
                        .push(message);
                })),
            },
            ..S3SessionOptions::default()
        };
        let session = S3Session::new_with_options(MemoryFs::empty(), options);

        assert_eq!(session.current_now_ms(), 1_700_000_000_000);
        assert_eq!(clock_calls.load(Ordering::Relaxed), 1);
        let head = S3RequestHead::new("GET", "/mountx/missing.txt");
        let response = session.handle_request(head.clone(), Vec::new()).await;
        assert_eq!(response.status, 404);
        assert_eq!(
            response
                .headers
                .iter()
                .find(|(name, _)| name == "x-amz-request-id")
                .map(|(_, value)| value.as_str()),
            Some("hooked-request")
        );
        {
            let error_reports = error_reports.lock().expect("error report");
            assert_eq!(error_reports.len(), 1);
            assert!(error_reports[0].0.contains("ENOENT"));
            assert_eq!(
                error_reports[0].1,
                Some(("GET".to_owned(), "/mountx/missing.txt".to_owned()))
            );
        }

        session.finish_request(Some(999), &head).await;
        assert_eq!(
            assertion_reports
                .lock()
                .expect("assertion report")
                .as_slice(),
            &["GET /mountx/missing.txt was answered twice".to_owned()]
        );
    }
}
