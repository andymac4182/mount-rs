//! WebDAV request/reply session over the shared `FsDriver` contract.

use std::collections::{BTreeMap, HashMap};
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use mount_rs_core::{ErrorCode, FileType, FsDriver, FsError, MkdirOptions, Stats};

use crate::constants::{
    ALLOW_HEADER, COLLECTION_CONTENT_TYPE, DAV_COMPLIANCE, DAV_NS, MAX_DIRECTORY_ENTRIES,
    MS_AUTHOR_VIA, READ_CHUNK_BYTES, RESOURCE_CONTENT_TYPE, status_for_error,
};
use crate::locks::{
    DavLock, DavLockGrant, DavLockRequest, DavLockTable, DavLockTableOptions, LockDepth,
};
use crate::protocol::{
    DavFault, DavPropertyName, Depth, FileBody, MultistatusEntry, PropfindRequest,
    ProppatchInstruction, Propstat, RangeSpec, WebdavBody, WebdavError, WebdavRequestHead,
    WebdavResponse, XmlNode, basic_authorization, condition_matches, dav_property,
    encode_lock_response, encode_multistatus, evaluate_conditionals, fault_response,
    format_http_date_ms, format_iso_date_ms, format_lock_token, href_of, lock_discovery_node,
    parse_depth, parse_destination, parse_if, parse_lock_info, parse_lock_token, parse_overwrite,
    parse_propfind, parse_proppatch, parse_range, parse_timeout, resource_etag, status_of_error,
    submitted_tokens, supported_lock_node,
};

/// A transport-neutral stream of request-body chunks.
///
/// The WebDAV session consumes `PUT` bodies incrementally and only buffers the
/// bounded XML grammars. HTTP adapters implement this trait for their native
/// body stream; callers that already have bytes can continue using
/// [`WebdavSession::handle_request`]. A known 413 size-limit fault may be
/// followed by more chunks while the session drains the request for HTTP/1.1
/// reuse; a non-recoverable body fault closes the connection boundary instead
/// of requiring a further drain.
pub trait WebdavRequestBody {
    fn poll_next_chunk(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, WebdavError>>>;
}

struct BytesRequestBody {
    body: Option<Bytes>,
}

impl BytesRequestBody {
    fn new(body: &[u8]) -> Self {
        Self {
            body: Some(Bytes::copy_from_slice(body)),
        }
    }
}

impl WebdavRequestBody for BytesRequestBody {
    fn poll_next_chunk(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, WebdavError>>> {
        Poll::Ready(self.body.take().map(Ok))
    }
}

/// Optional server-side Basic authentication.
#[derive(Debug, Clone)]
pub struct WebdavCredentials {
    pub username: String,
    pub password: String,
}

/// Session policy.  The defaults are intentionally bounded for HTTP request
/// bodies while retaining the upstream 256 KiB XML budget.
#[derive(Clone)]
pub struct WebdavSessionOptions {
    pub credentials: Option<WebdavCredentials>,
    pub realm: String,
    pub read_chunk_bytes: usize,
    pub max_xml_bytes: usize,
    pub max_body_bytes: Option<usize>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub locks: DavLockTableOptions,
    pub debug: bool,
}

impl std::fmt::Debug for WebdavSessionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebdavSessionOptions")
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "configured"),
            )
            .field("realm", &self.realm)
            .field("read_chunk_bytes", &self.read_chunk_bytes)
            .field("max_xml_bytes", &self.max_xml_bytes)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("locks", &self.locks)
            .field("debug", &self.debug)
            .finish()
    }
}

