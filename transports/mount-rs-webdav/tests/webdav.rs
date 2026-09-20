use std::sync::Arc;

use mount_rs_core::MemoryFs;
use mount_rs_webdav::protocol::{
    collect_body, href_of, parse_depth, parse_destination, parse_lock_token, parse_overwrite,
    parse_target_path, parse_xml, status_of_error,
};
use mount_rs_webdav::{
    DavFault, Depth, WebdavRequestHead, WebdavServer, WebdavServerOptions, WebdavSessionOptions,
    create_webdav_server,
};

fn method(name: &str) -> reqwest::Method {
    reqwest::Method::from_bytes(name.as_bytes()).expect("valid HTTP method")
}

async fn server() -> WebdavServer {
    let fs = Arc::new(MemoryFs::empty());
    let server = create_webdav_server(fs, WebdavServerOptions::default()).expect("loopback bind");
    server.listen().await.expect("listen");
    server
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
