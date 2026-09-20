use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{FsDriver, MemoryFs};
use mount_rs_s3::{
    CredentialScope, Credentials, EMPTY_PAYLOAD_SHA256, HeaderEntry, PresignRequest, S3BindError,
    S3Request, S3Response, S3Server, S3ServerOptions, S3Session, S3SessionOptions,
    STREAMING_PAYLOAD, SignRequest, canonical_query, format_amz_date, presign_request, sha256_hex,
    sign_chunk, sign_request,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn request(
    method: &str,
    target: &str,
    body: impl AsRef<[u8]>,
    headers: &[(&str, &str)],
) -> S3Request {
    let mut request = S3Request::new(method, target, body.as_ref().to_vec());
    for (name, value) in headers {
        request.head.headers.push(HeaderEntry::new(*name, *value));
    }
    request
}

fn header(response: &S3Response, name: &str) -> Option<String> {
    response
        .headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn xml_field(body: &[u8], name: &str) -> String {
    let body = String::from_utf8_lossy(body);
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open).map(|offset| offset + open.len());
    let end = start.and_then(|offset| body[offset..].find(&close).map(|length| offset + length));
    match (start, end) {
        (Some(start), Some(end)) => body[start..end].to_owned(),
        _ => String::new(),
    }
}

fn signed_chunked_body(
    payload: &[u8],
    credentials: &Credentials,
    scope: &CredentialScope,
    amz_date: &str,
    seed: &str,
) -> Vec<u8> {
    let first = sign_chunk(
        &credentials.secret_access_key,
        scope,
        amz_date,
        seed,
        &sha256_hex(payload),
    );
    let terminal = sign_chunk(
        &credentials.secret_access_key,
        scope,
        amz_date,
        &first,
        &sha256_hex(b""),
    );
    let mut body = format!("{:x};chunk-signature={first}\r\n", payload.len()).into_bytes();
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("0;chunk-signature={terminal}\r\n\r\n").as_bytes());
    body
}

fn authorization_signature(authorization: &str) -> &str {
    authorization
        .rsplit_once("Signature=")
        .map(|(_, signature)| signature)
        .expect("SigV4 authorization signature")
}

#[tokio::test]
async fn oracle_refusal_boundaries_keep_their_protocol_reasons() {
    let session = S3Session::new(MemoryFs::empty());
    let cases = [
        (
            "GET",
            "/mountx",
            "ListObjects (V1) is not implemented by this gateway.",
        ),
        (
            "GET",
            "/mountx?list-type=2&delimiter=.",
            "A delimiter other than \"/\" is not implemented by this gateway.",
        ),
        (
            "GET",
            "/mountx/object?partNumber=1",
            "GetObject for a single part is not implemented by this gateway.",
        ),
        (
            "HEAD",
            "/mountx/object?partNumber=1",
            "HeadObject for a single part is not implemented by this gateway.",
        ),
    ];

    for (method, target, reason) in cases {
        let response = session.handle(request(method, target, [], &[])).await;
        assert_eq!(response.status, 501, "{method} {target}");
        let document = String::from_utf8_lossy(&response.body);
        assert!(document.contains("<Code>NotImplemented</Code>"));
        let encoded_reason = reason.replace('"', "&quot;");
        assert!(
            document.contains(&encoded_reason),
            "{method} {target}: {document}"
        );
    }
}

