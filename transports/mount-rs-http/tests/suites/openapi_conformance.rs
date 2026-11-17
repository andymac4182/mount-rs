//! Proves that the server and `openapi.yaml` describe the same API.
//!
//! Three things are checked, all against a real server on a real socket:
//!
//! 1. **Routes.** Every path and method the document names is served, and
//!    nothing the server serves is missing from the document. The server's
//!    side is found by sending requests, not read from a list.
//! 2. **Responses.** For every operation, each status the document declares
//!    is produced by some request here, and each response the server gives
//!    to a set of ordinary and failing requests is declared: its status,
//!    its `Content-Type`, its JSON body against the document's schema, and
//!    its headers.
//! 3. **The document itself.** Its examples match its own schemas, and the
//!    request bodies used here match its request schemas.
//!
//! A status the document declares and nothing here can produce fails the
//! test unless it is listed in `NOT_REPRODUCED`, with the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::BodyExt;
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, MkdirOptions, Result, Stats,
    StatsFs, StreamedFileWrite,
};
use mount_rs_http::{DriveConfig, DriveRegistry, HttpServer, HttpServerOptions};
use mount_rs_memfs::MemoryFs;
use serde_json::{Value, json};

use crate::openapi::{
    METHODS, Operation, Reply, check_response, examples, operations, report, template_of, validate,
    vocabulary,
};
use crate::serving::Served;

const TOKEN: &str = "conformance-secret";
const DOCUMENT: &str = include_str!("../../openapi.yaml");
/// What the server says for a path that is no route.
const NO_ROUTE: &str = "drive or route not found";

/// Declared responses no request in this file produces, and why. An entry
/// that is no longer needed fails the test, so the list cannot go stale.
const NOT_REPRODUCED: [(&str, u16, &str); 2] = [
    (
        "listDrives",
        504,
        "answered from memory; the request timeout cannot be made to expire first",
    ),
    (
        "headDrives",
        504,
        "answered from memory; the request timeout cannot be made to expire first",
    ),
];

// ------------------------------------------------------------- the document

fn document() -> Value {
    crate::openapi::parse(DOCUMENT)
}

// --------------------------------------------------------------- the drives

/// What a broken drive does with every operation.
#[derive(Clone, Copy)]
enum Fault {
    Fail(ErrorCode),
    /// Never answers, so the request runs into the server's timeout.
    Stall,
}

struct BrokenFs(Fault);

impl BrokenFs {
    async fn hit<T>(&self) -> Result<T> {
        match self.0 {
            Fault::Fail(code) => Err(FsError::new(code)),
            Fault::Stall => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Err(FsError::new(ErrorCode::Eio))
            }
        }
    }
}

#[async_trait]
impl FsDriver for BrokenFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn begin_file_write(&self, _path: &str) -> Result<Option<Box<dyn StreamedFileWrite>>> {
        self.hit().await
    }
    async fn syncfs(&self) -> Result<()> {
        self.hit().await
    }
    async fn stat(&self, _path: &str) -> Result<Stats> {
        self.hit().await
    }
    async fn content_tag(&self, _path: &str) -> Result<Option<String>> {
        self.hit().await
    }
    async fn lstat(&self, _path: &str) -> Result<Stats> {
        self.hit().await
    }
    async fn statfs(&self, _path: &str) -> Result<StatsFs> {
        self.hit().await
    }
    async fn readdir(&self, _path: &str) -> Result<Vec<DirEntry>> {
        self.hit().await
    }
    async fn readdir_bounded(&self, _path: &str, _max_entries: usize) -> Result<Vec<DirEntry>> {
        self.hit().await
    }
    async fn open(&self, _path: &str, _flags: &str, _mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.hit().await
    }
    async fn write_file(&self, _path: &str, _data: &[u8]) -> Result<()> {
        self.hit().await
    }
    async fn mkdir(&self, _path: &str, _options: MkdirOptions) -> Result<Option<String>> {
        self.hit().await
    }
    async fn rmdir(&self, _path: &str) -> Result<()> {
        self.hit().await
    }
    async fn unlink(&self, _path: &str) -> Result<()> {
        self.hit().await
    }
    async fn rename(&self, _old_path: &str, _new_path: &str) -> Result<()> {
        self.hit().await
    }
    async fn readlink(&self, _path: &str) -> Result<String> {
        self.hit().await
    }
    async fn truncate(&self, _path: &str, _length: u64) -> Result<()> {
        self.hit().await
    }
}

/// A healthy drive in every respect except what it reports to `/readyz`.
struct UnhealthyFs(MemoryFs);

