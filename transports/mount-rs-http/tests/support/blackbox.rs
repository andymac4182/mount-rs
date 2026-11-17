//! The v1 contract as a client sees it: one fixed sequence of requests,
//! with the status each must get, sent to a server known only by its URL.
//!
//! The sequence needs nothing but an empty, writable drive, so it can be
//! given to any server: this crate's listener, a `serve-http` process, one
//! with `remote_sessions`, the owner of a drive in a forwarding cluster, or
//! an ingress that forwards to the owner. Every reply is checked against
//! `openapi.yaml`, and the replies are returned in a form that two runs can
//! be compared in, so that "cluster features do not change v1" is something
//! a test asserts: the same requests, the same answers.
//!
//! The few answers that are about the server rather than the drive are
//! marked `per_server` and left out of that comparison: what `/readyz`
//! counts, what discovery lists, and what an unknown drive gets.
#![allow(dead_code)]

use serde_json::{Value, json};

use crate::openapi::{Operation, Reply, check_response, operations, resolve, validate};

/// The limits a server must be configured with for the sequence to mean
/// what it says. They are small so that the limits are reached cheaply.
pub const MAX_REQUEST_BYTES: usize = 4096;
pub const MAX_UPLOAD_BYTES: u64 = 16384;
pub const MAX_DIRECTORY_ENTRIES: usize = 48;

/// What a drive id that no server holds gets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnknownDrive {
    /// A single server: 401, the same as a wrong token.
    Unauthorized,
    /// A forwarding server: no route, 503 with `x-mount-dispatch`.
    Unrouted,
}

pub struct Target {
    /// `http://host:port`, without a trailing slash.
    pub base: String,
    pub drive: String,
    pub token: String,
    /// Whether this server holds the drive itself, so that discovery
    /// lists it and readiness counts it.
    pub holds_drive: bool,
    pub unknown_drive: UnknownDrive,
}

/// One request and its reply, reduced to what must be the same on every
/// server: values that differ from run to run are replaced by a marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub what: String,
    pub per_server: bool,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

pub struct Outcome {
    pub exchanges: Vec<Exchange>,
    pub failures: Vec<String>,
}

