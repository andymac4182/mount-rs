use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use mount_rs_core::MemoryFs;
use mount_rs_webdav::protocol::{
    RangeSpec, collect_body, href_of, parse_depth, parse_destination, parse_lock_info,
    parse_lock_token, parse_overwrite, parse_range, parse_target_path, parse_xml, status_of_error,
};
use mount_rs_webdav::{
    ALLOW_HEADER, DAV_COMPLIANCE, DAV_NS, DavFault, Depth, WebdavRequestHead, WebdavServer,
    WebdavServerHooks, WebdavServerOptions, WebdavSession, WebdavSessionHooks,
    WebdavSessionOptions, WebdavTransportErrorKind, create_webdav_server,
    create_webdav_server_with_hooks, status_for_error, status_line, status_text,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

fn method(name: &str) -> reqwest::Method {
    reqwest::Method::from_bytes(name.as_bytes()).expect("valid HTTP method")
}

#[test]
fn public_constants_and_status_helpers_match_the_transport_contract() {
    assert_eq!(DAV_NS, "DAV:");
    assert_eq!(DAV_COMPLIANCE, "1, 2, 3");
    assert_eq!(
        ALLOW_HEADER,
        "OPTIONS, HEAD, GET, PUT, DELETE, MKCOL, COPY, MOVE, PROPFIND, PROPPATCH, LOCK, UNLOCK"
    );
    assert_eq!(status_text(207), Some("Multi-Status"));
    assert_eq!(status_line(423), "HTTP/1.1 423 Locked");
    assert_eq!(status_for_error(mount_rs_core::ErrorCode::Enoent), 404);
}

async fn server() -> WebdavServer {
    let fs = Arc::new(MemoryFs::empty());
    let server = create_webdav_server(fs, WebdavServerOptions::default()).expect("loopback bind");
    server.listen().await.expect("listen");
    server
}

#[tokio::test]
async fn transport_connection_failures_are_reported() {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let notified = Arc::new(Notify::new());
    let callback_reports = Arc::clone(&reports);
    let callback_notified = Arc::clone(&notified);
    let hooks = WebdavServerHooks {
        on_transport_error: Some(Arc::new(move |error| {
            callback_reports
                .lock()
                .expect("WebDAV hook lock")
                .push(error);
            callback_notified.notify_waiters();
        })),
    };
    let server = create_webdav_server_with_hooks(
        Arc::new(MemoryFs::empty()),
        WebdavServerOptions::default(),
        hooks,
    )
    .expect("loopback bind");
    server.listen().await.expect("listen");
    let mut stream = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("connect");
    stream
        .write_all(b"not a valid HTTP request\r\n\r\n")
        .await
        .expect("write malformed request");

    timeout(Duration::from_secs(1), async {
        loop {
            if !reports.lock().expect("WebDAV report lock").is_empty() {
                break;
            }
            notified.notified().await;
        }
    })
    .await
    .expect("transport callback");
    let report = reports.lock().expect("WebDAV report lock")[0].clone();
    assert_eq!(report.kind, WebdavTransportErrorKind::Connection);
    assert!(
        report
            .peer
            .as_deref()
            .is_some_and(|peer| peer.starts_with("127.0.0.1:"))
    );
    server.close().await.expect("close");
}

#[tokio::test(flavor = "current_thread")]
async fn immediate_close_after_listen_does_not_lose_shutdown_wakeup() {
    let server = create_webdav_server(
        Arc::new(MemoryFs::empty()),
        WebdavServerOptions {
            drain_timeout: Duration::from_millis(250),
            ..WebdavServerOptions::default()
        },
    )
    .expect("loopback bind");

    server.listen().await.expect("listen");
    timeout(Duration::from_secs(1), server.close())
        .await
        .expect("close did not complete")
        .expect("close");
    assert_eq!(server.connections(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_listen_calls_share_one_lifecycle() {
    let probe = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("reserve loopback port");
    let port = probe.local_addr().expect("probe address").port();
    drop(probe);

    let server = Arc::new(
        create_webdav_server(
            Arc::new(MemoryFs::empty()),
            WebdavServerOptions {
                port,
                ..WebdavServerOptions::default()
            },
        )
        .expect("loopback bind"),
    );
    let mut calls = Vec::new();
    for _ in 0..16 {
        let server = Arc::clone(&server);
        calls.push(tokio::spawn(async move { server.listen().await }));
    }
    for call in calls {
        call.await.expect("listen task").expect("listen");
    }
    assert_eq!(server.port(), port);
    server.close().await.expect("close");
}

#[tokio::test]
async fn session_errors_are_reported_once_with_the_request_head() {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let callback_reports = Arc::clone(&reports);
    let session = WebdavSession::new_with_hooks(
        Arc::new(MemoryFs::empty()),
        WebdavSessionOptions::default(),
        WebdavSessionHooks {
            on_error: Some(Arc::new(move |error, head| {
                callback_reports
                    .lock()
                    .expect("WebDAV request hook lock")
                    .push((error.to_string(), head));
            })),
        },
    );
    let unsupported_head = WebdavRequestHead {
        method: "PATCH".to_owned(),
        target: "/unsupported".to_owned(),
        headers: Default::default(),
    };
    let unsupported = session
        .handle_request(unsupported_head.clone(), Vec::<u8>::new())
        .await;
    assert_eq!(unsupported.status, 405);

    let authenticated = WebdavSession::new_with_hooks(
        Arc::new(MemoryFs::empty()),
        WebdavSessionOptions {
            credentials: Some(mount_rs_webdav::WebdavCredentials {
                username: "user".to_owned(),
                password: "secret".to_owned(),
            }),
            ..WebdavSessionOptions::default()
        },
        WebdavSessionHooks {
            on_error: Some(Arc::new({
                let reports = Arc::clone(&reports);
                move |error, head| {
                    reports
                        .lock()
                        .expect("WebDAV request hook lock")
                        .push((error.to_string(), head));
                }
            })),
        },
    );
    let unauthorized_head = WebdavRequestHead {
        method: "OPTIONS".to_owned(),
        target: "/".to_owned(),
        headers: Default::default(),
    };
    let unauthorized = authenticated
        .handle_request(unauthorized_head.clone(), Vec::<u8>::new())
        .await;
    assert_eq!(unauthorized.status, 401);

    let reports = reports.lock().expect("WebDAV request report lock");
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].1.method, unsupported_head.method);
    assert_eq!(reports[0].1.target, unsupported_head.target);
    assert_eq!(reports[1].1.method, unauthorized_head.method);
    assert_eq!(reports[1].1.target, unauthorized_head.target);
}

#[tokio::test]
async fn injected_session_clock_controls_lock_expiry_deterministically() {
    let now = Arc::new(AtomicI64::new(1_000));
    let clock = Arc::clone(&now);
    let options = WebdavSessionOptions {
        now: Some(Arc::new(move || clock.load(Ordering::SeqCst))),
        locks: mount_rs_webdav::DavLockTableOptions {
            default_timeout_seconds: 1,
            max_timeout_seconds: 1,
            ..mount_rs_webdav::DavLockTableOptions::default()
        },
        ..WebdavSessionOptions::default()
    };

    let session = WebdavSession::new(Arc::new(MemoryFs::empty()), options);
    let lock = session
        .handle_request(
            WebdavRequestHead {
                method: "LOCK".to_owned(),
                target: "/clocked".to_owned(),
                headers: [("timeout".to_owned(), "Second-1".to_owned())]
                    .into_iter()
                    .collect(),
            },
            br#"<lockinfo xmlns="DAV:"><lockscope><exclusive/></lockscope><locktype><write/></locktype></lockinfo>"#
                .to_vec(),
        )
        .await;

    assert_eq!(lock.status, 201);
    assert_eq!(session.lock_count(), 1);
    assert_eq!(session.lock_records()[0].expires_at, 2_000);

    now.store(1_999, Ordering::SeqCst);
    assert_eq!(session.lock_count(), 1);
    now.store(2_000, Ordering::SeqCst);
    assert_eq!(session.lock_count(), 0);
    assert!(session.lock_records().is_empty());
}

async fn read_http_response(stream: &mut TcpStream) -> (u16, Vec<u8>) {
    let mut response = Vec::new();
    let (header_end, content_length) = loop {
        let mut chunk = [0_u8; 1024];
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "connection ended before the HTTP response");
        response.extend_from_slice(&chunk[..count]);
        let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&response[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .or_else(|| line.strip_prefix("Content-Length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        break (header_end, content_length);
    };
    let body_start = header_end + 4;
    while response.len() < body_start + content_length {
        let mut chunk = [0_u8; 1024];
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "connection ended before the HTTP body");
        response.extend_from_slice(&chunk[..count]);
    }
    let status = String::from_utf8_lossy(&response[..header_end])
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("HTTP status line");
    (
        status,
        response[body_start..body_start + content_length].to_vec(),
    )
}

#[tokio::test]
async fn protocol_fixtures_match_mountx_path_and_header_rules() {
    assert_eq!(parse_target_path("/a%20b/c%C3%A9").unwrap(), "/a b/cé");
    assert_eq!(parse_target_path("/a?ignored=yes").unwrap(), "/a");
    assert_eq!(parse_target_path("/a/../../b").unwrap(), "/b");
    assert_eq!(href_of("/a b/c", false), "/a%20b/c");
    assert_eq!(href_of("/a?b", false), "/a%3Fb");
    assert_eq!(href_of("/a#b", false), "/a%23b");
    assert_eq!(href_of("/a%b", false), "/a%25b");
    assert_eq!(href_of("/a;b&c", false), "/a%3Bb%26c");
    assert_eq!(href_of("/dir", true), "/dir/");

    for target in ["/a%", "/a%zz", "/a%C3%28", "/a%00b", "relative"] {
        assert_eq!(
            parse_target_path(target).unwrap_err().status,
            400,
            "target {target}"
        );
    }
    assert_eq!(parse_depth(None, Depth::Infinity), Some(Depth::Infinity));
    assert_eq!(parse_depth(Some("0"), Depth::One), Some(Depth::Zero));
    assert_eq!(parse_depth(Some("1"), Depth::Zero), Some(Depth::One));
    assert_eq!(
        parse_depth(Some("Infinity"), Depth::Zero),
        Some(Depth::Infinity)
    );
    assert_eq!(parse_depth(Some("2"), Depth::Zero), None);
    assert_eq!(
        parse_range(Some("bytes=99-not-a-number"), 4),
        RangeSpec::Full
    );
    assert_eq!(
        parse_range(Some("bytes=99-101"), 4),
        RangeSpec::Unsatisfiable
    );
    assert_eq!(
        parse_range(Some("bytes=0-1,3-4"), 6),
        RangeSpec::Full,
        "mountx ignores multi-range requests rather than emitting multipart/byteranges"
    );
    assert_eq!(parse_overwrite(None), Some(true));
    assert_eq!(parse_overwrite(Some("f")), Some(false));
    assert_eq!(parse_overwrite(Some("yes")), None);
    assert_eq!(
        parse_destination(
            Some("http://dav.example:8080/a%20b"),
            Some("dav.example:8080")
        )
        .unwrap(),
        "/a b"
    );
    assert_eq!(
        parse_destination(Some("/a/b"), Some("dav.example")).unwrap(),
        "/a/b"
    );
    assert_eq!(
        parse_destination(Some("http://elsewhere/a"), Some("dav.example"))
            .unwrap_err()
            .status,
        502
    );

    let too_large = collect_body(b"12345", 4).unwrap_err();
    assert_eq!(status_of_error(&too_large), 413);
    assert_eq!(
        parse_lock_token(Some("<urn:uuid:token>")),
        Some("urn:uuid:token".to_owned())
    );
    assert_eq!(parse_lock_token(Some("<a><b>")), None);
    let lock_info = parse_lock_info(
        br#"<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><Z:name xmlns:Z="urn:test">A&amp;B&#x21;</Z:name></D:owner></D:lockinfo>"#,
        256 * 1024,
    )
    .expect("valid lockinfo")
    .expect("lockinfo body");
    assert_eq!(
        lock_info.owner.expect("lock owner").children[0].text,
        "A&B!"
    );
    assert_eq!(
        parse_xml(br#"<x>&quot;&apos;&#65;&#x42;</x>"#, 256)
            .unwrap()
            .text,
        "\"'AB"
    );
    assert_eq!(
        parse_xml(br#"<x>&unknown;</x>"#, 256).unwrap_err().status,
        400
    );

    let mut nested = String::new();
    for _ in 0..33 {
        nested.push_str("<x>");
    }
    for _ in 0..33 {
        nested.push_str("</x>");
    }
    assert_eq!(
        parse_xml(nested.as_bytes(), 256 * 1024).unwrap_err().status,
        400
    );
}

#[tokio::test]
async fn http_round_trip_covers_class_one_methods_and_properties() {
    let server = server().await;
    let client = reqwest::Client::new();
    let base = server.url();

    let options = client
        .request(method("OPTIONS"), format!("{base}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(options.status(), 200);
    assert_eq!(options.headers()["dav"], "1, 2, 3");
    assert!(
        options.headers()["allow"]
            .to_str()
            .unwrap()
            .contains("PROPFIND")
    );

    let response = client
        .request(method("MKCOL"), format!("{base}/notes"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);

    let response = client
        .put(format!("{base}/notes/a%20b.txt"))
        .body("the body")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert!(
        response.headers()["etag"]
            .to_str()
            .unwrap()
            .starts_with('"')
    );

    let propfind = client
        .request(method("PROPFIND"), format!("{base}/notes"))
        .header("Depth", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(propfind.status(), 207);
    assert_eq!(
        propfind.headers()["content-type"],
        "application/xml; charset=\"utf-8\""
    );
    let listing = propfind.text().await.unwrap();
    assert!(listing.contains("<href>/notes/</href>"));
    assert!(listing.contains("<href>/notes/a%20b.txt</href>"));
    assert!(listing.contains("<getetag>"));

    let get = client
        .get(format!("{base}/notes/a%20b.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 200);
    assert_eq!(get.text().await.unwrap(), "the body");

    let head = client
        .head(format!("{base}/notes/a%20b.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], "8");
    assert!(head.bytes().await.unwrap().is_empty());

    let copy = client
        .request(method("COPY"), format!("{base}/notes/a%20b.txt"))
        .header("Destination", format!("{base}/notes/copied.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(copy.status(), 201);
    assert_eq!(
        client
            .get(format!("{base}/notes/copied.txt"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "the body"
    );

    let move_response = client
        .request(method("MOVE"), format!("{base}/notes/copied.txt"))
        .header("Destination", format!("{base}/notes/renamed.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(move_response.status(), 201);

    let delete = client.delete(format!("{base}/notes")).send().await.unwrap();
    assert_eq!(delete.status(), 204);
    assert_eq!(
        client
            .get(format!("{base}/notes/renamed.txt"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    server.close().await.unwrap();
}

#[tokio::test]
async fn unsupported_dead_props_extended_mkcol_and_multi_range_match_mountx() {
    let server = server().await;
    let client = reqwest::Client::new();
    let base = server.url();

    let created = client
        .put(format!("{base}/resource"))
        .body("abcdef")
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);

    let unknown_propfind = client
        .request(method("PROPFIND"), format!("{base}/resource"))
        .header("Depth", "0")
        .body(
            r#"<D:propfind xmlns:D="DAV:" xmlns:Z="urn:example"><D:prop><Z:dead/></D:prop></D:propfind>"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_propfind.status(), 207);
    let unknown_body = unknown_propfind.text().await.unwrap();
    assert!(unknown_body.contains("HTTP/1.1 404 Not Found"));
    assert!(unknown_body.contains(r#"<dead xmlns="urn:example"></dead>"#));

    let dead_patch = client
        .request(method("PROPPATCH"), format!("{base}/resource"))
        .body(
            r#"<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:example"><D:set><D:prop><Z:dead>value</Z:dead></D:prop></D:set></D:propertyupdate>"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(dead_patch.status(), 207);
    let dead_body = dead_patch.text().await.unwrap();
    assert!(dead_body.contains("HTTP/1.1 403 Forbidden"));
    assert!(dead_body.contains("cannot-modify-protected-property"));
    assert!(dead_body.contains(r#"<dead xmlns="urn:example"></dead>"#));

    let still_unknown = client
        .request(method("PROPFIND"), format!("{base}/resource"))
        .header("Depth", "0")
        .body(
            r#"<D:propfind xmlns:D="DAV:" xmlns:Z="urn:example"><D:prop><Z:dead/></D:prop></D:propfind>"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(still_unknown.status(), 207);
    assert!(
        still_unknown
            .text()
            .await
            .unwrap()
            .contains("HTTP/1.1 404 Not Found")
    );

    let extended_mkcol = client
        .request(method("MKCOL"), format!("{base}/extended"))
        .body(r#"<D:mkcol xmlns:D="DAV:"><D:set/></D:mkcol>"#)
        .send()
        .await
        .unwrap();
    assert_eq!(extended_mkcol.status(), 415);
    assert_eq!(
        client
            .get(format!("{base}/extended"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    let multi_range = client
        .get(format!("{base}/resource"))
        .header("Range", "bytes=0-1,3-4")
        .send()
        .await
        .unwrap();
    assert_eq!(multi_range.status(), 200);
    assert!(multi_range.headers().get("content-range").is_none());
    assert_eq!(multi_range.text().await.unwrap(), "abcdef");

    server.close().await.unwrap();
}

#[tokio::test]
async fn ranges_conditionals_auth_and_request_limits_are_real_http() {
    let fs = Arc::new(MemoryFs::empty());
    let options = WebdavServerOptions {
        max_request_bytes: 4,
        session: WebdavSessionOptions {
            max_body_bytes: Some(4),
            ..WebdavSessionOptions::default()
        },
        ..WebdavServerOptions::default()
    };
    let server = create_webdav_server(fs, options).unwrap();
    server.listen().await.unwrap();
    let client = reqwest::Client::new();
    let base = server.url();

    let oversized = client
        .put(format!("{base}/too-large"))
        .body("12345")
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);

    let created = client
        .put(format!("{base}/range"))
        .body("abcd")
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let etag = created.headers()["etag"].to_str().unwrap().to_owned();
    let partial = client
        .get(format!("{base}/range"))
        .header("Range", "bytes=1-2")
        .send()
        .await
        .unwrap();
    assert_eq!(partial.status(), 206);
    assert_eq!(partial.headers()["content-range"], "bytes 1-2/4");
    assert_eq!(partial.text().await.unwrap(), "bc");

    let not_modified = client
        .get(format!("{base}/range"))
        .header("If-None-Match", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(not_modified.status(), 304);
    assert!(not_modified.bytes().await.unwrap().is_empty());

    let current = client.get(format!("{base}/range")).send().await.unwrap();
    let current_etag = current.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        client
            .get(format!("{base}/range"))
            .header("If-Match", &current_etag)
            .header("If-Unmodified-Since", "Thu, 01 Jan 1970 00:00:00 GMT")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/range"))
            .header("If-None-Match", "\"not-the-current-etag\"")
            .header("If-Modified-Since", "Sun, 06 Nov 2094 08:49:37 GMT")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    server.close().await.unwrap();
}

#[tokio::test]
async fn basic_auth_is_required_when_configured() {
    let fs = Arc::new(MemoryFs::empty());
    let options = WebdavServerOptions::default().with_credentials("ada", "secret");
    let server = create_webdav_server(fs, options).unwrap();
    server.listen().await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("{}/", server.url());

    let unauthorized = client
        .request(method("OPTIONS"), &url)
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    assert!(
        unauthorized.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Basic realm=")
    );

    let authorized = client
        .request(method("OPTIONS"), url)
        .basic_auth("ada", Some("secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), 200);
    server.close().await.unwrap();
}

#[tokio::test]
async fn locks_and_proppatch_use_rfc_statuses_and_multistatus() {
    let server = server().await;
    let client = reqwest::Client::new();
    let base = server.url();
    let lock_body = r#"<?xml version="1.0" encoding="UTF-8"?>
        <lockinfo xmlns="DAV:"><lockscope><exclusive/></lockscope>
        <locktype><write/></locktype><owner><href>tester</href></owner></lockinfo>"#;
    let lock = client
        .request(method("LOCK"), format!("{base}/locked"))
        .header("Timeout", "Second-600")
        .body(lock_body)
        .send()
        .await
        .unwrap();
    assert_eq!(lock.status(), 201);
    let token = lock.headers()["lock-token"].to_str().unwrap().to_owned();

    let blocked = client
        .put(format!("{base}/locked"))
        .body("blocked")
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), 423);

    let allowed = client
        .put(format!("{base}/locked"))
        .header("If", format!("({token})"))
        .body("allowed")
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 204);

    let patch = client
        .request(method("PROPPATCH"), format!("{base}/locked"))
        .header("If", format!("({token})"))
        .body(
            r#"<propertyupdate xmlns="DAV:"><set><prop><getdisplayname>new</getdisplayname></prop></set></propertyupdate>"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(patch.status(), 207);
    let patch_body = patch.text().await.unwrap();
    assert!(patch_body.contains("403"));
    assert!(patch_body.contains("cannot-modify-protected-property"));

    let unlock = client
        .request(method("UNLOCK"), format!("{base}/locked"))
        .header("Lock-Token", token)
        .send()
        .await
        .unwrap();
    assert_eq!(unlock.status(), 204);
    server.close().await.unwrap();
}

#[tokio::test]
async fn recursive_delete_honors_submitted_member_lock_tokens() {
    let server = server().await;
    let client = reqwest::Client::new();
    let base = server.url();
    assert_eq!(
        client
            .request(method("MKCOL"), format!("{base}/tree"))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    assert_eq!(
        client
            .put(format!("{base}/tree/member"))
            .body("x")
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    let lock_body = r#"<lockinfo xmlns="DAV:"><lockscope><shared/></lockscope><locktype><write/></locktype></lockinfo>"#;
    let lock = client
        .request(method("LOCK"), format!("{base}/tree/member"))
        .body(lock_body)
        .send()
        .await
        .unwrap();
    assert_eq!(lock.status(), 200);
    let token = lock.headers()["lock-token"].to_str().unwrap().to_owned();

    let blocked = client.delete(format!("{base}/tree")).send().await.unwrap();
    assert_eq!(blocked.status(), 207);
    assert!(blocked.text().await.unwrap().contains("423"));

    let allowed = client
        .delete(format!("{base}/tree"))
        .header("If", format!("</tree/member> ({token})"))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 204);
    server.close().await.unwrap();
}

#[tokio::test]
async fn chunked_put_streams_request_body_over_a_real_connection() {
    let server = server().await;
    let mut stream = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    stream
        .write_all(
            b"PUT /chunked HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
        )
        .await
        .unwrap();
    // The first chunk is deliberately split across writes: the body reaches
    // Hyper incrementally while the HTTP chunk framing remains valid.
    stream.write_all(b"4\r\n").await.unwrap();
    stream.write_all(b"abc").await.unwrap();
    stream.write_all(b"d\r\n3\r\n").await.unwrap();
    stream.write_all(b"ef").await.unwrap();
    stream.write_all(b"g\r\n0\r\n\r\n").await.unwrap();
    let (status, body) = read_http_response(&mut stream).await;
    assert_eq!(status, 201);
    assert!(body.is_empty());

    stream
        .write_all(b"GET /chunked HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let (status, body) = read_http_response(&mut stream).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"abcdefg");
    drop(stream);
    server.close().await.unwrap();
}

#[tokio::test]
async fn chunked_request_limit_drains_and_keeps_http11_framing() {
    let fs = Arc::new(MemoryFs::empty());
    let server = create_webdav_server(
        fs,
        WebdavServerOptions {
            max_request_bytes: 4,
            ..WebdavServerOptions::default()
        },
    )
    .unwrap();
    server.listen().await.unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    stream
        .write_all(
            b"PUT /too-large-chunked HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
        )
        .await
        .unwrap();
    stream
        .write_all(b"4\r\nabcd\r\n1\r\ne\r\n0\r\n\r\n")
        .await
        .unwrap();
    let (status, body) = read_http_response(&mut stream).await;
    assert_eq!(status, 413);
    assert!(body.is_empty());

    stream
        .write_all(b"OPTIONS * HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let (status, body) = read_http_response(&mut stream).await;
    assert_eq!(status, 200);
    assert!(body.is_empty());
    drop(stream);
    server.close().await.unwrap();
}

#[tokio::test]
async fn streaming_get_finishes_when_server_closes() {
    let fs = Arc::new(MemoryFs::empty());
    let server = create_webdav_server(
        fs,
        WebdavServerOptions {
            session: WebdavSessionOptions {
                read_chunk_bytes: 1024,
                ..WebdavSessionOptions::default()
            },
            ..WebdavServerOptions::default()
        },
    )
    .unwrap();
    server.listen().await.unwrap();
    let client = reqwest::Client::new();
    let base = server.url();
    let mut expected = vec![0_u8; 1024 * 1024];
    for (index, byte) in expected.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(31);
    }
    assert_eq!(
        client
            .put(format!("{base}/large"))
            .body(expected.clone())
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    let response = client.get(format!("{base}/large")).send().await.unwrap();
    let closing = server.close();
    let actual = response.bytes().await.unwrap();
    closing.await.unwrap();
    assert_eq!(actual.as_ref(), expected.as_slice());
    assert_eq!(server.connections(), 0);
}

#[test]
fn unsupported_methods_are_explicit_and_bind_is_loopback_only_without_auth() {
    let error = parse_target_path("not/a/path").unwrap_err();
    assert_eq!(error.status, 400);
    assert!(matches!(error, DavFault { status: 400, .. }));
    assert!(mount_rs_webdav::bind_refusal("0.0.0.0", false).is_some());
    assert!(mount_rs_webdav::bind_refusal("127.0.0.1", false).is_none());
    assert!(mount_rs_webdav::is_loopback_host("::ffff:127.9.9.9"));
    assert!(!mount_rs_webdav::is_loopback_host("::1]"));

    let request = WebdavRequestHead {
        method: "TRACE".to_owned(),
        target: "/".to_owned(),
        headers: Default::default(),
    };
    assert_eq!(request.method, "TRACE");
}