impl Default for WebdavSessionOptions {
    fn default() -> Self {
        Self {
            credentials: None,
            realm: "mountx".to_owned(),
            read_chunk_bytes: READ_CHUNK_BYTES,
            max_xml_bytes: crate::constants::MAX_XML_BYTES,
            max_body_bytes: Some(crate::constants::DEFAULT_MAX_REQUEST_BYTES),
            now: None,
            locks: DavLockTableOptions::default(),
            debug: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WebdavSessionStats {
    pub requests: u64,
    pub replies: u64,
    pub errors: u64,
    pub methods: BTreeMap<String, u64>,
    pub assertions: u64,
}

/// Synchronous callback used by a session to report one request that ended in
/// an error reply. The request head is owned by the callback so adapters can
/// safely hand it to another runtime without borrowing the dispatch future.
pub type WebdavErrorHook = Arc<dyn Fn(WebdavError, WebdavRequestHead) + Send + Sync + 'static>;

/// Optional request-level hooks for a [`WebdavSession`]. Kept separate from
/// [`WebdavSessionOptions`] so existing option literals remain compatible.
#[derive(Clone, Default)]
pub struct WebdavSessionHooks {
    pub on_error: Option<WebdavErrorHook>,
}

#[derive(Debug, Clone)]
struct Failure {
    path: String,
    collection: bool,
    status: u16,
}

#[derive(Debug, Clone)]
struct Guard {
    now: i64,
    submitted: Vec<String>,
    lists: Option<Vec<crate::protocol::IfList>>,
}

#[derive(Debug, Clone)]
struct ResourceState {
    tokens: Vec<String>,
    etag: Option<String>,
}

struct PropertyOutcome {
    name: DavPropertyName,
    status: u16,
    condition: Option<String>,
    set_mtime_ms: Option<i64>,
}

/// A request/reply WebDAV session.  One session is shared by every HTTP
/// connection for a server, so its lock table is the share's lock table.
pub struct WebdavSession {
    pub driver: Arc<dyn FsDriver>,
    pub locks: Arc<Mutex<DavLockTable>>,
    pub options: WebdavSessionOptions,
    hooks: WebdavSessionHooks,
    stats: Mutex<WebdavSessionStats>,
    assertions: Mutex<Vec<String>>,
}

impl std::fmt::Debug for WebdavSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebdavSession")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl WebdavSession {
    pub fn new(driver: Arc<dyn FsDriver>, options: WebdavSessionOptions) -> Self {
        Self::new_with_hooks(driver, options, WebdavSessionHooks::default())
    }

    pub fn new_with_hooks(
        driver: Arc<dyn FsDriver>,
        options: WebdavSessionOptions,
        hooks: WebdavSessionHooks,
    ) -> Self {
        Self {
            driver,
            locks: Arc::new(Mutex::new(DavLockTable::new(options.locks.clone()))),
            options,
            hooks,
            stats: Mutex::new(WebdavSessionStats::default()),
            assertions: Mutex::new(Vec::new()),
        }
    }

    pub fn stats(&self) -> WebdavSessionStats {
        self.stats
            .lock()
            .map(|stats| stats.clone())
            .unwrap_or_default()
    }

    pub fn assertions(&self) -> Vec<String> {
        self.assertions
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    pub fn lock_count(&self) -> usize {
        let now = self.now();
        self.locks
            .lock()
            .map(|mut locks| locks.size(now))
            .unwrap_or(0)
    }

    /// Return the active write locks in this session after expiring stale
    /// entries. The transport keeps the lock table private to request
    /// dispatch, but embedders need a read-only state view for lifecycle and
    /// parity inspection.
    pub fn lock_records(&self) -> Vec<DavLock> {
        let now = self.now();
        self.locks
            .lock()
            .map(|mut locks| locks.all(now))
            .unwrap_or_default()
    }

    /// Answer one request and never reject.  Driver and protocol errors become
    /// one response, matching the upstream exactly-one-reply contract.
    pub async fn handle_request<B>(&self, head: WebdavRequestHead, body: B) -> WebdavResponse
    where
        B: AsRef<[u8]> + Send,
    {
        self.handle_request_stream(head, BytesRequestBody::new(body.as_ref()))
            .await
    }

    /// Answer one request from an asynchronous body stream and never reject.
    ///
    /// `PUT` writes each non-empty chunk before asking the transport for the
    /// next one. XML request grammars remain bounded and buffered. Any body
    /// left after dispatch is drained while its framing remains trustworthy;
    /// a non-recoverable body fault is reported and closes the connection
    /// boundary instead of being treated as reusable EOF.
    pub async fn handle_request_stream<B>(
        &self,
        head: WebdavRequestHead,
        mut body: B,
    ) -> WebdavResponse
    where
        B: WebdavRequestBody + Send + Unpin,
    {
        self.count_request(&head.method);
        let result = self.dispatch_stream(&head, &mut body).await;
        let dispatch_body_fault = matches!(&result, Err(WebdavError::Body(_)));
        let drain_error = drain_body(&mut body).await;
        if let Some(error) = drain_error.as_ref()
            && !is_recoverable_body_drain_error(error)
            && result.is_ok()
        {
            self.report_error(error, &head);
        }
        let mut response = match result {
            Ok(response) => response,
            Err(error) => {
                self.report_error(&error, &head);
                fault_response(&error)
            }
        };
        if dispatch_body_fault
            || drain_error
                .as_ref()
                .is_some_and(|error| !is_recoverable_body_drain_error(error))
        {
            response
                .headers
                .insert("connection".to_owned(), "close".to_owned());
        }
        if head.method.eq_ignore_ascii_case("HEAD") {
            response.body = None;
        }
        self.count_reply(response.status);
        response
    }

    async fn dispatch_stream<B>(
        &self,
        head: &WebdavRequestHead,
        body: &mut B,
    ) -> Result<WebdavResponse, WebdavError>
    where
        B: WebdavRequestBody + Unpin,
    {
        let method = head.method.to_ascii_uppercase();
        self.authorize(head)?;
        if method == "OPTIONS" {
            return Ok(self.options_response());
        }
        let path = crate::protocol::parse_target_path(&head.target)?;
        let guard = self.guard(head, &path, method != "LOCK").await?;
        match method.as_str() {
            "GET" | "HEAD" => self.get(head, &path).await,
            "PUT" => self.put(head, &path, body, &guard).await,
            "DELETE" => self.delete(head, &path, &guard).await,
            "MKCOL" => self.mkcol(&path, body, &guard).await,
            "COPY" => self.copy_or_move(head, &path, false, &guard).await,
            "MOVE" => self.copy_or_move(head, &path, true, &guard).await,
            "PROPFIND" => self.propfind(head, &path, body, &guard).await,
            "PROPPATCH" => self.proppatch(&path, body, &guard).await,
            "LOCK" => self.lock(head, &path, body, &guard).await,
            "UNLOCK" => self.unlock(head, &path),
            _ => Err(refuse(405).with_header("allow", ALLOW_HEADER).into()),
        }
    }

    fn count_request(&self, method: &str) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.requests += 1;
            *stats
                .methods
                .entry(method.to_ascii_uppercase())
                .or_default() += 1;
        }
    }

    fn count_reply(&self, status: u16) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.replies += 1;
            if status >= 400 {
                stats.errors += 1;
            }
        }
    }

    fn report_error(&self, error: &WebdavError, head: &WebdavRequestHead) {
        let Some(hook) = self.hooks.on_error.as_ref().cloned() else {
            return;
        };
        let error = error.clone();
        let head = head.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            hook(error, head);
        }));
    }

    fn now(&self) -> i64 {
        self.options.now.as_ref().map_or_else(now_ms, |now| now())
    }

    fn authorize(&self, head: &WebdavRequestHead) -> Result<(), WebdavError> {
        let Some(credentials) = &self.options.credentials else {
            return Ok(());
        };
        let valid = head.headers.get("authorization").is_some_and(|header| {
            basic_authorization(header, &credentials.username, &credentials.password)
        });
        if valid {
            return Ok(());
        }
        let realm = self.options.realm.replace(['"', '\\'], "");
        Err(DavFault::new(401)
            .with_header(
                "www-authenticate",
                format!("Basic realm=\"{realm}\", charset=\"UTF-8\""),
            )
            .into())
    }

    fn options_response(&self) -> WebdavResponse {
        let mut response = WebdavResponse::empty(200);
        response
            .headers
            .insert("dav".to_owned(), DAV_COMPLIANCE.to_owned());
        response
            .headers
            .insert("allow".to_owned(), ALLOW_HEADER.to_owned());
        response
            .headers
            .insert("ms-author-via".to_owned(), MS_AUTHOR_VIA.to_owned());
        response
            .headers
            .insert("accept-ranges".to_owned(), "bytes".to_owned());
        response
    }

    async fn get(
        &self,
        head: &WebdavRequestHead,
        path: &str,
    ) -> Result<WebdavResponse, WebdavError> {
        let stats = self.stat(path).await?;
        if stats.is_directory() {
            return Err(refuse(405)
                .with_header("allow", ALLOW_HEADER)
                .with_message("a collection has no GET body")
                .into());
        }
        if !stats.is_file() {
            return Err(refuse(403)
                .with_message("that resource is not a regular file")
                .into());
        }
        let conditional = evaluate_conditionals(&head.headers, Some(&stats), &head.method);
        if conditional.status == 304 {
            return Ok(WebdavResponse {
                status: 304,
                headers: self.resource_headers(&stats),
                body: None,
            });
        }
        if conditional.status == 412 {
            return Err(refuse(412)
                .with_message("a conditional header did not match this resource")
                .into());
        }
        let range = parse_range(head.headers.get("range").map(String::as_str), stats.size);
        let mut headers = self.resource_headers(&stats);
        let (status, start, length) = match range {
            RangeSpec::Full => (200, 0, stats.size),
            RangeSpec::Range { start, length, end } => {
                headers.insert(
                    "content-range".to_owned(),
                    format!("bytes {start}-{end}/{}", stats.size),
                );
                (206, start, length)
            }
            RangeSpec::Unsatisfiable => {
                return Err(refuse(416)
                    .with_header("content-range", format!("bytes */{}", stats.size))
                    .into());
            }
        };
        headers.insert("content-length".to_owned(), length.to_string());
        if length == 0 || head.method.eq_ignore_ascii_case("HEAD") {
            return Ok(WebdavResponse {
                status,
                headers,
                body: None,
            });
        }
        let handle = self.driver.open(path, "r", 0).await?;
        Ok(WebdavResponse {
            status,
            headers,
            body: Some(WebdavBody::File(FileBody {
                handle,
                start,
                length,
                chunk_size: self.options.read_chunk_bytes.max(1),
            })),
        })
    }

    fn resource_headers(&self, stats: &Stats) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_owned(), RESOURCE_CONTENT_TYPE.to_owned());
        headers.insert(
            "last-modified".to_owned(),
            format_http_date_ms(stats.mtime_ms),
        );
        headers.insert("etag".to_owned(), resource_etag(stats));
        headers.insert("accept-ranges".to_owned(), "bytes".to_owned());
        headers
    }

    async fn put<B>(
        &self,
        head: &WebdavRequestHead,
        path: &str,
        body: &mut B,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError>
    where
        B: WebdavRequestBody + Unpin,
    {
        if head.headers.contains_key("content-range") {
            return Err(refuse(400)
                .with_message("Content-Range is not allowed on PUT")
                .into());
        }
        if path == "/" {
            return Err(refuse(405).with_header("allow", ALLOW_HEADER).into());
        }
        let existing = self.stat_or_absent(path).await?;
        if existing.as_ref().is_some_and(Stats::is_directory) {
            return Err(refuse(405).with_header("allow", ALLOW_HEADER).into());
        }
        self.require_collection(&parent(path)).await?;
        self.require_writable(path, guard, existing.is_none())?;
        self.check_conditionals(head, existing.as_ref())?;
        self.write_file_stream(path, body).await?;
        self.durability_barrier().await?;
        let stats = self.stat_or_absent(path).await?;
        let mut headers = BTreeMap::new();
        headers.insert("content-length".to_owned(), "0".to_owned());
        if let Some(stats) = stats {
            headers.insert("etag".to_owned(), resource_etag(&stats));
            headers.insert(
                "last-modified".to_owned(),
                format_http_date_ms(stats.mtime_ms),
            );
        }
        Ok(WebdavResponse {
            status: if existing.is_some() { 204 } else { 201 },
            headers,
            body: None,
        })
    }

    async fn write_file_stream<B>(&self, path: &str, body: &mut B) -> Result<(), WebdavError>
    where
        B: WebdavRequestBody + Unpin,
    {
        // Keep the destination write in place. The pinned WebDAV oracle opens
        // the resource at the first body byte and deliberately leaves a
        // written prefix when the request body fails; staging here would
        // change that supported protocol contract and would require a
        // stronger atomic-replace guarantee than FsDriver exposes.
        let cap = self.options.max_body_bytes;
        let mut handle = None;
        let mut written = 0_u64;
        let result: Result<(), WebdavError> = async {
            while let Some(chunk) = next_body_chunk(body).await {
                let chunk = chunk?;
                if chunk.is_empty() {
                    continue;
                }
                let chunk_len = chunk.len() as u64;
                if cap.is_some_and(|limit| {
                    written > limit as u64 || chunk_len > (limit as u64).saturating_sub(written)
                }) {
                    return Err(WebdavError::from(
                        refuse(413).with_message("the PUT body exceeds its byte budget"),
                    ));
                }
                if handle.is_none() {
                    handle = Some(self.driver.open(path, "w", 0o666).await?);
                }
                let file = handle.as_ref().expect("PUT handle opened above");
                let mut offset = 0_usize;
                while offset < chunk.len() {
                    let position = written.checked_add(offset as u64).ok_or_else(|| {
                        WebdavError::from(FsError::new(ErrorCode::Eio).with_syscall("write"))
                    })?;
                    let count = file.write(&chunk[offset..], Some(position)).await?;
                    if count == 0 || count > chunk.len() - offset {
                        return Err(WebdavError::from(
                            FsError::new(ErrorCode::Eio).with_syscall("write"),
                        ));
                    }
                    offset += count;
                }
                written = written.checked_add(chunk_len).ok_or_else(|| {
                    WebdavError::from(FsError::new(ErrorCode::Eio).with_syscall("write"))
                })?;
            }
            if handle.is_none() {
                // An empty request still creates or truncates the resource.
                handle = Some(self.driver.open(path, "w", 0o666).await?);
            }
            Ok::<(), WebdavError>(())
        }
        .await;
        let close = match handle {
            Some(handle) => handle.close().await.map_err(WebdavError::from),
            None => Ok(()),
        };
        match (result, close) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    async fn write_file(&self, path: &str, body: &[u8]) -> Result<(), WebdavError> {
        let handle = self.driver.open(path, "w", 0o666).await?;
        let result = async {
            let mut position = 0_u64;
            while position < body.len() as u64 {
                let remaining = body.len() - position as usize;
                let count = handle
                    .write(&body[position as usize..], Some(position))
                    .await?;
                if count == 0 || count > remaining {
                    return Err(FsError::new(ErrorCode::Eio).with_syscall("write"));
                }
                position += count as u64;
            }
            Ok::<(), FsError>(())
        }
        .await;
        let close = handle.close().await;
        result.and(close).map_err(WebdavError::from)
    }

    /// Await the driver's persistence barrier before acknowledging a
    /// filesystem mutation. Volatile drivers remain a successful no-op;
    /// durable drivers must implement `syncfs` or the request fails rather
    /// than reporting a false durable success.
    async fn durability_barrier(&self) -> Result<(), WebdavError> {
        if self.driver.capabilities().durable_writes {
            self.driver.syncfs().await?;
        }
        Ok(())
    }

    async fn delete(
        &self,
        head: &WebdavRequestHead,
        path: &str,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError> {
        if path == "/" {
            return Err(refuse(403)
                .with_message("the root collection is the share itself")
                .into());
        }
        let stats = self.link_stat(path).await?;
        let depth = parse_depth(
            head.headers.get("depth").map(String::as_str),
            Depth::Infinity,
        )
        .ok_or_else(|| refuse(400).with_message("Depth must be 0, 1 or infinity"))?;
        if stats.is_directory() && depth != Depth::Infinity {
            return Err(refuse(400)
                .with_message("DELETE of a collection is Depth: infinity")
                .into());
        }
        self.require_writable(path, guard, true)?;
        let locked = self.locked_members(path, guard);
        if !locked.is_empty() {
            return Ok(self.multistatus(&locked));
        }
        // A failure on the request resource is an ordinary HTTP error.
        // Only recursive collection deletion collects per-resource failures
        // into a multistatus (matching mountx's #delete / #deleteTree split).
        if !stats.is_directory() {
            self.driver.unlink(path).await?;
            self.discard_unmapped(path).await;
            self.durability_barrier().await?;
            return Ok(WebdavResponse::empty(204));
        }
        let (failures, mutated) = self.delete_tree(path).await;
        self.discard_unmapped(path).await;
        if mutated {
            self.durability_barrier().await?;
        }
        if failures.is_empty() {
            Ok(WebdavResponse::empty(204))
        } else {
            Ok(self.multistatus(&failures))
        }
    }

    fn delete_tree<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = (Vec<Failure>, bool)> + Send + 'a>> {
        Box::pin(async move {
            let entries = match self
                .driver
                .readdir_bounded(path, MAX_DIRECTORY_ENTRIES)
                .await
            {
                Ok(entries) => entries,
                Err(error) if is_absent(&error) => return (Vec::new(), false),
                Err(error) => {
                    return (
                        vec![Failure {
                            path: path.to_owned(),
                            collection: true,
                            status: status_for_error(error.code),
                        }],
                        false,
                    );
                }
            };
            let mut failures = Vec::new();
            let mut mutated = false;
            for entry in entries {
                let child = join(path, &entry.name);
                if entry.file_type == FileType::Directory {
                    let (child_failures, child_mutated) = self.delete_tree(&child).await;
                    failures.extend(child_failures);
                    mutated |= child_mutated;
                } else {
                    match self.driver.unlink(&child).await {
                        Ok(()) => mutated = true,
                        Err(error) if is_absent(&error) => {}
                        Err(error) => failures.push(Failure {
                            path: child,
                            collection: false,
                            status: status_for_error(error.code),
                        }),
                    }
                }
            }
            if failures.is_empty() {
                match self.driver.rmdir(path).await {
                    Ok(()) => mutated = true,
                    Err(error) if is_absent(&error) => {}
                    Err(error) => failures.push(Failure {
                        path: path.to_owned(),
                        collection: true,
                        status: status_for_error(error.code),
                    }),
                }
            }
            (failures, mutated)
        })
    }

    async fn mkcol<B>(
        &self,
        path: &str,
        body: &mut B,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError>
    where
        B: WebdavRequestBody + Unpin,
    {
        let body = collect_body_stream(body, self.options.max_xml_bytes).await?;
        if !body.is_empty() {
            return Err(refuse(415)
                .with_message("this server defines no MKCOL request body")
                .into());
        }
        if path == "/" || self.stat_or_absent(path).await?.is_some() {
            return Err(refuse(405).with_header("allow", ALLOW_HEADER).into());
        }
        self.require_collection(&parent(path)).await?;
        self.require_writable(path, guard, true)?;
        self.driver.mkdir(path, MkdirOptions::default()).await?;
        self.durability_barrier().await?;
        Ok(WebdavResponse::empty(201))
    }

    async fn copy_or_move(
        &self,
        head: &WebdavRequestHead,
        path: &str,
        moving: bool,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError> {
        let destination = parse_destination(
            head.headers.get("destination").map(String::as_str),
            head.headers.get("host").map(String::as_str),
        )?;
        let overwrite = parse_overwrite(head.headers.get("overwrite").map(String::as_str))
            .ok_or_else(|| refuse(400).with_message("Overwrite must be T or F"))?;
        let depth = parse_depth(
            head.headers.get("depth").map(String::as_str),
            Depth::Infinity,
        )
        .ok_or_else(|| refuse(400).with_message("Depth is invalid"))?;
        if moving && depth != Depth::Infinity {
            return Err(refuse(400).with_message("MOVE is Depth: infinity").into());
        }
        if !moving && depth == Depth::One {
            return Err(refuse(400)
                .with_message("COPY is Depth: 0 or infinity")
                .into());
        }
        let stats = self.stat(path).await?;
        if path == "/" {
            return Err(refuse(403)
                .with_message("the root collection is the share itself")
                .into());
        }
        if destination == path {
            return Err(refuse(403)
                .with_message("the destination is the source")
                .into());
        }
        if !moving && stats.is_directory() && self.link_stat(path).await?.is_symbolic_link() {
            return Err(refuse(403)
                .with_message("a symbolic link to a collection is not a collection to copy")
                .into());
        }
        if stats.is_directory() && mount_rs_core::path::is_path_inside(&destination, path) {
            return Err(refuse(403)
                .with_message("the destination is inside the source collection")
                .into());
        }
        self.require_collection(&parent(&destination)).await?;
        let existing = self.stat_or_absent(&destination).await?;
        if moving {
            self.require_writable(path, guard, true)?;
        }
        self.require_writable(&destination, guard, existing.is_none())?;
        let locked = [
            if moving {
                self.locked_members(path, guard)
            } else {
                Vec::new()
            },
            if existing.is_some() {
                self.locked_members(&destination, guard)
            } else {
                Vec::new()
            },
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if !locked.is_empty() {
            return Ok(self.multistatus(&locked));
        }
        if existing.is_some() && !overwrite {
            return Err(refuse(412)
                .with_message("the destination exists and Overwrite is F")
                .into());
        }
        let mut mutated = false;
        if existing.is_some() {
            let (failures, deleted) = if existing.as_ref().is_some_and(Stats::is_directory) {
                self.delete_tree(&destination).await
            } else {
                match self.driver.unlink(&destination).await {
                    Ok(()) => (Vec::new(), true),
                    Err(FsError {
                        code: ErrorCode::Enoent,
                        ..
                    }) => (Vec::new(), false),
                    Err(error) => (
                        vec![Failure {
                            path: destination.clone(),
                            collection: false,
                            status: status_for_error(error.code),
                        }],
                        false,
                    ),
                }
            };
            mutated |= deleted;
            if !failures.is_empty() {
                if mutated {
                    self.durability_barrier().await?;
                }
                return Ok(self.multistatus(&failures));
            }
        }
        if moving {
            self.driver.rename(path, &destination).await?;
            mutated = true;
            self.discard_unmapped(path).await;
            self.discard_unmapped(&destination).await;
        } else {
            let (failures, copied) = self
                .copy_tree(path, &destination, &stats, depth == Depth::Infinity)
                .await;
            mutated |= copied;
            self.discard_unmapped(&destination).await;
            if !failures.is_empty() {
                if mutated {
                    self.durability_barrier().await?;
                }
                return Ok(self.multistatus(&failures));
            }
        }
        if mutated {
            self.durability_barrier().await?;
        }
        Ok(WebdavResponse::empty(if existing.is_some() {
            204
        } else {
            201
        }))
    }

    fn copy_tree<'a>(
        &'a self,
        source: &'a str,
        destination: &'a str,
        stats: &'a Stats,
        deep: bool,
    ) -> Pin<Box<dyn Future<Output = (Vec<Failure>, bool)> + Send + 'a>> {
        Box::pin(async move {
            if !stats.is_directory() {
                if !stats.is_file() {
                    return (
                        vec![Failure {
                            path: source.to_owned(),
                            collection: false,
                            status: 403,
                        }],
                        false,
                    );
                }
                return match self.copy_file(source, destination, stats.size).await {
                    Ok(()) => (Vec::new(), true),
                    Err(error) => (
                        vec![Failure {
                            path: source.to_owned(),
                            collection: false,
                            status: status_of_error(&error),
                        }],
                        false,
                    ),
                };
            }
            if let Err(error) = self
                .driver
                .mkdir(destination, MkdirOptions::default())
                .await
            {
                return (
                    vec![Failure {
                        path: source.to_owned(),
                        collection: true,
                        status: status_for_error(error.code),
                    }],
                    false,
                );
            }
            if !deep {
                return (Vec::new(), true);
            }
            let entries = match self
                .driver
                .readdir_bounded(source, MAX_DIRECTORY_ENTRIES)
                .await
            {
                Ok(entries) => entries,
                Err(error) => {
                    return (
                        vec![Failure {
                            path: source.to_owned(),
                            collection: true,
                            status: status_for_error(error.code),
                        }],
                        true,
                    );
                }
            };
            let mut failures = Vec::new();
            let mut mutated = true;
            for entry in entries {
                let child = join(source, &entry.name);
                let target = join(destination, &entry.name);
                let child_stats = match self.stat_or_absent(&child).await {
                    Ok(Some(stats)) => stats,
                    Ok(None) => continue,
                    Err(error) => {
                        failures.push(Failure {
                            path: child,
                            collection: entry.file_type == FileType::Directory,
                            status: status_of_error(&error),
                        });
                        continue;
                    }
                };
                if entry.file_type == FileType::Symlink && child_stats.is_directory() {
                    failures.push(Failure {
                        path: child,
                        collection: true,
                        status: 403,
                    });
                } else {
                    let (child_failures, child_mutated) =
                        self.copy_tree(&child, &target, &child_stats, true).await;
                    failures.extend(child_failures);
                    mutated |= child_mutated;
                }
            }
            (failures, mutated)
        })
    }

    async fn copy_file(
        &self,
        source: &str,
        destination: &str,
        size: u64,
    ) -> Result<(), WebdavError> {
        let from = self.driver.open(source, "r", 0).await?;
        let to = match self.driver.open(destination, "w", 0o666).await {
            Ok(handle) => handle,
            Err(error) => {
                let _ = from.close().await;
                return Err(error.into());
            }
        };
        let result = async {
            let mut position = 0_u64;
            let mut buffer = vec![0_u8; self.options.read_chunk_bytes.max(1)];
            while position < size {
                let wanted = (size - position).min(buffer.len() as u64) as usize;
                let count = from.read(&mut buffer[..wanted], Some(position)).await?;
                if count == 0 {
                    return Err(FsError::new(ErrorCode::Eio)
                        .with_syscall("read")
                        .with_message("short WebDAV COPY source"));
                }
                if count > wanted {
                    return Err(FsError::new(ErrorCode::Eio)
                        .with_syscall("read")
                        .with_message("driver returned more bytes than requested"));
                }
                let mut written = 0;
                while written < count {
                    let n = to
                        .write(&buffer[written..count], Some(position + written as u64))
                        .await?;
                    if n == 0 || n > count - written {
                        return Err(FsError::new(ErrorCode::Eio).with_syscall("write"));
                    }
                    written += n;
                }
                position += count as u64;
            }
            Ok::<(), FsError>(())
        }
        .await;
        let close_to = to.close().await;
        let close_from = from.close().await;
        result
            .and(close_to)
            .and(close_from)
            .map_err(WebdavError::from)
    }

    async fn propfind<B>(
        &self,
        head: &WebdavRequestHead,
        path: &str,
        body: &mut B,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError>
    where
        B: WebdavRequestBody + Unpin,
    {
        let depth = parse_depth(
            head.headers.get("depth").map(String::as_str),
            Depth::Infinity,
        )
        .ok_or_else(|| refuse(400).with_message("Depth must be 0, 1 or infinity"))?;
        if depth == Depth::Infinity {
            return Err(refuse(403)
                .with_condition("propfind-finite-depth", Vec::new())
                .into());
        }
        let body = collect_body_stream(body, self.options.max_xml_bytes).await?;
        let request = parse_propfind(&body, self.options.max_xml_bytes)?;
        let stats = self.stat(path).await?;
        let now = guard.now;
        let mut entries = vec![MultistatusEntry {
            href: href_of(path, stats.is_directory()),
            propstat: self.propstats(path, &stats, &request, now).await?,
            status: None,
        }];
        if depth == Depth::One && stats.is_directory() {
            let children = self
                .driver
                .readdir_bounded(path, MAX_DIRECTORY_ENTRIES)
                .await
                .map_err(propfind_directory_error)?;
            for entry in children {
                let child = join(path, &entry.name);
                match self.stat_or_absent(&child).await {
                    Ok(Some(child_stats)) => entries.push(MultistatusEntry {
                        href: href_of(&child, child_stats.is_directory()),
                        propstat: self.propstats(&child, &child_stats, &request, now).await?,
                        status: None,
                    }),
                    Ok(None) => {}
                    Err(error) => entries.push(MultistatusEntry {
                        href: href_of(&child, entry.file_type == FileType::Directory),
                        propstat: Vec::new(),
                        status: Some(status_of_error(&error)),
                    }),
                }
            }
        }
        Ok(crate::protocol::xml_body(
            207,
            &encode_multistatus(&entries),
            BTreeMap::new(),
        ))
    }

    async fn propstats(
        &self,
        path: &str,
        stats: &Stats,
        request: &PropfindRequest,
        now: i64,
    ) -> Result<Vec<Propstat>, WebdavError> {
        let explicit = matches!(request, PropfindRequest::Prop(_));
        let names: Vec<DavPropertyName> = match request {
            PropfindRequest::Allprop | PropfindRequest::Propname => crate::protocol::ALLPROP_NAMES
                .iter()
                .map(|name| dav_property(name))
                .collect(),
            PropfindRequest::Prop(names) => names.clone(),
        };
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for name in names {
            match self.property(&name, path, stats, now).await? {
                Some(node) => found.push(if matches!(request, PropfindRequest::Propname) {
                    property_node(&name)
                } else {
                    node
                }),
                None if explicit => missing.push(property_node(&name)),
                None => {}
            }
        }
        let mut result = Vec::new();
        if !found.is_empty() || missing.is_empty() {
            result.push(Propstat {
                status: 200,
                props: found,
                condition: None,
            });
        }
        if !missing.is_empty() {
            result.push(Propstat {
                status: 404,
                props: missing,
                condition: None,
            });
        }
        Ok(result)
    }

    async fn property(
        &self,
        property: &DavPropertyName,
        path: &str,
        stats: &Stats,
        now: i64,
    ) -> Result<Option<XmlNode>, WebdavError> {
        if property.ns != DAV_NS {
            return Ok(None);
        }
        let name = property.name.as_str();
        let node = |text: String| XmlNode {
            name: name.to_owned(),
            ns: DAV_NS.to_owned(),
            text,
            children: Vec::new(),
        };
        Ok(match name {
            "creationdate" => Some(node(format_iso_date_ms(stats.birthtime_ms))),
            "displayname" => Some(node(if path == "/" {
                String::new()
            } else {
                basename(path)
            })),
            "getcontentlength" if !stats.is_directory() => Some(node(stats.size.to_string())),
            "getcontenttype" => Some(node(if stats.is_directory() {
                COLLECTION_CONTENT_TYPE.to_owned()
            } else {
                RESOURCE_CONTENT_TYPE.to_owned()
            })),
            "getetag" if !stats.is_directory() => Some(node(resource_etag(stats))),
            "getlastmodified" => Some(node(format_http_date_ms(stats.mtime_ms))),
            "resourcetype" => {
                let mut value = node(String::new());
                if stats.is_directory() {
                    value.children.push(XmlNode::dav("collection"));
                }
                Some(value)
            }
            "supportedlock" => Some(supported_lock_node()),
            "lockdiscovery" => {
                let locks = self
                    .locks
                    .lock()
                    .map(|mut locks| locks.covering(path, now))
                    .unwrap_or_default();
                Some(lock_discovery_node(&locks, now))
            }
            "quota-available-bytes" | "quota-used-bytes" => self.quota(name, path).await?,
            _ => None,
        })
    }

    async fn quota(&self, name: &str, path: &str) -> Result<Option<XmlNode>, WebdavError> {
        let Ok(statfs) = self.driver.statfs(path).await else {
            return Ok(None);
        };
        let value = if name == "quota-available-bytes" {
            statfs.blocks_available.saturating_mul(statfs.block_size)
        } else {
            statfs
                .blocks
                .saturating_sub(statfs.blocks_free)
                .saturating_mul(statfs.block_size)
        };
        Ok(Some(XmlNode {
            name: name.to_owned(),
            ns: DAV_NS.to_owned(),
            text: value.to_string(),
            children: Vec::new(),
        }))
    }

    async fn proppatch<B>(
        &self,
        path: &str,
        body: &mut B,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError>
    where
        B: WebdavRequestBody + Unpin,
    {
        let body = collect_body_stream(body, self.options.max_xml_bytes).await?;
        let request = parse_proppatch(&body, self.options.max_xml_bytes)?;
        let stats = self.stat(path).await?;
        self.require_writable(path, guard, false)?;
        let mut outcomes = Vec::new();
        let writable_times = self.driver.capabilities().times;
        for instruction in request.instructions {
            match instruction {
                ProppatchInstruction::Set(instruction) => {
                    if instruction.name != dav_property("getlastmodified") || !writable_times {
                        outcomes.push(PropertyOutcome {
                            name: instruction.name,
                            status: 403,
                            condition: Some("cannot-modify-protected-property".to_owned()),
                            set_mtime_ms: None,
                        });
                    } else if let Some(mtime) =
                        crate::protocol::parse_http_date_ms(instruction.text.trim())
                    {
                        outcomes.push(PropertyOutcome {
                            name: instruction.name,
                            status: 200,
                            condition: None,
                            set_mtime_ms: Some(mtime),
                        });
                    } else {
                        outcomes.push(PropertyOutcome {
                            name: instruction.name,
                            status: 409,
                            condition: None,
                            set_mtime_ms: None,
                        });
                    }
                }
                ProppatchInstruction::Remove(name) => outcomes.push(PropertyOutcome {
                    name,
                    status: 403,
                    condition: Some("cannot-modify-protected-property".to_owned()),
                    set_mtime_ms: None,
                }),
            }
        }
        let blocked = outcomes.iter().any(|outcome| outcome.status != 200);
        let mut mutated = false;
        if !blocked {
            for outcome in &outcomes {
                if let Some(mtime) = outcome.set_mtime_ms {
                    self.driver.utimes(path, stats.atime_ms, mtime).await?;
                    mutated = true;
                }
            }
        }
        if mutated {
            self.durability_barrier().await?;
        }
        let mut propstats: Vec<Propstat> = Vec::new();
        for outcome in outcomes {
            let status = if blocked && outcome.status == 200 {
                424
            } else {
                outcome.status
            };
            let condition = if status == 424 {
                None
            } else {
                outcome.condition
            };
            if let Some(propstat) = propstats
                .iter_mut()
                .find(|propstat| propstat.status == status && propstat.condition == condition)
            {
                propstat.props.push(property_node(&outcome.name));
            } else {
                propstats.push(Propstat {
                    status,
                    props: vec![property_node(&outcome.name)],
                    condition,
                });
            }
        }
        Ok(crate::protocol::xml_body(
            207,
            &encode_multistatus(&[MultistatusEntry {
                href: href_of(path, stats.is_directory()),
                propstat: propstats,
                status: None,
            }]),
            BTreeMap::new(),
        ))
    }

    async fn lock<B>(
        &self,
        head: &WebdavRequestHead,
        path: &str,
        body: &mut B,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError>
    where
        B: WebdavRequestBody + Unpin,
    {
        let body = collect_body_stream(body, self.options.max_xml_bytes).await?;
        let info = parse_lock_info(&body, self.options.max_xml_bytes)?;
        let timeout = parse_timeout(head.headers.get("timeout").map(String::as_str));
        let Some(info) = info else {
            return self.refresh_lock(path, timeout, guard).await;
        };
        self.require_if(guard, path).await?;
        let depth = parse_depth(
            head.headers.get("depth").map(String::as_str),
            Depth::Infinity,
        )
        .ok_or_else(|| refuse(400).with_message("LOCK is Depth: 0 or infinity"))?;
        let depth = match depth {
            Depth::Zero => LockDepth::Zero,
            Depth::Infinity => LockDepth::Infinity,
            Depth::One => {
                return Err(refuse(400)
                    .with_message("LOCK does not support Depth: 1")
                    .into());
            }
        };
        let existing = self.stat_or_absent(path).await?;
        if let Some(conflict) = self
            .locks
            .lock()
            .ok()
            .and_then(|mut locks| locks.conflict(path, depth, info.exclusive, guard.now))
        {
            return Err(self.conflicting_lock(&conflict).into());
        }
        let collection = existing.as_ref().is_some_and(Stats::is_directory);
        if existing.is_none() {
            self.require_collection(&parent(path)).await?;
            self.require_writable(path, guard, true)?;
            self.write_file(path, &[]).await?;
            self.durability_barrier().await?;
        }
        let grant = self
            .locks
            .lock()
            .map_err(|_| WebdavError::Body("lock table poisoned".to_owned()))?
            .create(
                DavLockRequest {
                    path: path.to_owned(),
                    collection,
                    depth,
                    exclusive: info.exclusive,
                    owner: info.owner,
                    timeout,
                },
                guard.now,
            );
        let lock = match grant {
            DavLockGrant::Granted(lock) => lock,
            DavLockGrant::Conflict(lock) => return Err(self.conflicting_lock(&lock).into()),
            DavLockGrant::Full => {
                return Err(refuse(503).with_message("the lock table is full").into());
            }
        };
        let mut headers = BTreeMap::new();
        headers.insert("lock-token".to_owned(), format_lock_token(&lock.token));
        Ok(crate::protocol::xml_body(
            if existing.is_some() { 200 } else { 201 },
            &encode_lock_response(&lock, guard.now),
            headers,
        ))
    }

    async fn refresh_lock(
        &self,
        path: &str,
        timeout: Option<crate::protocol::LockTimeout>,
        guard: &Guard,
    ) -> Result<WebdavResponse, WebdavError> {
        if guard.lists.is_none() {
            return Err(refuse(400)
                .with_message("a bodyless LOCK needs an If header")
                .into());
        }
        let token = guard.submitted.iter().find_map(|token| {
            self.locks
                .lock()
                .ok()
                .and_then(|mut locks| locks.find(token, guard.now))
                .filter(|lock| DavLockTable::in_scope(lock, path))
                .map(|lock| lock.token)
        });
        let Some(token) = token else {
            return Err(refuse(412)
                .with_condition("lock-token-matches-request-uri", Vec::new())
                .into());
        };
        self.require_if(guard, path).await?;
        let lock = self
            .locks
            .lock()
            .ok()
            .and_then(|mut locks| locks.refresh(&token, timeout, guard.now))
            .ok_or_else(|| {
                refuse(412).with_condition("lock-token-matches-request-uri", Vec::new())
            })?;
        Ok(crate::protocol::xml_body(
            200,
            &encode_lock_response(&lock, guard.now),
            BTreeMap::new(),
        ))
    }

    fn unlock(&self, head: &WebdavRequestHead, path: &str) -> Result<WebdavResponse, WebdavError> {
        let Some(token) = parse_lock_token(head.headers.get("lock-token").map(String::as_str))
        else {
            return Err(refuse(400)
                .with_message("UNLOCK needs a Lock-Token header")
                .into());
        };
        let now = self.now();
        let lock = self
            .locks
            .lock()
            .ok()
            .and_then(|mut locks| locks.find(&token, now));
        if lock
            .as_ref()
            .is_none_or(|lock| !DavLockTable::in_scope(lock, path))
        {
            return Err(refuse(409)
                .with_condition("lock-token-matches-request-uri", Vec::new())
                .into());
        }
        if let Ok(mut locks) = self.locks.lock() {
            locks.remove(&token);
        }
        Ok(WebdavResponse::empty(204))
    }

    fn conflicting_lock(&self, lock: &DavLock) -> DavFault {
        refuse(423).with_condition(
            "no-conflicting-lock",
            vec![href_of(&lock.path, lock.collection)],
        )
    }

    async fn guard(
        &self,
        head: &WebdavRequestHead,
        path: &str,
        evaluate: bool,
    ) -> Result<Guard, WebdavError> {
        let now = self.now();
        let lists = match head.headers.get("if") {
            None => None,
            Some(value) => Some(
                parse_if(value, head.headers.get("host").map(String::as_str)).ok_or_else(|| {
                    refuse(400).with_message("the If header is not valid RFC 4918 syntax")
                })?,
            ),
        };
        let submitted = lists
            .as_ref()
            .map_or_else(Vec::new, |lists| submitted_tokens(lists));
        let guard = Guard {
            now,
            submitted,
            lists,
        };
        if evaluate {
            self.require_if(&guard, path).await?;
        }
        Ok(guard)
    }

    async fn require_if(&self, guard: &Guard, path: &str) -> Result<(), WebdavError> {
        let Some(lists) = &guard.lists else {
            return Ok(());
        };
        if !self.evaluate_if(lists, path, guard.now).await? {
            return Err(refuse(412)
                .with_message("the If header evaluated to false")
                .into());
        }
        Ok(())
    }

    async fn evaluate_if(
        &self,
        lists: &[crate::protocol::IfList],
        path: &str,
        now: i64,
    ) -> Result<bool, WebdavError> {
        let mut states: HashMap<String, ResourceState> = HashMap::new();
        for list in lists {
            let state = if list.foreign {
                ResourceState {
                    tokens: Vec::new(),
                    etag: None,
                }
            } else {
                let resource = list.resource.as_deref().unwrap_or(path);
                if let Some(cached) = states.get(resource) {
                    cached.clone()
                } else {
                    let stats = self.stat_or_absent(resource).await?;
                    let tokens = self
                        .locks
                        .lock()
                        .map(|mut locks| {
                            locks
                                .covering(resource, now)
                                .into_iter()
                                .map(|lock| lock.token)
                                .collect()
                        })
                        .unwrap_or_default();
                    let state = ResourceState {
                        tokens,
                        etag: stats
                            .as_ref()
                            .filter(|stats| !stats.is_directory())
                            .map(resource_etag),
                    };
                    states.insert(resource.to_owned(), state.clone());
                    state
                }
            };
            if list
                .conditions
                .iter()
                .all(|condition| condition_matches(condition, &state.tokens, state.etag.as_deref()))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn require_writable(
        &self,
        path: &str,
        guard: &Guard,
        membership: bool,
    ) -> Result<(), WebdavError> {
        let mut blocking = self
            .locks
            .lock()
            .map(|mut locks| {
                locks
                    .covering(path, guard.now)
                    .into_iter()
                    .filter(|lock| !guard.submitted.contains(&lock.token))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if membership && path != "/" {
            let parent = parent(path);
            if let Ok(mut locks) = self.locks.lock() {
                for lock in locks.covering(&parent, guard.now) {
                    if !guard.submitted.contains(&lock.token)
                        && !blocking.iter().any(|existing| existing.token == lock.token)
                    {
                        blocking.push(lock);
                    }
                }
            }
        }
        if blocking.is_empty() {
            Ok(())
        } else {
            Err(refuse(423)
                .with_condition(
                    "lock-token-submitted",
                    blocking
                        .iter()
                        .map(|lock| href_of(&lock.path, lock.collection))
                        .collect(),
                )
                .into())
        }
    }

    fn locked_members(&self, path: &str, guard: &Guard) -> Vec<Failure> {
        self.locks
            .lock()
            .map(|mut locks| {
                locks
                    .within(path, guard.now)
                    .into_iter()
                    .filter(|lock| {
                        lock.path != path
                            && !guard.submitted.iter().any(|token| token == &lock.token)
                    })
                    .map(|lock| Failure {
                        path: lock.path,
                        collection: lock.collection,
                        status: 423,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn discard_unmapped(&self, path: &str) {
        let now = self.now();
        let locks = self
            .locks
            .lock()
            .map(|mut locks| locks.within(path, now))
            .unwrap_or_default();
        for lock in locks {
            if self
                .stat_or_absent(&lock.path)
                .await
                .ok()
                .flatten()
                .is_none()
                && let Ok(mut table) = self.locks.lock()
            {
                table.remove(&lock.token);
            }
        }
    }

    async fn stat(&self, path: &str) -> Result<Stats, WebdavError> {
        self.driver.stat(path).await.map_err(|error| {
            if is_absent(&error) {
                refuse(404).into()
            } else {
                error.into()
            }
        })
    }

    async fn stat_or_absent(&self, path: &str) -> Result<Option<Stats>, WebdavError> {
        match self.driver.stat(path).await {
            Ok(stats) => Ok(Some(stats)),
            Err(error) if is_absent(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn link_stat(&self, path: &str) -> Result<Stats, WebdavError> {
        match self.driver.lstat(path).await {
            Ok(stats) => Ok(stats),
            Err(error) if error.code == ErrorCode::Enosys => self.stat(path).await,
            Err(error) if is_absent(&error) => Err(refuse(404).into()),
            Err(error) => Err(error.into()),
        }
    }

    async fn require_collection(&self, path: &str) -> Result<(), WebdavError> {
        let Some(stats) = self.stat_or_absent(path).await? else {
            return Err(refuse(409)
                .with_message("the parent collection does not exist")
                .into());
        };
        if !stats.is_directory() {
            return Err(refuse(409)
                .with_message("the parent is not a collection")
                .into());
        }
        Ok(())
    }

    fn check_conditionals(
        &self,
        head: &WebdavRequestHead,
        stats: Option<&Stats>,
    ) -> Result<(), WebdavError> {
        if evaluate_conditionals(&head.headers, stats, &head.method).status == 412 {
            Err(refuse(412)
                .with_message("a conditional header did not match this resource")
                .into())
        } else {
            Ok(())
        }
    }

    fn multistatus(&self, failures: &[Failure]) -> WebdavResponse {
        let entries = failures
            .iter()
            .map(|failure| MultistatusEntry {
                href: href_of(&failure.path, failure.collection),
                propstat: Vec::new(),
                status: Some(failure.status),
            })
            .collect::<Vec<_>>();
        crate::protocol::xml_body(207, &encode_multistatus(&entries), BTreeMap::new())
    }
}

async fn next_body_chunk<B>(body: &mut B) -> Option<Result<Bytes, WebdavError>>
where
    B: WebdavRequestBody + Unpin,
{
    poll_fn(|cx| Pin::new(&mut *body).poll_next_chunk(cx)).await
}

async fn drain_body<B>(body: &mut B) -> Option<WebdavError>
where
    B: WebdavRequestBody + Unpin,
{
    let mut limit_error = None;
    while let Some(chunk) = next_body_chunk(body).await {
        match chunk {
            Ok(_) => {}
            Err(error) if is_recoverable_body_drain_error(&error) => {
                limit_error.get_or_insert(error);
            }
            Err(error) => return Some(error),
        }
    }
    limit_error
}

fn is_recoverable_body_drain_error(error: &WebdavError) -> bool {
    matches!(error, WebdavError::Fault(fault) if fault.status == 413)
}

async fn collect_body_stream<B>(body: &mut B, limit: usize) -> Result<Vec<u8>, WebdavError>
where
    B: WebdavRequestBody + Unpin,
{
    let mut result = Vec::new();
    while let Some(chunk) = next_body_chunk(body).await {
        let chunk = chunk?;
        if result.len().saturating_add(chunk.len()) > limit {
            return Err(refuse(413)
                .with_message(format!("the request body exceeds its {limit}-byte budget"))
                .into());
        }
        result.extend_from_slice(&chunk);
    }
    Ok(result)
}

fn property_node(name: &DavPropertyName) -> XmlNode {
    XmlNode {
        name: name.name.clone(),
        ns: name.ns.clone(),
        text: String::new(),
        children: Vec::new(),
    }
}

fn is_absent(error: &FsError) -> bool {
    crate::constants::is_absent(error.code)
}

fn parent(path: &str) -> String {
    mount_rs_core::path::dirname(path)
}

fn basename(path: &str) -> String {
    mount_rs_core::path::basename(path)
}

fn join(path: &str, name: &str) -> String {
    mount_rs_core::path::normalize_path(&format!("{path}/{name}"))
}

fn now_ms() -> i64 {
    mount_rs_core::types::now_ms()
}

fn propfind_directory_error(error: FsError) -> WebdavError {
    if error.code == ErrorCode::Eoverflow {
        refuse(413)
            .with_header("connection", "close")
            .with_message("the PROPFIND directory listing exceeds its entry budget")
            .into()
    } else {
        error.into()
    }
}

fn refuse(status: u16) -> DavFault {
    crate::protocol::refuse(status)
}