#[tokio::test]
async fn session_round_trip_lists_ranges_and_conditionals() {
    let driver = MemoryFs::empty();
    let session = S3Session::new(driver.clone());

    let put = session
        .handle(request("PUT", "/mountx/hello.txt", b"hello world", &[]))
        .await;
    assert_eq!(put.status, 200);
    assert!(header(&put, "etag").is_some_and(|value| value.ends_with("-1\"")));
    assert_eq!(header(&put, "content-length").as_deref(), Some("0"));

    let get = session
        .handle(request("GET", "/mountx/hello.txt", [], &[]))
        .await;
    assert_eq!(get.status, 200);
    assert_eq!(get.body, b"hello world");
    assert_eq!(header(&get, "content-length").as_deref(), Some("11"));
    assert_eq!(
        header(&get, "content-type").as_deref(),
        Some("application/octet-stream")
    );

    let etag = header(&get, "etag").expect("GET has an ETag");
    let not_modified = session
        .handle(request(
            "GET",
            "/mountx/hello.txt",
            [],
            &[("if-none-match", &etag)],
        ))
        .await;
    assert_eq!(not_modified.status, 304);
    assert!(not_modified.body.is_empty());

    let range = session
        .handle(request(
            "GET",
            "/mountx/hello.txt",
            [],
            &[("range", "bytes=6-10")],
        ))
        .await;
    assert_eq!(range.status, 206);
    assert_eq!(range.body, b"world");
    assert_eq!(
        header(&range, "content-range").as_deref(),
        Some("bytes 6-10/11")
    );

    for (target, body) in [
        ("/mountx/a.txt", b"one".as_slice()),
        ("/mountx/a/b", b"two".as_slice()),
        ("/mountx/a0", b"three".as_slice()),
        ("/mountx/empty/", b"".as_slice()),
    ] {
        assert_eq!(
            session
                .handle(request("PUT", target, body, &[]))
                .await
                .status,
            200
        );
    }

    let listing = session
        .handle(request("GET", "/mountx?list-type=2", [], &[]))
        .await;
    assert_eq!(listing.status, 200);
    let listing_text = String::from_utf8(listing.body).expect("listing is XML");
    assert!(listing_text.contains("<Key>a.txt</Key>"));
    assert!(listing_text.contains("<Key>a/b</Key>"));
    assert!(listing_text.contains("<Key>a0</Key>"));
    assert!(listing_text.contains("<Key>empty/</Key>"));

    let page = session
        .handle(request(
            "GET",
            "/mountx?list-type=2&delimiter=%2F&max-keys=1",
            [],
            &[],
        ))
        .await;
    assert_eq!(page.status, 200);
    let page_text = String::from_utf8(page.body).expect("listing is XML");
    assert!(page_text.contains("<KeyCount>1</KeyCount>"));
    assert!(page_text.contains("<IsTruncated>true</IsTruncated>"));
    assert!(!page_text.contains(".mountx-multipart"));

    let missing_range = session
        .handle(request(
            "GET",
            "/mountx/hello.txt",
            [],
            &[("range", "bytes=100-")],
        ))
        .await;
    assert_eq!(missing_range.status, 416);
    assert_eq!(
        header(&missing_range, "content-range").as_deref(),
        Some("bytes */11")
    );
}