#[async_trait]
impl FsDriver for UnhealthyFs {
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn health(&self) -> Result<()> {
        Err(FsError::new(ErrorCode::Estale))
    }
    async fn stat(&self, path: &str) -> Result<Stats> {
        self.0.stat(path).await
    }
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.0.readdir(path).await
    }
    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.0.open(path, flags, mode).await
    }
}

/// The error a drive is made to fail with to produce a status, where the
/// status has no request of its own below.
fn code_for(status: u16) -> Option<ErrorCode> {
    Some(match status {
        400 => ErrorCode::Einval,
        403 => ErrorCode::Eacces,
        404 => ErrorCode::Enoent,
        409 => ErrorCode::Eexist,
        414 => ErrorCode::Enametoolong,
        500 => ErrorCode::Eio,
        501 => ErrorCode::Enotsup,
        503 => ErrorCode::Estale,
        507 => ErrorCode::Enospc,
        _ => return None,
    })
}

/// Failures any operation on any drive can meet, whatever the document
/// says the operation is for.
const ANYWHERE: [ErrorCode; 5] = [
    ErrorCode::Eacces,
    ErrorCode::Eio,
    ErrorCode::Enotsup,
    ErrorCode::Estale,
    ErrorCode::Eagain,
];

fn failing_drive(code: ErrorCode) -> String {
    format!("fail-{}", code.as_str())
}

async fn serve(drives: Vec<(String, Arc<dyn FsDriver>)>, options: HttpServerOptions) -> Served {
    let mut registry = DriveRegistry::new();
    for (id, driver) in drives {
        registry
            .register(DriveConfig::new(id, driver, TOKEN).expect("drive config"))
            .expect("drive registration");
    }
    crate::serving::serve(registry, options, crate::BEHIND_HANDLER).await
}

/// The content the requests below are written against.
async fn populated() -> Arc<MemoryFs> {
    let fs = Arc::new(MemoryFs::empty());
    let recursive = MkdirOptions {
        recursive: true,
        mode: None,
    };
    for directory in ["/dir", "/full", "/empty", "/gone", "/wide", "/moved"] {
        fs.mkdir(directory, recursive).await.expect("mkdir");
    }
    for file in [
        "/dir/a.txt",
        "/dir/b.txt",
        "/full/child",
        "/gone/victim",
        "/moved/from",
    ] {
        fs.write_file(file, b"hello world").await.expect("write");
    }
    for index in 0..64 {
        fs.write_file(&format!("/wide/entry-with-a-long-name-{index:04}"), b"")
            .await
            .expect("write");
    }
    fs.symlink("dir/a.txt", "/link").await.expect("symlink");
    fs
}

// ------------------------------------------------------------- the requests

#[derive(Clone, Default)]
struct Call {
    drive: String,
    /// What `{path}` stands for.
    path: String,
    query: String,
    headers: Vec<(&'static str, String)>,
    body: Option<Vec<u8>>,
    anonymous: bool,
}

impl Call {
    fn on(drive: &str) -> Self {
        Self {
            drive: drive.to_owned(),
            path: "dir/a.txt".to_owned(),
            ..Self::default()
        }
    }
    fn path(mut self, path: &str) -> Self {
        self.path = path.to_owned();
        self
    }
    fn query(mut self, query: &str) -> Self {
        self.query = query.to_owned();
        self
    }
    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }
    fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }
    fn json(self, body: Value) -> Self {
        self.header("content-type", "application/json")
            .body(body.to_string())
    }
    fn anonymous(mut self) -> Self {
        self.anonymous = true;
        self
    }
}

/// A request for `operation` that is well formed, so that what a broken
/// drive, a missing token or a closing server answers is about them.
fn ordinary(operation: &Operation, drive: &str) -> Call {
    let call = Call::on(drive);
    match operation.id.as_str() {
        "mkdir" => call.json(json!({"path": "/made/here", "recursive": true, "mode": 493})),
        "rename" => call.json(json!({"from": "/dir/a.txt", "to": "/dir/c.txt"})),
        "truncate" => call.json(json!({"path": "/dir/a.txt", "length": 4})),
        _ if operation.method == "PUT" => call.body("conformance"),
        _ => call,
    }
}