#[derive(Clone, Default)]
struct Call {
    drive: Option<String>,
    path: String,
    query: String,
    headers: Vec<(&'static str, String)>,
    body: Option<Vec<u8>>,
    anonymous: bool,
    token: Option<String>,
    per_server: bool,
}

impl Call {
    fn at(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            ..Self::default()
        }
    }
    fn drive(mut self, drive: &str) -> Self {
        self.drive = Some(drive.to_owned());
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
    fn token(mut self, token: &str) -> Self {
        self.token = Some(token.to_owned());
        self
    }
    fn per_server(mut self) -> Self {
        self.per_server = true;
        self
    }
}

/// JSON properties whose value differs between two runs of the sequence.
const VOLATILE: [&str; 11] = [
    "atime_ms",
    "mtime_ms",
    "ctime_ms",
    "birthtime_ms",
    "ino",
    "dev",
    "blocks",
    "blocks_free",
    "blocks_available",
    "files",
    "files_free",
];

fn scrub(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (name, value) in object.iter_mut() {
                if VOLATILE.contains(&name.as_str()) {
                    *value = json!("<varies>");
                } else {
                    scrub(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        _ => {}
    }
}

fn exchange(what: &str, per_server: bool, sent_id: Option<&str>, reply: &Reply) -> Exchange {
    let json = serde_json::from_slice::<Value>(&reply.body).ok();
    let mut headers: Vec<(String, String)> = reply
        .headers
        .iter()
        .filter(|(name, _)| name != "date")
        // The length of a JSON body follows the values scrubbed from it.
        .filter(|(name, _)| !(name == "content-length" && json.is_some()))
        .map(|(name, value)| {
            let value = match name.as_str() {
                "etag" => "<etag>".to_owned(),
                "x-request-id" if sent_id != Some(value.as_str()) => "<generated>".to_owned(),
                _ => value.clone(),
            };
            (name.clone(), value)
        })
        .collect();
    headers.sort();
    let body = match json {
        Some(mut body) => {
            scrub(&mut body);
            body.to_string()
        }
        None => format!("{} bytes: {:02x?}", reply.body.len(), {
            let shown = reply.body.len().min(64);
            &reply.body[..shown]
        }),
    };
    Exchange {
        what: what.to_owned(),
        per_server,
        status: reply.status,
        headers,
        body,
    }
}

struct Run<'a> {
    target: &'a Target,
    doc: &'a Value,
    cluster: Option<&'a Value>,
    operations: Vec<Operation>,
    client: reqwest::Client,
    outcome: Outcome,
}

impl Run<'_> {
    fn operation(&self, id: &str) -> Operation {
        self.operations
            .iter()
            .find(|operation| operation.id == id)
            .unwrap_or_else(|| panic!("the document has no operation `{id}`"))
            .clone()
    }

    async fn send(&self, method: &str, path: &str, call: &Call) -> Reply {
        let method = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let url = format!("{}{path}{}", self.target.base, call.query);
        let mut request = self.client.request(method, url);
        if !call.anonymous {
            let token = call.token.as_deref().unwrap_or(&self.target.token);
            request = request.header("authorization", format!("Bearer {token}"));
        }
        for (name, value) in &call.headers {
            request = request.header(*name, value);
        }
        if let Some(body) = &call.body {
            request = request.body(body.clone());
        }
        let response = request
            .send()
            .await
            .unwrap_or_else(|error| panic!("{path}: no response: {error}"));
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .unwrap_or_else(|error| panic!("{path}: body: {error}"));
        Reply::collect(status, &headers, &body)
    }

    /// Send `call` for the operation, require `status`, check the reply
    /// against the document and record it.
    async fn expect(&mut self, id: &str, call: Call, status: u16, what: &str) -> Reply {
        let operation = self.operation(id);
        let drive = call.drive.as_deref().unwrap_or(&self.target.drive);
        let path = operation
            .template
            .replace("{id}", drive)
            .replace("{path}", &call.path);
        let reply = self.send(&operation.method, &path, &call).await;
        let label = format!("{id} ({} {path}) {what}", operation.method);
        if reply.status != status {
            self.outcome.failures.push(format!(
                "{label}: expected {status}, the server answered {} {}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            ));
        }
        // A forwarding server that holds no drive is ready with none, which
        // the cluster document declares and the v1 document does not.
        let without_drives = !self.target.holds_drive
            && reply.status == 200
            && matches!(id, "getReadiness" | "headReadiness");
        let (doc, declared) = if without_drives {
            let cluster = self.cluster.expect("the cluster document");
            let declared = &cluster["components"]["responses"]["ForwardingReady"];
            (cluster, declared.is_object().then_some(declared))
        } else {
            let declared = operation.spec["responses"].get(reply.status.to_string());
            (self.doc, declared)
        };
        match declared {
            None => self.outcome.failures.push(format!(
                "{label}: {}: the document does not declare this status",
                reply.status
            )),
            Some(declared) => {
                let failures = check_response(doc, declared, &operation.method, &reply);
                self.outcome.failures.extend(
                    failures
                        .into_iter()
                        .map(|failure| format!("{label}: {}: {failure}", reply.status)),
                );
            }
        }
        self.record(&label, &call, &reply);
        reply
    }

    fn record(&mut self, label: &str, call: &Call, reply: &Reply) {
        let sent_id = call
            .headers
            .iter()
            .find(|(name, _)| *name == "x-request-id")
            .map(|(_, value)| value.as_str());
        self.outcome
            .exchanges
            .push(exchange(label, call.per_server, sent_id, reply));
    }

    fn require(&mut self, reply: &Reply, name: &str, value: Option<&str>, what: &str) {
        match (reply.header(name), value) {
            (None, _) => self
                .outcome
                .failures
                .push(format!("{what}: no `{name}` header")),
            (Some(actual), Some(value)) if actual != value => self
                .outcome
                .failures
                .push(format!("{what}: `{name}` is {actual:?}, not {value:?}")),
            _ => {}
        }
    }

    fn fail(&mut self, failure: String) {
        self.outcome.failures.push(failure);
    }
}

/// Send the sequence to `target` and return what it answered. `cluster` is
/// the cluster document, needed when the target is a forwarding server.
pub async fn run(target: &Target, doc: &Value, cluster: Option<&Value>) -> Outcome {
    let mut run = Run {
        target,
        doc,
        cluster,
        operations: operations(doc),
        client: reqwest::Client::new(),
        outcome: Outcome {
            exchanges: Vec::new(),
            failures: Vec::new(),
        },
    };
    let at = Call::at;
    let root = Call::default;

    // -- probes and discovery
    for id in ["getHealth", "headHealth"] {
        let reply = run.expect(id, root(), 200, "liveness").await;
        run.require(&reply, "cache-control", Some("no-store"), id);
        run.require(&reply, "x-content-type-options", Some("nosniff"), id);
    }
    for id in ["getReadiness", "headReadiness"] {
        let reply = run.expect(id, root().per_server(), 200, "readiness").await;
        run.require(&reply, "cache-control", Some("no-store"), id);
    }
    let ready = run
        .expect("getReadiness", root().per_server(), 200, "readiness")
        .await;
    let drives = serde_json::from_slice::<Value>(&ready.body).unwrap_or_default()["drives"]
        .as_u64()
        .unwrap_or(u64::MAX);
    if (drives > 0) != target.holds_drive {
        run.fail(format!(
            "readiness counts {drives} drives on a server that {} the drive",
            if target.holds_drive {
                "holds"
            } else {
                "does not hold"
            }
        ));
    }
    for id in ["listDrives", "headDrives"] {
        let status = if target.holds_drive { 200 } else { 401 };
        run.expect(id, root().per_server(), status, "discovery")
            .await;
        let reply = run.expect(id, root().anonymous(), 401, "no token").await;
        run.require(&reply, "www-authenticate", Some("Bearer"), id);
    }

    // -- the content the rest is written against
    for directory in [
        "/dir", "/full", "/empty", "/gone", "/wide", "/moved", "/huge",
    ] {
        run.expect(
            "mkdir",
            root().json(json!({"path": directory})),
            200,
            "setup",
        )
        .await;
    }
    for file in [
        "dir/a.txt",
        "dir/b.txt",
        "full/child",
        "gone/victim",
        "moved/from",
    ] {
        let reply = run
            .expect("putFile", at(file).body("hello world"), 200, "setup")
            .await;
        run.require(&reply, "etag", None, "putFile");
    }
    for index in 0..MAX_DIRECTORY_ENTRIES - 4 {
        let name = format!("wide/entry-with-a-long-name-{index:04}");
        run.expect("putFile", at(&name).body(""), 200, "setup")
            .await;
    }
    for index in 0..MAX_DIRECTORY_ENTRIES + 2 {
        let name = format!("huge/{index:04}");
        run.expect("putFile", at(&name).body(""), 200, "setup")
            .await;
    }

    // -- reading
    for id in ["getRoot", "headRoot"] {
        run.expect(id, root(), 200, "the root").await;
    }
    for id in ["getPath", "headPath"] {
        let reply = run.expect(id, at("dir/a.txt"), 200, "a file").await;
        run.require(&reply, "accept-ranges", Some("bytes"), id);
        run.require(&reply, "content-length", Some("11"), id);
        let etag = reply.header("etag").unwrap_or_default().to_owned();
        if etag.is_empty() {
            run.fail(format!("{id}: a file has no ETag"));
        }
        if id == "getPath" && reply.body != b"hello world" {
            run.fail(format!("{id}: the content is {:?}", reply.body));
        }
        run.expect(id, at("dir"), 200, "a directory").await;
        let range = at("dir/a.txt").header("range", "bytes=0-4");
        let reply = run.expect(id, range, 206, "a range").await;
        run.require(&reply, "content-range", Some("bytes 0-4/11"), id);
        run.require(&reply, "etag", Some(&etag), id);
        let suffix = at("dir/a.txt").header("range", "bytes=-5");
        let reply = run.expect(id, suffix, 206, "a suffix range").await;
        run.require(&reply, "content-range", Some("bytes 6-10/11"), id);
        let current = at("dir/a.txt").header("if-none-match", &etag);
        let reply = run.expect(id, current, 304, "a current tag").await;
        run.require(&reply, "etag", Some(&etag), id);
        let matching = at("dir/a.txt").header("if-match", &etag);
        run.expect(id, matching, 200, "a matching tag").await;
        let stale = at("dir/a.txt").header("if-match", "\"other\"");
        run.expect(id, stale, 412, "a stale tag").await;
        let ranged = at("dir/a.txt")
            .header("range", "bytes=0-4")
            .header("if-range", "\"other\"");
        run.expect(id, ranged, 200, "a range under a stale If-Range")
            .await;
        let past = at("dir/a.txt").header("range", "bytes=99-");
        let reply = run.expect(id, past, 416, "a range past the end").await;
        run.require(&reply, "content-range", Some("bytes */11"), id);
        run.expect(id, at("a%2Fb"), 400, "an encoded slash").await;
        run.expect(id, at("dir/.mount-rs-upload.x"), 400, "a reserved name")
            .await;
        run.expect(id, at("dir/none"), 404, "a missing file").await;
        let reply = run.expect(id, at("huge"), 413, "too many entries").await;
        // `Connection` belongs to one hop: the owner closes the forwarded
        // stream, and the ingress keeps the client's connection, which
        // has nothing unread on it.
        if target.holds_drive {
            run.require(&reply, "connection", Some("close"), id);
        }
    }

    // -- listing
    for (id, directory) in [
        ("listRootEntries", ""),
        ("headRootEntries", ""),
        ("listEntries", "wide"),
        ("headEntries", "wide"),
    ] {
        let reply = run.expect(id, at(directory), 200, "a listing").await;
        run.require(&reply, "vary", Some("Accept-Encoding"), id);
        run.expect(id, at(directory).query("?limit=2"), 200, "a page")
            .await;
        let last = at(directory).query("?limit=48");
        run.expect(id, last, 200, "the last page").await;
        run.expect(id, at(directory).query("?limit=0"), 400, "a bad limit")
            .await;
        let cursor = at(directory).query("?limit=2&cursor=nonsense");
        run.expect(id, cursor, 400, "a bad cursor").await;
        let refused = at(directory).header("accept-encoding", "gzip;q=0, identity;q=0");
        let reply = run.expect(id, refused, 406, "no acceptable coding").await;
        run.require(&reply, "vary", Some("Accept-Encoding"), id);
    }
    for id in ["listEntries", "headEntries"] {
        let gzip = at("wide").header("accept-encoding", "gzip");
        let reply = run.expect(id, gzip, 200, "a compressed listing").await;
        run.require(&reply, "content-encoding", Some("gzip"), id);
        run.expect(id, at("a%2Fb"), 400, "an encoded slash").await;
        run.expect(id, at("none"), 404, "a missing directory").await;
        run.expect(id, at("dir/a.txt"), 404, "a file").await;
        run.expect(id, at("huge"), 413, "too many entries").await;
        let paged = at("huge").query("?limit=2");
        run.expect(id, paged, 413, "too many entries, paged").await;
    }
    // Every entry once, in name order, whatever the page size.
    let mut names = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_DIRECTORY_ENTRIES {
        let query = match &cursor {
            Some(cursor) => format!("?limit=7&cursor={cursor}"),
            None => "?limit=7".to_owned(),
        };
        let reply = run
            .expect("listEntries", at("wide").query(&query), 200, "a walk")
            .await;
        let page: Value = serde_json::from_slice(&reply.body).unwrap_or_default();
        for entry in page["entries"].as_array().into_iter().flatten() {
            names.push(entry["name"].as_str().unwrap_or_default().to_owned());
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    if names != sorted || names.len() != MAX_DIRECTORY_ENTRIES - 4 {
        run.fail(format!(
            "a paginated walk returned {} names, not {} once each in order",
            names.len(),
            MAX_DIRECTORY_ENTRIES - 4
        ));
    }

    // -- metadata
    for kind in ["stat", "lstat", "statfs", "readlink"] {
        let capital = format!("{}{}", kind[..1].to_uppercase(), &kind[1..]);
        let link = kind == "readlink";
        for id in [format!("{kind}Root"), format!("head{capital}Root")] {
            let status = if link { 400 } else { 200 };
            run.expect(&id, root(), status, "the root").await;
        }
        for id in [format!("{kind}Path"), format!("head{capital}Path")] {
            let status = if link { 400 } else { 200 };
            run.expect(&id, at("dir/a.txt"), status, "a file").await;
            run.expect(&id, at("a%2Fb"), 400, "an encoded slash").await;
            if kind != "statfs" {
                run.expect(&id, at("none"), 404, "a missing path").await;
            }
        }
    }

    // -- writing and deleting
    let reply = run
        .expect("putFile", at("dir/new.txt").body("new"), 200, "a new file")
        .await;
    let created = reply.header("etag").unwrap_or_default().to_owned();
    let replace = at("dir/new.txt").header("if-match", &created).body("newer");
    let reply = run
        .expect("putFile", replace, 200, "a replacement under its tag")
        .await;
    if reply.header("etag") == Some(created.as_str()) {
        run.fail("putFile: a replaced file kept its ETag".to_owned());
    }
    let lost = at("dir/new.txt").header("if-match", &created).body("lost");
    run.expect("putFile", lost, 412, "a replacement under the old tag")
        .await;
    let reply = run
        .expect(
            "getPath",
            at("dir/new.txt"),
            200,
            "the file after a refused write",
        )
        .await;
    if reply.body != b"newer" {
        run.fail(format!(
            "a refused write changed the file: {:?}",
            reply.body
        ));
    }
    let only = at("dir/a.txt").header("if-none-match", "*").body("x");
    run.expect("putFile", only, 412, "create-only on a file")
        .await;
    let only = at("dir/once.txt").header("if-none-match", "*").body("x");
    run.expect("putFile", only, 200, "create-only on a free name")
        .await;
    run.expect("putFile", at("none/x").body("x"), 404, "a missing parent")
        .await;
    run.expect("putFile", at("dir").body("x"), 409, "a directory")
        .await;
    run.expect("putFile", at("a%2Fb").body("x"), 400, "an encoded slash")
        .await;
    // Larger than a body the server holds whole: streamed to storage.
    let streamed: Vec<u8> = (0..MAX_REQUEST_BYTES * 2 + 77)
        .map(|index| (index % 251) as u8)
        .collect();
    run.expect(
        "putFile",
        at("dir/streamed.bin").body(streamed.clone()),
        200,
        "a streamed upload",
    )
    .await;
    let reply = run
        .expect(
            "getPath",
            at("dir/streamed.bin"),
            200,
            "the streamed upload",
        )
        .await;
    if reply.body != streamed {
        run.fail("a streamed upload was not read back as it was sent".to_owned());
    }
    let middle = at("dir/streamed.bin").header("range", "bytes=5000-5099");
    let reply = run
        .expect("getPath", middle, 206, "a range of the streamed upload")
        .await;
    if reply.body != streamed[5000..5100] {
        run.fail("a range of a streamed upload has the wrong bytes".to_owned());
    }
    let large = at("dir/large").body(vec![b'x'; MAX_UPLOAD_BYTES as usize + 1]);
    let reply = run.expect("putFile", large, 413, "an oversized body").await;
    run.require(&reply, "connection", Some("close"), "putFile 413");
    run.expect("getPath", at("dir/large"), 404, "the refused upload")
        .await;
    run.expect("putRoot", root().body("x"), 409, "the root")
        .await;

    run.expect("deleteRoot", root(), 400, "the root").await;
    run.expect("deletePath", at("gone/victim"), 204, "a file")
        .await;
    run.expect("deletePath", at("gone"), 204, "an empty directory")
        .await;
    run.expect("deletePath", at("gone"), 404, "already gone")
        .await;
    run.expect("deletePath", at("full"), 409, "a full directory")
        .await;
    let stale = at("dir/a.txt").header("if-match", "\"other\"");
    run.expect("deletePath", stale, 412, "a stale tag").await;
    run.expect("deletePath", at("a%2Fb"), 400, "an encoded slash")
        .await;

    // -- operations
    let op = |body: Value| root().json(body);
    let deep = json!({"path": "/made/deep", "recursive": true});
    run.expect("mkdir", op(deep), 200, "with parents").await;
    let one = json!({"path": "/made/one"});
    run.expect("mkdir", op(one.clone()), 200, "one").await;
    run.expect("mkdir", op(one), 409, "again").await;
    let orphan = json!({"path": "/none/x"});
    run.expect("mkdir", op(orphan), 404, "no parent").await;
    let relative = json!({"path": "relative"});
    run.expect("mkdir", op(relative), 400, "a relative path")
        .await;
    run.expect("mkdir", root().body("{"), 400, "not JSON").await;
    run.expect("mkdir", op(json!({})), 400, "no path").await;
    let renamed = json!({"from": "/moved/from", "to": "/moved/to"});
    run.expect("rename", op(renamed.clone()), 200, "a file")
        .await;
    run.expect("rename", op(renamed), 404, "again").await;
    let onto = json!({"from": "/empty", "to": "/full"});
    run.expect("rename", op(onto), 409, "onto a full directory")
        .await;
    let half = json!({"from": "/dir"});
    run.expect("rename", op(half), 400, "no target").await;
    let cut = json!({"path": "/dir/b.txt", "length": 2});
    run.expect("truncate", op(cut), 200, "a file").await;
    let reply = run
        .expect("getPath", at("dir/b.txt"), 200, "the truncated file")
        .await;
    if reply.body != b"he" {
        run.fail(format!("a truncated file reads {:?}", reply.body));
    }
    let missing = json!({"path": "/none", "length": 2});
    run.expect("truncate", op(missing), 404, "a missing file")
        .await;
    let negative = json!({"path": "/dir/b.txt", "length": -1});
    run.expect("truncate", op(negative), 400, "a negative length")
        .await;
    run.expect("sync", root(), 204, "a flush").await;
    run.expect("sync", root().body("x"), 400, "a body").await;
    for id in ["mkdir", "rename", "truncate", "sync"] {
        let large = root().body(vec![b' '; MAX_REQUEST_BYTES + 1]);
        let reply = run.expect(id, large, 413, "an oversized body").await;
        run.require(&reply, "connection", Some("close"), id);
    }

    // -- request ids
    let named = at("dir/a.txt").header("x-request-id", "client-chosen.id_1");
    let reply = run
        .expect("statPath", named, 200, "a client request id")
        .await;
    run.require(
        &reply,
        "x-request-id",
        Some("client-chosen.id_1"),
        "a client request id",
    );
    let unsafe_id = at("dir/a.txt").header("x-request-id", "not a safe id");
    let reply = run
        .expect("statPath", unsafe_id, 200, "an unsafe request id")
        .await;
    let generated = reply.header("x-request-id").unwrap_or_default();
    if generated.len() != 32 || !generated.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        run.fail(format!(
            "an unsafe request id was answered with {generated:?}"
        ));
    }
    let failed = at("dir/none").header("x-request-id", "client-chosen.id_2");
    let reply = run
        .expect("statPath", failed, 404, "a client request id on an error")
        .await;
    run.require(
        &reply,
        "x-request-id",
        Some("client-chosen.id_2"),
        "a client request id on an error",
    );

    // -- what every drive operation answers to a request that is not let in
    for operation in run.operations.clone() {
        if !operation.on_a_drive() {
            continue;
        }
        let id = operation.id.as_str();
        let ordinary = || match id {
            "mkdir" => at("x").json(json!({"path": "/refused"})),
            "rename" => at("x").json(json!({"from": "/dir/a.txt", "to": "/dir/z"})),
            "truncate" => at("x").json(json!({"path": "/dir/a.txt", "length": 1})),
            _ if operation.method == "PUT" => at("dir/refused").body("refused"),
            _ => at("dir/a.txt"),
        };
        let reply = run
            .expect(id, ordinary().anonymous(), 401, "no token")
            .await;
        run.require(&reply, "www-authenticate", Some("Bearer"), id);
        let wrong = ordinary().token("not-the-token-of-this-drive-0123456789");
        let reply = run.expect(id, wrong, 401, "a wrong token").await;
        run.require(&reply, "www-authenticate", Some("Bearer"), id);
        let basic = ordinary()
            .anonymous()
            .header("authorization", "Basic dXNlcjpwYXNz");
        run.expect(id, basic, 401, "another scheme").await;

        // A drive no server holds.
        let call = ordinary().drive("no-such-drive").per_server();
        let path = operation
            .template
            .replace("{id}", "no-such-drive")
            .replace("{path}", &call.path);
        let reply = run.send(&operation.method, &path, &call).await;
        let label = format!("{id} on a drive no server holds");
        let (status, declared) = match target.unknown_drive {
            UnknownDrive::Unauthorized => (401, operation.spec["responses"]["401"].clone()),
            UnknownDrive::Unrouted => (
                503,
                run.cluster.expect("the cluster document")["components"]["responses"]
                    ["ForwardingUnavailable"]
                    .clone(),
            ),
        };
        if reply.status != status {
            run.fail(format!(
                "{label}: expected {status}, the server answered {}",
                reply.status
            ));
        }
        let against = match target.unknown_drive {
            UnknownDrive::Unauthorized => run.doc,
            UnknownDrive::Unrouted => run.cluster.expect("the cluster document"),
        };
        let failures = check_response(against, &declared, &operation.method, &reply);
        run.outcome.failures.extend(
            failures
                .into_iter()
                .map(|failure| format!("{label}: {failure}")),
        );
        run.record(&label, &call, &reply);
    }

    // -- methods a route does not have
    let mut templates: Vec<String> = run
        .operations
        .iter()
        .filter(|operation| operation.on_a_drive())
        .map(|operation| operation.template.clone())
        .collect();
    templates.sort();
    templates.dedup();
    for template in templates {
        let path = template
            .replace("{id}", &target.drive)
            .replace("{path}", "dir/a.txt");
        let documented: Vec<&Operation> = run
            .operations
            .iter()
            .filter(|operation| operation.template == template)
            .collect();
        let declared = documented
            .iter()
            .find_map(|operation| operation.spec["responses"].get("405"))
            .cloned();
        let mut allowed: Vec<String> = documented
            .iter()
            .map(|operation| operation.method.clone())
            .collect();
        allowed.sort();
        let call = Call::default();
        let reply = run.send("PATCH", &path, &call).await;
        let label = format!("PATCH {template}");
        if reply.status != 405 {
            run.fail(format!("{label}: the server answered {}", reply.status));
        }
        let mut allow: Vec<String> = reply
            .header("allow")
            .unwrap_or_default()
            .split(',')
            .map(|method| method.trim().to_owned())
            .collect();
        allow.sort();
        if allow != allowed {
            run.fail(format!("{label}: Allow is {allow:?}, not {allowed:?}"));
        }
        match declared {
            Some(declared) => {
                let failures = check_response(run.doc, &declared, "PATCH", &reply);
                run.outcome.failures.extend(
                    failures
                        .into_iter()
                        .map(|failure| format!("{label}: {failure}")),
                );
            }
            None => run.fail(format!("{label}: no operation declares 405")),
        }
        run.record(&label, &call, &reply);
    }

    // -- paths that are no route
    for path in [
        "/".to_owned(),
        "/v2/drives".to_owned(),
        "/metrics".to_owned(),
        format!("/v1/drives/{}", target.drive),
        format!("/v1/drives/{}/ops/chown", target.drive),
        format!("/v1/drives/{}/copy/dir/a.txt", target.drive),
    ] {
        let call = Call::default();
        let reply = run.send("GET", &path, &call).await;
        let label = format!("GET {path}");
        if reply.status != 404 {
            run.fail(format!("{label}: the server answered {}", reply.status));
        }
        let error = &run.doc["components"]["schemas"]["Error"];
        let body: Value = serde_json::from_slice(&reply.body).unwrap_or_default();
        if let Err(failure) = validate(run.doc, resolve(run.doc, error), &body, "body") {
            run.fail(format!("{label}: {failure}"));
        }
        if body["error"]["code"] != "ENOENT" {
            run.fail(format!("{label}: the code is {}", body["error"]["code"]));
        }
        run.record(&label, &call, &reply);
    }

    run.outcome
}

/// Where two runs of the sequence differ, leaving out the answers that
/// are about the server. Empty when `other` answered as `reference` did.
///
/// `forwarded` says that `other` was reached through another server. The
/// one thing forwarding is specified to change in an answer is its
/// `Connection` header, which belongs to a single hop, so it is left out
/// of that comparison and of no other.
pub fn differences(reference: &Outcome, other: &Outcome, forwarded: bool) -> Vec<String> {
    let hop = |exchange: &Exchange| {
        let mut exchange = exchange.clone();
        if forwarded {
            exchange.headers.retain(|(name, _)| name != "connection");
        }
        exchange
    };
    let mut found = Vec::new();
    if reference.exchanges.len() != other.exchanges.len() {
        found.push(format!(
            "{} exchanges against {}",
            reference.exchanges.len(),
            other.exchanges.len()
        ));
    }
    for (expected, actual) in reference.exchanges.iter().zip(&other.exchanges) {
        if expected.per_server || actual.per_server {
            continue;
        }
        let (expected, actual) = (&hop(expected), &hop(actual));
        if expected != actual {
            found.push(format!(
                "{}:\n  reference: {} {:?} {}\n  other:     {} {:?} {}",
                expected.what,
                expected.status,
                expected.headers,
                expected.body,
                actual.status,
                actual.headers,
                actual.body
            ));
        }
    }
    found
}