#[tokio::test]
async fn copy_delete_objects_and_multipart_use_driver_state() {
    let driver = MemoryFs::empty();
    let session = S3Session::new(driver.clone());
    assert_eq!(
        session
            .handle(request("PUT", "/mountx/source.txt", b"source bytes", &[]))
            .await
            .status,
        200
    );

    let copy = session
        .handle({
            let mut request = S3Request::new("PUT", "/mountx/copy.txt", Vec::<u8>::new());
            request
                .head
                .headers
                .push(HeaderEntry::new("x-amz-copy-source", "/mountx/source.txt"));
            request
        })
        .await;
    assert_eq!(copy.status, 200);
    assert!(String::from_utf8_lossy(&copy.body).contains("<CopyObjectResult>"));

    let delete_body = b"<Delete><Object><Key>source.txt</Key></Object><Object><Key>ghost.txt</Key></Object></Delete>";
    let deleted = session
        .handle(request("POST", "/mountx?delete", delete_body, &[]))
        .await;
    assert_eq!(deleted.status, 200);
    let deleted_text = String::from_utf8_lossy(&deleted.body);
    assert!(deleted_text.contains("<Deleted><Key>source.txt</Key></Deleted>"));
    assert!(deleted_text.contains("<Deleted><Key>ghost.txt</Key></Deleted>"));

    let initiated = session
        .handle(request("POST", "/mountx/multipart.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    assert_eq!(upload_id.len(), 32);
    assert!(
        upload_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );

    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/multipart.bin?uploadId={upload_id}&partNumber=1"),
            b"only part",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    let part_etag = header(&part, "etag").expect("part ETag");

    let parts = session
        .handle(request(
            "GET",
            &format!("/mountx/multipart.bin?uploadId={upload_id}"),
            [],
            &[],
        ))
        .await;
    assert_eq!(parts.status, 200);
    assert!(String::from_utf8_lossy(&parts.body).contains("<PartNumber>1</PartNumber>"));

    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
    );
    let completed = session
        .handle(request(
            "POST",
            &format!("/mountx/multipart.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        ))
        .await;
    assert_eq!(completed.status, 200);
    assert_eq!(
        session
            .handle(request("GET", "/mountx/multipart.bin", [], &[]))
            .await
            .body,
        b"only part"
    );
    assert!(
        driver
            .stat(&format!("/.mountx-multipart/{upload_id}"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn http_server_is_rootless_and_sigv4_supports_header_and_presigned_forms() {
    let credentials = Credentials::new("AKIAMOUNTX7GATEWAY9", "test-secret-key");
    let session_options = S3SessionOptions {
        credentials: Some(credentials.clone()),
        region: Some("us-east-1".to_owned()),
        ..S3SessionOptions::default()
    };
    let session = Arc::new(S3Session::new_with_options(
        MemoryFs::empty(),
        session_options,
    ));
    let server = S3Server::start(session, S3ServerOptions::default())
        .await
        .expect("loopback listener");
    let host = server.address().to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as i64;

    let body = b"signed bytes";
    let payload_hash = sha256_hex(body);
    let amz_date = format_amz_date(now);
    let base_headers = vec![
        HeaderEntry::new("host", &host),
        HeaderEntry::new("x-amz-date", &amz_date),
        HeaderEntry::new("x-amz-content-sha256", &payload_hash),
        HeaderEntry::new("content-length", body.len().to_string()),
    ];
    let signed_names = base_headers
        .iter()
        .map(|header| header.name.clone())
        .collect::<Vec<_>>();
    let authorization = sign_request(SignRequest {
        method: "PUT",
        path: "/mountx/signed.txt",
        query: &[],
        headers: &base_headers,
        signed_headers: &signed_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        payload_hash: &payload_hash,
    });
    let mut wire_headers = base_headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    wire_headers.push(("authorization".to_owned(), authorization));
    let stored = wire_request(&server, "PUT", "/mountx/signed.txt", &wire_headers, body).await;
    assert_eq!(stored.status, 200);
    assert_eq!(
        stored.headers.get("content-length").map(String::as_str),
        Some("0")
    );

    let chunked_payload = b"signed aws-chunked bytes";
    let chunked_scope = CredentialScope {
        date: amz_date[..8].to_owned(),
        region: "us-east-1".to_owned(),
        service: "s3".to_owned(),
    };
    let chunked_base = [
        HeaderEntry::new("host", &host),
        HeaderEntry::new("x-amz-date", &amz_date),
        HeaderEntry::new("x-amz-content-sha256", STREAMING_PAYLOAD),
        HeaderEntry::new("content-encoding", "aws-chunked"),
        HeaderEntry::new(
            "x-amz-decoded-content-length",
            chunked_payload.len().to_string(),
        ),
        HeaderEntry::new("content-length", "0"),
    ];
    let chunked_names = chunked_base
        .iter()
        .map(|header| header.name.clone())
        .collect::<Vec<_>>();
    let draft_chunked_body = signed_chunked_body(
        chunked_payload,
        &credentials,
        &chunked_scope,
        &amz_date,
        &"0".repeat(64),
    );
    let chunked_headers = chunked_base
        .iter()
        .map(|header| {
            if header.name.eq_ignore_ascii_case("content-length") {
                HeaderEntry::new("content-length", draft_chunked_body.len().to_string())
            } else {
                header.clone()
            }
        })
        .collect::<Vec<_>>();
    let chunked_authorization = sign_request(SignRequest {
        method: "PUT",
        path: "/mountx/chunked.txt",
        query: &[],
        headers: &chunked_headers,
        signed_headers: &chunked_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        payload_hash: STREAMING_PAYLOAD,
    });
    let chunked_body = signed_chunked_body(
        chunked_payload,
        &credentials,
        &chunked_scope,
        &amz_date,
        authorization_signature(&chunked_authorization),
    );
    assert_eq!(chunked_body.len(), draft_chunked_body.len());
    let mut chunked_wire_headers = chunked_headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    chunked_wire_headers.push(("authorization".to_owned(), chunked_authorization));
    let chunked_stored = wire_request(
        &server,
        "PUT",
        "/mountx/chunked.txt",
        &chunked_wire_headers,
        &chunked_body,
    )
    .await;
    assert_eq!(chunked_stored.status, 200);

    let get_headers = vec![
        HeaderEntry::new("host", &host),
        HeaderEntry::new("x-amz-date", &amz_date),
        HeaderEntry::new("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256),
    ];
    let get_names = get_headers
        .iter()
        .map(|header| header.name.clone())
        .collect::<Vec<_>>();
    let get_authorization = sign_request(SignRequest {
        method: "GET",
        path: "/mountx/signed.txt",
        query: &[],
        headers: &get_headers,
        signed_headers: &get_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        payload_hash: EMPTY_PAYLOAD_SHA256,
    });
    let mut get_wire_headers = get_headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    get_wire_headers.push(("authorization".to_owned(), get_authorization));
    let read = wire_request(&server, "GET", "/mountx/signed.txt", &get_wire_headers, &[]).await;
    assert_eq!(read.status, 200);
    assert_eq!(read.body, body);

    let chunked_get_headers = vec![
        HeaderEntry::new("host", &host),
        HeaderEntry::new("x-amz-date", &amz_date),
        HeaderEntry::new("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256),
    ];
    let chunked_get_names = chunked_get_headers
        .iter()
        .map(|header| header.name.clone())
        .collect::<Vec<_>>();
    let chunked_get_authorization = sign_request(SignRequest {
        method: "GET",
        path: "/mountx/chunked.txt",
        query: &[],
        headers: &chunked_get_headers,
        signed_headers: &chunked_get_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        payload_hash: EMPTY_PAYLOAD_SHA256,
    });
    let mut chunked_get_wire_headers = chunked_get_headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    chunked_get_wire_headers.push(("authorization".to_owned(), chunked_get_authorization));
    let chunked_read = wire_request(
        &server,
        "GET",
        "/mountx/chunked.txt",
        &chunked_get_wire_headers,
        &[],
    )
    .await;
    assert_eq!(chunked_read.status, 200);
    assert_eq!(chunked_read.body, chunked_payload);

    let presigned_headers = [HeaderEntry::new("host", &host)];
    let presigned_names = ["host".to_owned()];
    let presigned = presign_request(PresignRequest {
        method: "GET",
        path: "/mountx/signed.txt",
        query: &[],
        headers: &presigned_headers,
        signed_headers: &presigned_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        expires: 60,
        payload_hash: None,
    })
    .expect("valid presign");
    let target = format!("/mountx/signed.txt?{}", canonical_query(&presigned.query));
    let presigned_read = wire_request(
        &server,
        "GET",
        &target,
        &[("host".to_owned(), host.clone())],
        &[],
    )
    .await;
    assert_eq!(presigned_read.status, 200);
    assert_eq!(presigned_read.body, body);

    let unsigned = wire_request(&server, "GET", "/mountx/signed.txt", &[], &[]).await;
    assert_eq!(unsigned.status, 403);
    assert!(String::from_utf8_lossy(&unsigned.body).contains("<Code>AccessDenied</Code>"));

    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn unauthenticated_server_refuses_non_loopback_bind() {
    let session = Arc::new(S3Session::new(MemoryFs::empty()));
    let result = S3Server::start(
        session,
        S3ServerOptions {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 0,
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(S3BindError::UnauthenticatedNonLoopback { host }) if host == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    ));
}

#[derive(Debug)]
struct WireResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn wire_request(
    server: &S3Server,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> WireResponse {
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let has_host = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"));
    let mut request = format!("{method} {target} HTTP/1.1\r\nConnection: close\r\n");
    if !has_host {
        request.push_str(&format!("Host: {}\r\n", server.address()));
    }
    let has_content_length = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"));
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    if !has_content_length && !body.is_empty() {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request head");
    stream.write_all(body).await.expect("write request body");
    // The request carries a complete Content-Length when it has a body;
    // keeping the write side open avoids making the server treat a half-close
    // as a transport error on platforms with different TCP FIN timing.
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response headers");
    let head = String::from_utf8_lossy(&raw[..separator]);
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .expect("HTTP response status");
    let mut response_headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            response_headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    WireResponse {
        status,
        headers: response_headers,
        body: raw[separator + 4..].to_vec(),
    }
}