async fn send(
    client: &reqwest::Client,
    server: &HttpServer,
    method: &str,
    path: &str,
    call: &Call,
) -> Reply {
    let method = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
    let mut request = client.request(method, format!("{}{path}{}", server.url(), call.query));
    if !call.anonymous {
        request = request.header("authorization", format!("Bearer {TOKEN}"));
    }
    for (name, value) in &call.headers {
        request = request.header(*name, value);
    }
    if let Some(body) = &call.body {
        request = request.body(body.clone());
    }
    let response = request.send().await.expect("response");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.bytes().await.expect("body");
    Reply::collect(status, &headers, &body)
}

/// The same request given to a server that has been closed, which is how a
/// request in progress when a shutdown begins is answered.
async fn send_closed(server: &HttpServer, method: &str, path: &str, call: &Call) -> Reply {
    let body = http_body_util::Full::new(Bytes::from(call.body.clone().unwrap_or_default()))
        .map_err(|never| -> mount_rs_http::BoxError { match never {} })
        .boxed_unsync();
    let mut request = hyper::Request::builder()
        .method(method)
        .uri(format!("{path}{}", call.query))
        .header("authorization", format!("Bearer {TOKEN}"));
    for (name, value) in &call.headers {
        request = request.header(*name, value);
    }
    let response = server.handle(request.body(body).expect("request")).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    // On a connection it is the HTTP layer that leaves out the body of a
    // HEAD response; there is none here.
    let body = if method == "HEAD" { Bytes::new() } else { body };
    Reply::collect(status, &headers, &body)
}

fn request_path(operation: &Operation, call: &Call) -> String {
    operation
        .template
        .replace("{id}", &call.drive)
        .replace("{path}", &call.path)
}

// --------------------------------------------------------------- the checks

struct Suite {
    doc: Value,
    operations: Vec<Operation>,
    client: reqwest::Client,
    /// `(operationId, status)` pairs a request here has produced.
    produced: BTreeSet<(String, u16)>,
    failures: Vec<String>,
}

impl Suite {
    fn new() -> Self {
        let doc = document();
        Self {
            operations: operations(&doc),
            doc,
            client: reqwest::Client::new(),
            produced: BTreeSet::new(),
            failures: Vec::new(),
        }
    }

    fn operation(&self, id: &str) -> Operation {
        self.operations
            .iter()
            .find(|operation| operation.id == id)
            .unwrap_or_else(|| panic!("the document has no operation `{id}`"))
            .clone()
    }

    /// Check one reply against what the document says `operation` answers.
    fn check(&mut self, operation: &Operation, reply: &Reply, what: &str) {
        let label = format!(
            "{} ({} {}) {what}: {}",
            operation.id, operation.method, operation.template, reply.status
        );
        let Some(declared) = operation.spec["responses"].get(reply.status.to_string()) else {
            self.failures.push(format!(
                "{label}: the document does not declare this status"
            ));
            return;
        };
        self.produced.insert((operation.id.clone(), reply.status));
        let failures = check_response(&self.doc, declared, &operation.method, reply);
        self.failures.extend(
            failures
                .into_iter()
                .map(|failure| format!("{label}: {failure}")),
        );
    }

    /// Send `call` for the operation, check the reply, and require
    /// `status` when the request is meant to produce one in particular.
    async fn expect(
        &mut self,
        server: &HttpServer,
        id: &str,
        call: Call,
        status: u16,
        what: &str,
    ) -> Reply {
        let operation = self.operation(id);
        let path = request_path(&operation, &call);
        let reply = send(&self.client, server, &operation.method, &path, &call).await;
        if reply.status != status {
            self.failures.push(format!(
                "{id} {what}: expected {status}, the server answered {} {}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            ));
        }
        self.check(&operation, &reply, what);
        reply
    }

    fn require_header(&mut self, reply: &Reply, name: &str, value: Option<&str>, what: &str) {
        match (reply.header(name), value) {
            (None, _) => self.failures.push(format!("{what}: no `{name}` header")),
            (Some(actual), Some(value)) if actual != value => self
                .failures
                .push(format!("{what}: `{name}` is {actual:?}, not {value:?}")),
            _ => {}
        }
    }
}

// ------------------------------------------------------------------- routes

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_document_and_the_server_have_the_same_routes_and_methods() {
    let doc = document();
    let server = serve(
        vec![("main".to_owned(), populated().await as Arc<dyn FsDriver>)],
        HttpServerOptions::default(),
    )
    .await;
    let client = reqwest::Client::new();

    let mut paths = BTreeSet::from(["/".to_owned(), "/v1/drives/main".to_owned()]);
    for template in doc["paths"].as_object().expect("paths").keys() {
        paths.insert(
            template
                .replace("{id}", "main")
                .replace("{path}", "dir/a.txt"),
        );
    }
    for word in vocabulary(&doc) {
        for shape in [
            "/{}",
            "/{}/x",
            "/v1/{}",
            "/v1/drives/main/{}",
            "/v1/drives/main/{}/x",
            "/v1/drives/main/{}/x/y",
            "/v1/drives/main/ops/{}",
            "/v1/drives/main/ops/{}/x",
        ] {
            paths.insert(shape.replace("{}", &word));
        }
    }

    let mut failures = Vec::new();
    let mut served = BTreeSet::new();
    for path in &paths {
        let template = template_of(&doc, path);
        let documented: BTreeSet<&str> = METHODS
            .into_iter()
            .filter(|method| {
                template.is_some_and(|template| {
                    doc["paths"][template]
                        .get(method.to_ascii_lowercase())
                        .is_some()
                })
            })
            .collect();
        let mut get_found_no_route = false;
        for method in METHODS {
            let call = Call::on("main").body("{}");
            let reply = send(&client, &server, method, path, &call).await;
            let what = format!("{method} {path}");
            // A HEAD response has no body to tell a missing route from a
            // missing file; the GET before it has.
            let no_route = reply.status == 404
                && if method == "HEAD" {
                    get_found_no_route
                } else {
                    reply.message().as_deref() == Some(NO_ROUTE)
                };
            get_found_no_route |= method == "GET" && no_route;
            if no_route {
                if let Some(template) = template {
                    failures.push(format!(
                        "{what}: documented as {template}, but the server has no such route"
                    ));
                }
                continue;
            }
            let Some(template) = template else {
                failures.push(format!(
                    "{what}: the server answers {}, but the document has no such path",
                    reply.status
                ));
                continue;
            };
            if reply.status != 405 {
                served.insert((template, method));
                if !documented.contains(method) {
                    failures.push(format!(
                        "{what}: the server answers {}, but {template} does not document {method}",
                        reply.status
                    ));
                }
                continue;
            }
            if documented.contains(method) {
                failures.push(format!("{what}: documented, but the server answers 405"));
            }
            // A 405 names the methods the route has: exactly the documented ones.
            let allowed: BTreeSet<&str> = reply
                .header("allow")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .collect();
            if allowed != documented {
                failures.push(format!(
                    "{what}: Allow is {allowed:?}, the document has {documented:?}"
                ));
            }
            let declared = METHODS.into_iter().find_map(|method| {
                doc["paths"][template][method.to_ascii_lowercase()]["responses"].get("405")
            });
            match declared {
                None => failures.push(format!("{what}: no operation of {template} declares 405")),
                Some(declared) => failures.extend(
                    check_response(&doc, declared, method, &reply)
                        .into_iter()
                        .map(|failure| format!("{what}: 405: {failure}")),
                ),
            }
        }
    }
    for operation in operations(&doc) {
        if !served.contains(&(operation.template.as_str(), operation.method.as_str())) {
            failures.push(format!(
                "{} {}: documented, but no request reached it",
                operation.method, operation.template
            ));
        }
    }
    server.close().await.expect("close");
    report(&failures);
}

// ---------------------------------------------------------------- responses

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_declared_response_is_produced_and_every_response_is_declared() {
    let mut suite = Suite::new();
    let fs = populated().await;

    let mut drives: Vec<(String, Arc<dyn FsDriver>)> = vec![("main".to_owned(), fs.clone())];
    let mut codes: BTreeMap<&str, ErrorCode> = BTreeMap::new();
    for code in ANYWHERE.into_iter().chain((400..600).filter_map(code_for)) {
        codes.insert(code.as_str(), code);
    }
    for code in codes.values() {
        drives.push((failing_drive(*code), Arc::new(BrokenFs(Fault::Fail(*code)))));
    }
    let server = serve(drives, HttpServerOptions::default()).await;
    let slow = serve(
        vec![(
            "stall".to_owned(),
            Arc::new(BrokenFs(Fault::Stall)) as Arc<dyn FsDriver>,
        )],
        HttpServerOptions {
            request_timeout: Duration::from_millis(200),
            ..HttpServerOptions::default()
        },
    )
    .await;
    let small = serve(
        vec![("main".to_owned(), populated().await as Arc<dyn FsDriver>)],
        HttpServerOptions {
            max_request_bytes: 1024,
            max_upload_bytes: 4096,
            max_directory_entries: 3,
            ..HttpServerOptions::default()
        },
    )
    .await;
    let unready = serve(
        vec![(
            "main".to_owned(),
            Arc::new(UnhealthyFs(MemoryFs::empty())) as Arc<dyn FsDriver>,
        )],
        HttpServerOptions::default(),
    )
    .await;
    let closed = {
        let mut registry = DriveRegistry::new();
        registry
            .register(DriveConfig::new("main", fs.clone(), TOKEN).expect("drive config"))
            .expect("drive registration");
        let server = HttpServer::unbound(registry, HttpServerOptions::default());
        server.close().await.expect("close");
        server
    };

    let main = || Call::on("main");
    let s = &server;

    // -- probes and discovery
    for id in ["getHealth", "headHealth", "getReadiness", "headReadiness"] {
        let reply = suite.expect(s, id, main(), 200, "probe").await;
        suite.require_header(&reply, "cache-control", Some("no-store"), id);
        suite.require_header(&reply, "x-content-type-options", Some("nosniff"), id);
    }
    for id in ["getReadiness", "headReadiness"] {
        suite
            .expect(&unready, id, main(), 503, "a failed drive")
            .await;
    }
    for id in ["listDrives", "headDrives"] {
        suite.expect(s, id, main(), 200, "discovery").await;
        let reply = suite
            .expect(s, id, main().anonymous(), 401, "no token")
            .await;
        suite.require_header(&reply, "www-authenticate", Some("Bearer"), id);
    }

    // -- reading
    for id in ["getRoot", "headRoot"] {
        suite.expect(s, id, main(), 200, "the root").await;
        suite
            .expect(&small, id, main(), 413, "too many entries")
            .await;
    }
    for id in ["getPath", "headPath"] {
        let reply = suite.expect(s, id, main(), 200, "a file").await;
        suite.require_header(&reply, "etag", None, id);
        suite.require_header(&reply, "accept-ranges", Some("bytes"), id);
        suite.require_header(&reply, "content-length", Some("11"), id);
        let etag = reply.header("etag").unwrap_or_default().to_owned();
        suite
            .expect(s, id, main().path("dir"), 200, "a directory")
            .await;
        suite
            .expect(s, id, main().path("link"), 200, "a symbolic link")
            .await;
        let reply = suite
            .expect(s, id, main().header("range", "bytes=0-4"), 206, "a range")
            .await;
        suite.require_header(&reply, "content-range", Some("bytes 0-4/11"), id);
        suite.require_header(&reply, "etag", Some(&etag), id);
        let reply = suite
            .expect(
                s,
                id,
                main().header("if-none-match", &etag),
                304,
                "a current tag",
            )
            .await;
        suite.require_header(&reply, "etag", Some(&etag), id);
        suite
            .expect(
                s,
                id,
                main().header("if-match", "\"other\""),
                412,
                "a stale tag",
            )
            .await;
        let reply = suite
            .expect(
                s,
                id,
                main().header("range", "bytes=99-"),
                416,
                "a range past the end",
            )
            .await;
        suite.require_header(&reply, "content-range", Some("bytes */11"), id);
        suite
            .expect(s, id, main().path("a%2Fb"), 400, "an encoded slash")
            .await;
        suite
            .expect(s, id, main().path("dir/none"), 404, "a missing file")
            .await;
        suite
            .expect(&small, id, main().path("wide"), 413, "too many entries")
            .await;
    }

    // -- listing
    for (id, directory) in [
        ("listRootEntries", ""),
        ("headRootEntries", ""),
        ("listEntries", "wide"),
        ("headEntries", "wide"),
    ] {
        let at = || main().path(directory);
        let reply = suite.expect(s, id, at(), 200, "a listing").await;
        suite.require_header(&reply, "vary", Some("Accept-Encoding"), id);
        suite
            .expect(s, id, at().query("?limit=2"), 200, "a page")
            .await;
        suite
            .expect(s, id, at().query("?limit=1000"), 200, "the last page")
            .await;
        suite
            .expect(s, id, at().query("?limit=0"), 400, "a bad limit")
            .await;
        let refused = at().header("accept-encoding", "gzip;q=0, identity;q=0");
        let reply = suite
            .expect(s, id, refused, 406, "no acceptable coding")
            .await;
        suite.require_header(&reply, "vary", Some("Accept-Encoding"), id);
        suite
            .expect(&small, id, at(), 413, "too many entries")
            .await;
    }
    for id in ["listEntries", "headEntries"] {
        let gzip = main().path("wide").header("accept-encoding", "gzip");
        let reply = suite.expect(s, id, gzip, 200, "a compressed listing").await;
        suite.require_header(&reply, "content-encoding", Some("gzip"), id);
        suite
            .expect(s, id, main().path("a%2Fb"), 400, "an encoded slash")
            .await;
        suite
            .expect(s, id, main().path("none"), 404, "a missing directory")
            .await;
        suite.expect(s, id, main(), 404, "a file").await;
    }

    // -- metadata
    for kind in ["stat", "lstat", "statfs", "readlink"] {
        let ids = [
            format!("{kind}Root"),
            format!("head{}{}Root", kind[..1].to_uppercase(), &kind[1..]),
            format!("{kind}Path"),
            format!("head{}{}Path", kind[..1].to_uppercase(), &kind[1..]),
        ];
        let link = kind == "readlink";
        for id in &ids[..2] {
            let status = if link { 400 } else { 200 };
            suite.expect(s, id, main(), status, "the root").await;
        }
        for id in &ids[2..] {
            let target = if link { "link" } else { "dir/a.txt" };
            suite
                .expect(s, id, main().path(target), 200, "a path")
                .await;
            suite
                .expect(s, id, main().path("a%2Fb"), 400, "an encoded slash")
                .await;
            if link {
                suite
                    .expect(s, id, main(), 400, "not a symbolic link")
                    .await;
            }
            if kind != "statfs" {
                suite
                    .expect(s, id, main().path("none"), 404, "a missing path")
                    .await;
            }
        }
    }

    // -- writing and deleting
    let reply = suite
        .expect(
            s,
            "putFile",
            main().path("dir/new.txt").body("new"),
            200,
            "a new file",
        )
        .await;
    suite.require_header(&reply, "etag", None, "putFile");
    suite
        .expect(
            s,
            "putFile",
            main().path("none/x").body("x"),
            404,
            "a missing parent",
        )
        .await;
    suite
        .expect(
            s,
            "putFile",
            main().path("dir").body("x"),
            409,
            "a directory",
        )
        .await;
    let create_only = main().header("if-none-match", "*").body("x");
    suite
        .expect(s, "putFile", create_only, 412, "create-only on a file")
        .await;
    suite
        .expect(
            s,
            "putFile",
            main().path("a%2Fb").body("x"),
            400,
            "an encoded slash",
        )
        .await;
    let large = main().path("dir/large").body(vec![b'x'; 5000]);
    let reply = suite
        .expect(&small, "putFile", large, 413, "an oversized body")
        .await;
    suite.require_header(&reply, "connection", Some("close"), "putFile 413");
    suite
        .expect(s, "putRoot", main().body("x"), 409, "the root")
        .await;
    let large = main().body(vec![b'x'; 5000]);
    suite
        .expect(&small, "putRoot", large, 413, "an oversized body")
        .await;

    suite.expect(s, "deleteRoot", main(), 400, "the root").await;
    suite
        .expect(s, "deletePath", main().path("gone/victim"), 204, "a file")
        .await;
    suite
        .expect(
            s,
            "deletePath",
            main().path("gone"),
            204,
            "an empty directory",
        )
        .await;
    suite
        .expect(s, "deletePath", main().path("gone"), 404, "already gone")
        .await;
    suite
        .expect(
            s,
            "deletePath",
            main().path("full"),
            409,
            "a full directory",
        )
        .await;
    let stale = main().header("if-match", "\"other\"");
    suite
        .expect(s, "deletePath", stale, 412, "a stale tag")
        .await;
    suite
        .expect(
            s,
            "deletePath",
            main().path("a%2Fb"),
            400,
            "an encoded slash",
        )
        .await;

    // -- operations
    let mkdir = |body: Value| main().json(body);
    let made = json!({"path": "/made/deep", "recursive": true});
    suite
        .expect(s, "mkdir", mkdir(made), 200, "with parents")
        .await;
    suite
        .expect(s, "mkdir", mkdir(json!({"path": "/made/one"})), 200, "one")
        .await;
    suite
        .expect(
            s,
            "mkdir",
            mkdir(json!({"path": "/made/one"})),
            409,
            "again",
        )
        .await;
    suite
        .expect(
            s,
            "mkdir",
            mkdir(json!({"path": "/none/x"})),
            404,
            "no parent",
        )
        .await;
    suite
        .expect(
            s,
            "mkdir",
            mkdir(json!({"path": "relative"})),
            400,
            "a relative path",
        )
        .await;
    suite
        .expect(s, "mkdir", main().body("{"), 400, "not JSON")
        .await;
    suite
        .expect(s, "mkdir", mkdir(json!({})), 400, "no path")
        .await;
    let renamed = json!({"from": "/moved/from", "to": "/moved/to"});
    suite
        .expect(s, "rename", mkdir(renamed.clone()), 200, "a file")
        .await;
    suite
        .expect(s, "rename", mkdir(renamed), 404, "again")
        .await;
    let onto = json!({"from": "/empty", "to": "/full"});
    suite
        .expect(s, "rename", mkdir(onto), 409, "onto a full directory")
        .await;
    suite
        .expect(
            s,
            "rename",
            mkdir(json!({"from": "/dir"})),
            400,
            "no target",
        )
        .await;
    let cut = json!({"path": "/dir/b.txt", "length": 2});
    suite.expect(s, "truncate", mkdir(cut), 200, "a file").await;
    let missing = json!({"path": "/none", "length": 2});
    suite
        .expect(s, "truncate", mkdir(missing), 404, "a missing file")
        .await;
    let directory = json!({"path": "/dir", "length": 2});
    suite
        .expect(s, "truncate", mkdir(directory), 409, "a directory")
        .await;
    let negative = json!({"path": "/dir/b.txt", "length": -1});
    suite
        .expect(s, "truncate", mkdir(negative), 400, "a negative length")
        .await;
    suite.expect(s, "sync", main(), 204, "a flush").await;
    suite
        .expect(s, "sync", main().body("x"), 400, "a body")
        .await;
    for id in ["mkdir", "rename", "truncate", "sync"] {
        let large = main().body(vec![b' '; 2000]);
        let reply = suite
            .expect(&small, id, large, 413, "an oversized body")
            .await;
        suite.require_header(&reply, "connection", Some("close"), id);
    }

    // -- what every operation answers to trouble that is not its own
    for operation in suite.operations.clone() {
        let id = operation.id.as_str();
        let reply = {
            let call = ordinary(&operation, "main");
            let path = request_path(&operation, &call);
            send_closed(&closed, &operation.method, &path, &call).await
        };
        if reply.status != 503 {
            suite
                .failures
                .push(format!("{id}: a closed server answered {}", reply.status));
        }
        suite.check(&operation, &reply, "while shutting down");
        suite.require_header(&reply, "connection", Some("close"), id);

        if !operation.on_a_drive() {
            continue;
        }
        for (drive, what) in [("main", "no token"), ("absent", "an unknown drive")] {
            let call = ordinary(&operation, drive);
            let call = if drive == "main" {
                call.anonymous()
            } else {
                call
            };
            let reply = suite.expect(s, id, call, 401, what).await;
            suite.require_header(&reply, "www-authenticate", Some("Bearer"), id);
        }
        for code in ANYWHERE {
            let call = ordinary(&operation, &failing_drive(code));
            let path = request_path(&operation, &call);
            let reply = send(&suite.client, s, &operation.method, &path, &call).await;
            suite.check(
                &operation,
                &reply,
                &format!("on a drive failing with {}", code.as_str()),
            );
            if code == ErrorCode::Eagain && reply.status == 503 {
                suite.require_header(&reply, "retry-after", Some("1"), id);
            }
        }
        let call = ordinary(&operation, "stall");
        let path = request_path(&operation, &call);
        let reply = send(&suite.client, &slow, &operation.method, &path, &call).await;
        suite.check(&operation, &reply, "on a drive that does not answer");
        if reply.status == 504 {
            suite.require_header(&reply, "connection", Some("close"), id);
        }
    }

    // -- every other declared status, from a drive failing with its error
    for operation in suite.operations.clone() {
        for status in operation.declared() {
            if suite.produced.contains(&(operation.id.clone(), status)) {
                continue;
            }
            let Some(code) = code_for(status).filter(|_| operation.on_a_drive()) else {
                continue;
            };
            let what = format!("on a drive failing with {}", code.as_str());
            let call = ordinary(&operation, &failing_drive(code));
            suite.expect(s, &operation.id, call, status, &what).await;
        }
    }

    // -- the 405 of each path is produced by the route test; check it is
    //    declared once per path that has one, and count it here.
    for operation in suite.operations.clone() {
        if operation.declared().contains(&405) {
            let call = ordinary(&operation, "main");
            let path = request_path(&operation, &call);
            let reply = send(&suite.client, s, "PATCH", &path, &call).await;
            if reply.status != 405 {
                suite
                    .failures
                    .push(format!("{}: PATCH answered {}", operation.id, reply.status));
            }
            suite.produced.insert((operation.id.clone(), 405));
        }
    }

    let mut exempt: BTreeSet<(String, u16)> = NOT_REPRODUCED
        .into_iter()
        .map(|(id, status, _)| (id.to_owned(), status))
        .collect();
    for operation in &suite.operations {
        for status in operation.declared() {
            let pair = (operation.id.clone(), status);
            if !suite.produced.contains(&pair) && !exempt.remove(&pair) {
                suite.failures.push(format!(
                    "{} declares {status}, which no request here produces",
                    operation.id
                ));
            }
        }
    }
    for (id, status) in exempt {
        suite.failures.push(format!(
            "NOT_REPRODUCED lists {id} {status}, which is produced or no longer declared"
        ));
    }

    for server in [server, slow, small, unready] {
        server.close().await.expect("close");
    }
    report(&suite.failures);
}

// ------------------------------------------------------ the client sequence

/// The sequence of requests that is also sent to real `serve-http`
/// processes, with and without cluster features, by the tests of
/// `mount-rs-cli`. Here it establishes what the answers are.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_sequence_gets_the_documented_answers() {
    use crate::blackbox::{self, Target, UnknownDrive};
    let server = serve(
        vec![(
            "main".to_owned(),
            Arc::new(MemoryFs::empty()) as Arc<dyn FsDriver>,
        )],
        HttpServerOptions {
            max_request_bytes: blackbox::MAX_REQUEST_BYTES,
            max_upload_bytes: blackbox::MAX_UPLOAD_BYTES,
            max_directory_entries: blackbox::MAX_DIRECTORY_ENTRIES,
            ..HttpServerOptions::default()
        },
    )
    .await;
    let target = Target {
        base: server.url(),
        drive: "main".to_owned(),
        token: TOKEN.to_owned(),
        holds_drive: true,
        unknown_drive: UnknownDrive::Unauthorized,
    };
    let outcome = blackbox::run(&target, &document(), None).await;
    assert!(outcome.exchanges.len() > 400, "the sequence was sent");
    server.close().await.expect("close");
    report(&outcome.failures);
}

// ------------------------------------------------------------- the document

#[test]
fn the_examples_in_the_document_match_its_schemas() {
    let doc = document();
    let mut found = Vec::new();
    examples(&doc, String::new(), &mut found);
    assert!(found.len() > 20, "the document's examples were not found");
    let failures: Vec<String> = found
        .into_iter()
        .filter_map(|(at, schema, example)| validate(&doc, schema, example, &at).err())
        .collect();
    report(&failures);
}

#[test]
fn the_request_bodies_sent_here_match_the_documented_request_schemas() {
    let doc = document();
    let mut checked = 0;
    for operation in operations(&doc) {
        let Some(schema) = operation.spec["requestBody"]["content"]
            .get("application/json")
            .map(|media| &media["schema"])
        else {
            continue;
        };
        let body = ordinary(&operation, "main").body.expect("a JSON body");
        let body: Value = serde_json::from_slice(&body).expect("JSON");
        validate(&doc, schema, &body, &operation.id).expect("the request matches its schema");
        checked += 1;
    }
    assert_eq!(checked, 3, "mkdir, rename and truncate take JSON");
}

#[test]
fn the_validator_rejects_what_a_schema_forbids() {
    let doc = document();
    let schema = |name: &str| doc["components"]["schemas"][name].clone();
    let rejects = |name: &str, value: Value| {
        assert!(
            validate(&doc, &schema(name), &value, name).is_err(),
            "{name} accepted {value}"
        );
    };
    let error = json!({"error": {"code": "ENOENT", "message": "gone"}});
    validate(&doc, &schema("Error"), &error, "Error").expect("a well-formed error");
    rejects("Error", json!({"error": {"code": "ENOENT"}}));
    rejects(
        "Error",
        json!({"error": {"code": "EWHATEVER", "message": "x"}}),
    );
    rejects("Error", json!({"error": "ENOENT"}));
    rejects("Health", json!({"status": "fine"}));
    rejects("Ready", json!({"status": "ready", "drives": 0}));
    rejects("DriveSummary", json!({"id": 7}));
    rejects("WriteResult", json!({"path": "/a", "bytes": -1}));
    rejects("WriteResult", json!({"path": "/a", "bytes": 1.5}));
    rejects("EntryPage", json!({"next_cursor": "v1.61"}));
    rejects(
        "DirEntry",
        json!({"name": "a", "parent_path": "/", "file_type": "Pipe"}),
    );
    rejects("MkdirResult", json!({"path": "/a", "created": 3}));
    rejects("JsonPath", json!("relative"));
    assert!(
        validate(&doc, &json!({"not": {}}), &json!(1), "x")
            .unwrap_err()
            .contains("does not implement"),
        "an unknown keyword is an error, not a pass"
    );
}
