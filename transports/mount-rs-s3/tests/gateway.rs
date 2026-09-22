use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_core::Stream;
use mount_rs_core::{Capabilities, FileHandle, FsDriver, MemoryFs, Result as FsResult};
use mount_rs_s3::{
    CredentialScope, Credentials, EMPTY_PAYLOAD_SHA256, HeaderEntry, MIN_PART_SIZE, PresignRequest,
    S3BindError, S3ErrorClass, S3Request, S3RequestHead, S3Response, S3Server, S3ServerHooks,
    S3ServerOptions, S3Session, S3SessionHooks, S3SessionOptions, S3TransportErrorKind,
    STREAMING_PAYLOAD, STREAMING_PAYLOAD_TRAILER, STREAMING_UNSIGNED_PAYLOAD_TRAILER, SignRequest,
    canonical_query, format_amz_date, presign_request, sha256_hex, sign_chunk, sign_request,
    sign_trailer,
};
use socket2::SockRef;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

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

fn signed_chunked_body_with_trailer(
    payload: &[u8],
    credentials: &Credentials,
    scope: &CredentialScope,
    amz_date: &str,
    seed: &str,
    trailer_name: &str,
    trailer_value: &str,
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
    let trailer_block = format!("{trailer_name}:{trailer_value}\n");
    let trailer_signature = sign_trailer(
        &credentials.secret_access_key,
        scope,
        amz_date,
        &terminal,
        &sha256_hex(&trailer_block),
    );
    let mut body = format!("{:x};chunk-signature={first}\r\n", payload.len()).into_bytes();
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "0;chunk-signature={terminal}\r\n{trailer_name}:{trailer_value}\r\nx-amz-trailer-signature:{trailer_signature}\r\n\r\n"
        )
        .as_bytes(),
    );
    body
}

fn unsigned_chunked_body_with_trailer(
    payload: &[u8],
    trailer_name: &str,
    trailer_value: &str,
    final_crlf: bool,
) -> Vec<u8> {
    let mut body = format!("{:x}\r\n", payload.len()).into_bytes();
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\r\n0\r\n");
    body.extend_from_slice(format!("{trailer_name}:{trailer_value}\r\n").as_bytes());
    if final_crlf {
        body.extend_from_slice(b"\r\n");
    }
    body
}

fn authorization_signature(authorization: &str) -> &str {
    authorization
        .rsplit_once("Signature=")
        .map(|(_, signature)| signature)
        .expect("SigV4 authorization signature")
}

struct PendingBody {
    first: Option<Vec<u8>>,
    first_polled: Arc<Notify>,
}

impl Stream for PendingBody {
    type Item = Result<Vec<u8>, String>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(first) = self.first.take() {
            self.first_polled.notify_one();
            Poll::Ready(Some(Ok(first)))
        } else {
            Poll::Pending
        }
    }
}

async fn wait_for_root_staging(driver: &MemoryFs) {
    timeout(Duration::from_secs(1), async {
        loop {
            if driver
                .readdir("/")
                .await
                .expect("root directory")
                .iter()
                .any(|entry| entry.name.starts_with(".mountx-put-"))
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("streaming PUT created private staging");
}

async fn wait_for_no_root_staging(driver: &MemoryFs) {
    timeout(Duration::from_secs(1), async {
        loop {
            if !driver
                .readdir("/")
                .await
                .expect("root directory")
                .iter()
                .any(|entry| entry.name.starts_with(".mountx-put-"))
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled streaming PUT removed private staging");
}

async fn wait_for_no_part_staging(driver: &MemoryFs, upload_id: &str) {
    let directory = format!("/.mountx-multipart/{upload_id}");
    timeout(Duration::from_secs(1), async {
        loop {
            let has_staging = driver
                .readdir(&directory)
                .await
                .map(|entries| entries.iter().any(|entry| entry.name.starts_with(".part-")))
                .unwrap_or(false);
            if !has_staging {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled multipart part removed private staging");
}

async fn wait_for_part_staging(driver: &MemoryFs, upload_id: &str) {
    let directory = format!("/.mountx-multipart/{upload_id}");
    timeout(Duration::from_secs(1), async {
        loop {
            if driver
                .readdir(&directory)
                .await
                .expect("multipart directory")
                .iter()
                .any(|entry| entry.name.starts_with(".part-"))
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("streaming multipart overwrite created private staging");
}

#[tokio::test]
async fn session_stats_capture_latency_bytes_and_bounded_error_classes() {
    let session = S3Session::new(MemoryFs::empty());

    assert_eq!(
        session
            .handle(request("PUT", "/mountx/stats.txt", b"metrics", &[]))
            .await
            .status,
        200
    );
    assert_eq!(
        session
            .handle(request("GET", "/mountx/stats.txt", [], &[]))
            .await
            .status,
        200
    );
    assert_eq!(
        session
            .handle(request("GET", "/mountx/missing.txt", [], &[]))
            .await
            .status,
        404
    );

    let stats = session.stats().await;
    assert_eq!(stats.requests, 3);
    assert_eq!(stats.replies, 3);
    assert_eq!(stats.errors, 1);
    assert!(stats.duration_ms_total >= stats.duration_ms_max);
    assert!(stats.request_bytes >= b"metrics".len() as u64);
    assert!(stats.response_bytes >= b"metrics".len() as u64);
    assert_eq!(stats.operations.get("PutObject"), Some(&1));
    assert_eq!(stats.operations.get("GetObject"), Some(&2));
    assert_eq!(stats.error_classes.get(&S3ErrorClass::Client), Some(&1));
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

    for method in ["PUT", "DELETE"] {
        let response = session.handle(request(method, "/mountx", [], &[])).await;
        assert_eq!(response.status, 501, "{method} bucket root");
        let document = String::from_utf8_lossy(&response.body);
        assert!(document.contains("<Code>NotImplemented</Code>"));
        assert!(
            document
                .contains("A header you provided implies functionality that is not implemented.")
        );
    }

    let response = session
        .handle(request("PATCH", "/mountx/object", [], &[]))
        .await;
    assert_eq!(response.status, 405);
    let document = String::from_utf8_lossy(&response.body);
    assert!(document.contains("<Code>MethodNotAllowed</Code>"));
    assert!(document.contains("The method PATCH is not allowed against this resource."));
    assert!(document.contains("<Resource>/mountx/object</Resource>"));
}

#[tokio::test]
async fn unsigned_checksum_trailers_match_oracle_framing() {
    let session = S3Session::new(MemoryFs::empty());
    let payload = b"checksum trailer payload";
    for (key, final_crlf) in [("with-final-crlf", true), ("without-final-crlf", false)] {
        let body = unsigned_chunked_body_with_trailer(
            payload,
            "x-amz-checksum-crc32",
            "1B2M2Y8=",
            final_crlf,
        );
        let mut upload = S3Request::new("PUT", format!("/mountx/{key}"), body);
        upload.head.headers.extend([
            HeaderEntry::new("content-encoding", "aws-chunked"),
            HeaderEntry::new("x-amz-content-sha256", STREAMING_UNSIGNED_PAYLOAD_TRAILER),
            HeaderEntry::new("x-amz-trailer", "x-amz-checksum-crc32"),
            HeaderEntry::new("x-amz-decoded-content-length", payload.len().to_string()),
        ]);
        let response = session.handle(upload).await;
        assert_eq!(response.status, 200, "{key}");

        let read = session
            .handle(request("GET", &format!("/mountx/{key}"), [], &[]))
            .await;
        assert_eq!(read.status, 200, "{key}");
        assert_eq!(read.body, payload, "{key}");
    }
}

#[tokio::test]
async fn chunked_lengths_and_trailer_declarations_follow_oracle_refusals() {
    let session = S3Session::new(MemoryFs::empty());
    let payload = b"framed without a wire length";
    let body =
        unsigned_chunked_body_with_trailer(payload, "x-amz-checksum-crc32", "1B2M2Y8=", false);
    let mut no_wire_length = S3Request::new("PUT", "/mountx/no-wire-length", body);
    no_wire_length
        .head
        .headers
        .retain(|header| !header.name.eq_ignore_ascii_case("content-length"));
    no_wire_length.head.headers.extend([
        HeaderEntry::new("content-encoding", "aws-chunked"),
        HeaderEntry::new("x-amz-content-sha256", STREAMING_UNSIGNED_PAYLOAD_TRAILER),
        HeaderEntry::new("x-amz-trailer", "x-amz-checksum-crc32"),
        HeaderEntry::new("x-amz-decoded-content-length", payload.len().to_string()),
    ]);
    assert_eq!(session.handle(no_wire_length).await.status, 200);

    let undeclared =
        unsigned_chunked_body_with_trailer(payload, "x-amz-checksum-sha256", "bogus", true);
    let mut request = S3Request::new("PUT", "/mountx/undeclared", undeclared);
    request.head.headers.extend([
        HeaderEntry::new("content-encoding", "aws-chunked"),
        HeaderEntry::new("x-amz-content-sha256", STREAMING_UNSIGNED_PAYLOAD_TRAILER),
        HeaderEntry::new("x-amz-trailer", "x-amz-checksum-crc32"),
        HeaderEntry::new("x-amz-decoded-content-length", payload.len().to_string()),
    ]);
    let response = session.handle(request).await;
    assert_eq!(response.status, 400);
    assert!(String::from_utf8_lossy(&response.body).contains("<Code>InvalidRequest</Code>"));

    let missing = format!(
        "{:x}\r\n{}\r\n0\r\n",
        payload.len(),
        String::from_utf8_lossy(payload)
    )
    .into_bytes();
    let mut request = S3Request::new("PUT", "/mountx/missing-trailer", missing);
    request.head.headers.extend([
        HeaderEntry::new("content-encoding", "aws-chunked"),
        HeaderEntry::new("x-amz-content-sha256", STREAMING_UNSIGNED_PAYLOAD_TRAILER),
        HeaderEntry::new("x-amz-trailer", "x-amz-checksum-crc32"),
        HeaderEntry::new("x-amz-decoded-content-length", payload.len().to_string()),
    ]);
    let response = session.handle(request).await;
    assert_eq!(response.status, 400);
    assert!(String::from_utf8_lossy(&response.body).contains("<Code>InvalidRequest</Code>"));
}

#[tokio::test]
async fn signed_checksum_trailer_chain_is_verified() {
    let credentials = Credentials::new("AKIAMOUNTX7TRAILER", "test-secret-key");
    let session = S3Session::new_with_options(
        MemoryFs::empty(),
        S3SessionOptions {
            credentials: Some(credentials.clone()),
            region: Some("us-east-1".to_owned()),
            ..S3SessionOptions::default()
        },
    );
    let payload = b"signed checksum trailer payload";
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as i64;
    let amz_date = format_amz_date(now);
    let scope = CredentialScope {
        date: amz_date[..8].to_owned(),
        region: "us-east-1".to_owned(),
        service: "s3".to_owned(),
    };
    let draft_headers = [
        HeaderEntry::new("host", "localhost"),
        HeaderEntry::new("x-amz-date", &amz_date),
        HeaderEntry::new("x-amz-content-sha256", STREAMING_PAYLOAD_TRAILER),
        HeaderEntry::new("content-encoding", "aws-chunked"),
        HeaderEntry::new("x-amz-trailer", "x-amz-checksum-crc32"),
        HeaderEntry::new("x-amz-decoded-content-length", payload.len().to_string()),
        HeaderEntry::new("content-length", "0"),
    ];
    let signed_names = draft_headers
        .iter()
        .map(|header| header.name.clone())
        .collect::<Vec<_>>();
    let draft_body = signed_chunked_body_with_trailer(
        payload,
        &credentials,
        &scope,
        &amz_date,
        &"0".repeat(64),
        "x-amz-checksum-crc32",
        "1B2M2Y8=",
    );
    let headers = draft_headers
        .iter()
        .map(|header| {
            if header.name.eq_ignore_ascii_case("content-length") {
                HeaderEntry::new("content-length", draft_body.len().to_string())
            } else {
                header.clone()
            }
        })
        .collect::<Vec<_>>();
    let authorization = sign_request(SignRequest {
        method: "PUT",
        path: "/mountx/signed-checksum.txt",
        query: &[],
        headers: &headers,
        signed_headers: &signed_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        payload_hash: STREAMING_PAYLOAD_TRAILER,
    });
    let body = signed_chunked_body_with_trailer(
        payload,
        &credentials,
        &scope,
        &amz_date,
        authorization_signature(&authorization),
        "x-amz-checksum-crc32",
        "1B2M2Y8=",
    );
    assert_eq!(body.len(), draft_body.len());
    let mut tampered_body = body.clone();
    let checksum_offset = tampered_body
        .windows(b"1B2M2Y8=".len())
        .position(|window| window == b"1B2M2Y8=")
        .expect("checksum trailer");
    tampered_body[checksum_offset + 6] = b'9';
    let mut upload = S3Request::new("PUT", "/mountx/signed-checksum.txt", body.clone());
    upload.head.headers = headers.clone();
    upload
        .head
        .headers
        .push(HeaderEntry::new("authorization", authorization.clone()));
    let stored = session.handle(upload).await;
    assert_eq!(stored.status, 200);

    let mut tampered = S3Request::new("PUT", "/mountx/signed-checksum.txt", tampered_body);
    tampered.head.headers = headers.clone();
    tampered
        .head
        .headers
        .push(HeaderEntry::new("authorization", authorization.clone()));
    let rejected = session.handle(tampered).await;
    assert_eq!(rejected.status, 403);
    assert!(String::from_utf8_lossy(&rejected.body).contains("<Code>SignatureDoesNotMatch</Code>"));

    let mut bad_trailer_signature = body.clone();
    let signature_marker = b"x-amz-trailer-signature:";
    let signature_offset = bad_trailer_signature
        .windows(signature_marker.len())
        .position(|window| window == signature_marker)
        .map(|offset| offset + signature_marker.len())
        .expect("trailer signature");
    bad_trailer_signature[signature_offset] = if bad_trailer_signature[signature_offset] == b'0' {
        b'1'
    } else {
        b'0'
    };
    let mut tampered_signature =
        S3Request::new("PUT", "/mountx/signed-checksum.txt", bad_trailer_signature);
    tampered_signature.head.headers = headers;
    tampered_signature
        .head
        .headers
        .push(HeaderEntry::new("authorization", authorization));
    let rejected = session.handle(tampered_signature).await;
    assert_eq!(rejected.status, 403);
    assert!(String::from_utf8_lossy(&rejected.body).contains("<Code>SignatureDoesNotMatch</Code>"));

    let mut get_headers = vec![
        HeaderEntry::new("host", "localhost"),
        HeaderEntry::new("x-amz-date", &amz_date),
        HeaderEntry::new("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256),
    ];
    let get_names = get_headers
        .iter()
        .map(|header| header.name.clone())
        .collect::<Vec<_>>();
    let get_authorization = sign_request(SignRequest {
        method: "GET",
        path: "/mountx/signed-checksum.txt",
        query: &[],
        headers: &get_headers,
        signed_headers: &get_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        payload_hash: EMPTY_PAYLOAD_SHA256,
    });
    get_headers.push(HeaderEntry::new("authorization", get_authorization));
    let read = session
        .handle({
            let mut request = S3Request::new("GET", "/mountx/signed-checksum.txt", Vec::new());
            request.head.headers = get_headers;
            request
        })
        .await;
    assert_eq!(read.status, 200);
    assert_eq!(read.body, payload);
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
    assert_eq!(
        header(&not_modified, "etag").as_deref(),
        Some(etag.as_str())
    );
    assert!(header(&not_modified, "last-modified").is_some());
    assert!(header(&not_modified, "x-amz-meta-mtime").is_some());

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
    let token = xml_field(page_text.as_bytes(), "NextContinuationToken");
    assert!(!token.is_empty());
    let next_page = session
        .handle(request(
            "GET",
            &format!("/mountx?list-type=2&delimiter=%2F&max-keys=1&continuation-token={token}"),
            [],
            &[],
        ))
        .await;
    assert_eq!(next_page.status, 200);
    let next_page_text = String::from_utf8(next_page.body).expect("listing is XML");
    assert!(next_page_text.contains("<KeyCount>1</KeyCount>"));
    assert!(next_page_text.contains("<IsTruncated>true</IsTruncated>"));
    assert!(next_page_text.contains("<CommonPrefixes><Prefix>a/</Prefix>"));

    let empty_page = session
        .handle(request("GET", "/mountx?list-type=2&max-keys=0", [], &[]))
        .await;
    assert_eq!(empty_page.status, 200);
    let empty_page_text = String::from_utf8(empty_page.body).expect("listing is XML");
    assert!(empty_page_text.contains("<KeyCount>0</KeyCount>"));
    assert!(empty_page_text.contains("<IsTruncated>false</IsTruncated>"));

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
    let copy_text = String::from_utf8_lossy(&copy.body);
    assert!(
        copy_text.contains(r#"<CopyObjectResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#)
    );
    assert!(copy_text.contains("<LastModified>"));
    assert!(copy_text.contains("<ETag>"));
    assert!(copy_text.contains("</ETag>"));

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
    let parts_text = String::from_utf8_lossy(&parts.body);
    assert!(parts_text.contains("<PartNumber>1</PartNumber>"));
    assert!(parts_text.contains(
        "<PartNumberMarker>0</PartNumberMarker><MaxParts>1000</MaxParts><IsTruncated>false</IsTruncated>"
    ));

    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
    );
    let mut complete_request = request(
        "POST",
        &format!("/mountx/multipart.bin?uploadId={upload_id}"),
        complete_body.as_bytes(),
        &[],
    );
    complete_request
        .head
        .headers
        .push(HeaderEntry::new("host", "s3.example"));
    let completed = session.handle(complete_request).await;
    assert_eq!(completed.status, 200);
    let completed_text = String::from_utf8_lossy(&completed.body);
    assert!(completed_text.contains("<Location>http://s3.example/mountx/multipart.bin</Location>"));
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
    let after = session
        .handle(request(
            "GET",
            &format!("/mountx/multipart.bin?uploadId={upload_id}"),
            [],
            &[],
        ))
        .await;
    assert_eq!(after.status, 404);
    let after_text = String::from_utf8_lossy(&after.body);
    assert!(after_text.contains("<Code>NoSuchUpload</Code>"));
    assert!(after_text.contains(
        "The specified multipart upload does not exist. The upload ID might be invalid, or the multipart upload might have been aborted or completed."
    ));
}

#[tokio::test]
async fn concurrent_multipart_parts_publish_in_numeric_order_and_complete_atomically() {
    let driver = MemoryFs::empty();
    let session = Arc::new(S3Session::new(driver));
    let initiated = session
        .handle(request("POST", "/mountx/concurrent.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let first_body = vec![b'a'; MIN_PART_SIZE as usize];
    let second_body = vec![b'b'; MIN_PART_SIZE as usize];
    let third_body = vec![b'c'; MIN_PART_SIZE as usize];
    let fourth_body = b"tail".to_vec();
    let (first, second, third, fourth) = tokio::join!(
        session.handle(request(
            "PUT",
            &format!("/mountx/concurrent.bin?uploadId={upload_id}&partNumber=1"),
            &first_body,
            &[],
        )),
        session.handle(request(
            "PUT",
            &format!("/mountx/concurrent.bin?uploadId={upload_id}&partNumber=2"),
            &second_body,
            &[],
        )),
        session.handle(request(
            "PUT",
            &format!("/mountx/concurrent.bin?uploadId={upload_id}&partNumber=3"),
            &third_body,
            &[],
        )),
        session.handle(request(
            "PUT",
            &format!("/mountx/concurrent.bin?uploadId={upload_id}&partNumber=4"),
            &fourth_body,
            &[],
        )),
    );
    assert_eq!(first.status, 200);
    assert_eq!(second.status, 200);
    assert_eq!(third.status, 200);
    assert_eq!(fourth.status, 200);
    let first_etag = header(&first, "etag").expect("first part ETag");
    let second_etag = header(&second, "etag").expect("second part ETag");
    let third_etag = header(&third, "etag").expect("third part ETag");
    let fourth_etag = header(&fourth, "etag").expect("fourth part ETag");
    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{first_etag}</ETag></Part><Part><PartNumber>2</PartNumber><ETag>{second_etag}</ETag></Part><Part><PartNumber>3</PartNumber><ETag>{third_etag}</ETag></Part><Part><PartNumber>4</PartNumber><ETag>{fourth_etag}</ETag></Part></CompleteMultipartUpload>"
    );
    let completed = session
        .handle(request(
            "POST",
            &format!("/mountx/concurrent.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        ))
        .await;
    assert_eq!(completed.status, 200);

    let object = session
        .handle(request("GET", "/mountx/concurrent.bin", [], &[]))
        .await;
    assert_eq!(object.status, 200);
    let mut expected = first_body;
    expected.extend_from_slice(&second_body);
    expected.extend_from_slice(&third_body);
    expected.extend_from_slice(&fourth_body);
    assert_eq!(object.body, expected);
}

#[tokio::test]
async fn multipart_uploads_survive_session_replacement_and_complete_from_disk() {
    let driver = MemoryFs::empty();
    let first = S3Session::new(driver.clone());
    let initiated = first
        .handle(request("POST", "/mountx/restart.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let part = first
        .handle(request(
            "PUT",
            &format!("/mountx/restart.bin?uploadId={upload_id}&partNumber=1"),
            b"durable staged bytes",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    let part_etag = header(&part, "etag").expect("part ETag");

    // A replacement session has no in-memory upload registry to consult; the
    // manifest and part are the persistent hand-off between the two instances.
    let restarted = S3Session::new(driver.clone());
    let listed = restarted
        .handle(request(
            "GET",
            &format!("/mountx/restart.bin?uploadId={upload_id}"),
            [],
            &[],
        ))
        .await;
    assert_eq!(listed.status, 200);
    assert!(String::from_utf8_lossy(&listed.body).contains("<PartNumber>1</PartNumber>"));

    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
    );
    let completed = restarted
        .handle(request(
            "POST",
            &format!("/mountx/restart.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        ))
        .await;
    assert_eq!(completed.status, 200);
    let object = restarted
        .handle(request("GET", "/mountx/restart.bin", [], &[]))
        .await;
    assert_eq!(object.status, 200);
    assert_eq!(object.body, b"durable staged bytes");
    assert!(
        driver
            .stat(&format!("/.mountx-multipart/{upload_id}"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn invalid_multipart_complete_releases_finalization_claim_for_retry() {
    let driver = MemoryFs::empty();
    let session = S3Session::new(driver.clone());
    let initiated = session
        .handle(request("POST", "/mountx/retry.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/retry.bin?uploadId={upload_id}&partNumber=1"),
            b"retryable staged bytes",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    let part_etag = header(&part, "etag").expect("part ETag");

    let invalid = session
        .handle(request(
            "POST",
            &format!("/mountx/retry.bin?uploadId={upload_id}"),
            b"<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>wrong</ETag></Part></CompleteMultipartUpload>",
            &[],
        ))
        .await;
    assert_eq!(invalid.status, 400);
    assert!(String::from_utf8_lossy(&invalid.body).contains("<Code>InvalidPart</Code>"));
    assert!(
        driver
            .stat(&format!("/.mountx-multipart/{upload_id}/.finalizing"))
            .await
            .is_err()
    );

    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
    );
    let completed = session
        .handle(request(
            "POST",
            &format!("/mountx/retry.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        ))
        .await;
    assert_eq!(completed.status, 200);
    let object = session
        .handle(request("GET", "/mountx/retry.bin", [], &[]))
        .await;
    assert_eq!(object.status, 200);
    assert_eq!(object.body, b"retryable staged bytes");
    assert!(
        driver
            .stat(&format!("/.mountx-multipart/{upload_id}"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_streaming_put_removes_private_staging_and_request_ticket() {
    let driver = MemoryFs::empty();
    let session = Arc::new(S3Session::new(driver.clone()));
    let first_polled = Arc::new(Notify::new());
    let first_notification = first_polled.notified();
    let body_notification = Arc::clone(&first_polled);
    let request = S3RequestHead::new("PUT", "/mountx/cancelled-stream.txt")
        .with_header("content-length", "7");
    let task = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .handle_request_stream(
                    request,
                    Box::pin(PendingBody {
                        first: Some(b"partial".to_vec()),
                        first_polled: body_notification,
                    }),
                )
                .await
        }
    });

    first_notification.await;
    wait_for_root_staging(&driver).await;
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    wait_for_no_root_staging(&driver).await;

    assert!(driver.stat("/cancelled-stream.txt").await.is_err());
    assert!(session.assertions().is_empty());
}

#[tokio::test]
async fn cancelled_streaming_part_preserves_existing_part_and_staging_budget() {
    let driver = MemoryFs::empty();
    let session = Arc::new(S3Session::new(driver.clone()));
    let initiated = session
        .handle(request("POST", "/mountx/atomic-part.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let original = session
        .handle(request(
            "PUT",
            &format!("/mountx/atomic-part.bin?uploadId={upload_id}&partNumber=1"),
            b"original part",
            &[],
        ))
        .await;
    assert_eq!(original.status, 200);

    let first_polled = Arc::new(Notify::new());
    let first_notification = first_polled.notified();
    let body_notification = Arc::clone(&first_polled);
    let request = S3RequestHead::new(
        "PUT",
        format!("/mountx/atomic-part.bin?uploadId={upload_id}&partNumber=1"),
    )
    .with_header("content-length", "partial replacement".len().to_string());
    let task = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .handle_request_stream(
                    request,
                    Box::pin(PendingBody {
                        first: Some(b"partial replacement".to_vec()),
                        first_polled: body_notification,
                    }),
                )
                .await
        }
    });

    first_notification.await;
    wait_for_part_staging(&driver, &upload_id).await;
    let has_part_staging = driver
        .readdir(&format!("/.mountx-multipart/{upload_id}"))
        .await
        .expect("multipart directory")
        .iter()
        .any(|entry| entry.name.starts_with(".part-"));
    assert!(
        has_part_staging,
        "streaming overwrite reached private staging"
    );
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    wait_for_no_part_staging(&driver, &upload_id).await;

    let path = format!("/.mountx-multipart/{upload_id}/part-1");
    let stats = driver.stat(&path).await.expect("original part remains");
    let handle = driver
        .open(&path, "r", 0)
        .await
        .expect("open original part");
    let mut bytes = vec![0_u8; stats.size as usize];
    let count = handle
        .read(&mut bytes, Some(0))
        .await
        .expect("read original part");
    handle.close().await.expect("close original part");
    bytes.truncate(count);
    assert_eq!(bytes, b"original part");
    assert!(session.assertions().is_empty());
}

#[tokio::test]
async fn delete_objects_reports_per_key_driver_errors_to_error_hook() {
    let driver = FaultOnReadFs {
        inner: MemoryFs::empty(),
        fail_next_read: Arc::new(AtomicBool::new(false)),
        fail_unlink: Arc::new(AtomicBool::new(false)),
        durable_writes: false,
        sync_calls: Arc::new(AtomicUsize::new(0)),
        fail_syncfs: Arc::new(AtomicBool::new(false)),
    };
    let reports = Arc::new(StdMutex::new(Vec::<(String, String, String)>::new()));
    let observed = Arc::clone(&reports);
    let session = S3Session::new_with_options(
        driver.clone(),
        S3SessionOptions {
            hooks: S3SessionHooks {
                on_error: Some(Arc::new(move |message, head| {
                    let head = head.expect("per-key driver error head");
                    observed.lock().expect("error reports lock").push((
                        message,
                        head.method,
                        head.target,
                    ));
                })),
                ..S3SessionHooks::default()
            },
            ..S3SessionOptions::default()
        },
    );

    let seeded = session
        .handle(request("PUT", "/mountx/fault.txt", b"retained", &[]))
        .await;
    assert_eq!(seeded.status, 200);
    driver.fail_unlink.store(true, Ordering::SeqCst);

    let deleted = session
        .handle(request(
            "POST",
            "/mountx?delete",
            b"<Delete><Object><Key>fault.txt</Key></Object></Delete>",
            &[],
        ))
        .await;
    assert_eq!(deleted.status, 200);
    assert!(String::from_utf8_lossy(&deleted.body).contains("<Key>fault.txt</Key>"));

    let reports = reports.lock().expect("error reports lock");
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].0.is_empty());
    assert_eq!(reports[0].1, "POST");
    assert_eq!(reports[0].2, "/mountx?delete");
}

#[tokio::test]
async fn session_close_reports_cleanup_errors_without_rejecting() {
    let driver = FaultOnReadFs {
        inner: MemoryFs::empty(),
        fail_next_read: Arc::new(AtomicBool::new(false)),
        fail_unlink: Arc::new(AtomicBool::new(false)),
        durable_writes: false,
        sync_calls: Arc::new(AtomicUsize::new(0)),
        fail_syncfs: Arc::new(AtomicBool::new(false)),
    };
    let reports = Arc::new(StdMutex::new(Vec::<(String, Option<S3RequestHead>)>::new()));
    let observed = Arc::clone(&reports);
    let session = S3Session::new_with_options(
        driver.clone(),
        S3SessionOptions {
            hooks: S3SessionHooks {
                on_error: Some(Arc::new(move |message, head| {
                    observed
                        .lock()
                        .expect("close error reports lock")
                        .push((message, head));
                })),
                ..S3SessionHooks::default()
            },
            ..S3SessionOptions::default()
        },
    );
    let initiated = session
        .handle(request("POST", "/mountx/close.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/close.bin?uploadId={upload_id}&partNumber=1"),
            b"close fault",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);

    driver.fail_unlink.store(true, Ordering::SeqCst);
    session
        .close()
        .await
        .expect("close reports cleanup errors without rejecting");

    let reports = reports.lock().expect("close error reports lock");
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].0.is_empty());
    assert!(reports[0].1.is_none());
}

#[derive(Clone)]
struct FaultOnReadFs {
    inner: MemoryFs,
    fail_next_read: Arc<AtomicBool>,
    fail_unlink: Arc<AtomicBool>,
    durable_writes: bool,
    sync_calls: Arc<AtomicUsize>,
    fail_syncfs: Arc<AtomicBool>,
}

struct FaultOnReadHandle {
    inner: Arc<dyn FileHandle>,
    fail_next_read: Arc<AtomicBool>,
}

#[async_trait]
impl FileHandle for FaultOnReadHandle {
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> FsResult<usize> {
        if self.fail_next_read.swap(false, Ordering::SeqCst) {
            return Err(
                mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio).with_syscall("read")
            );
        }
        self.inner.read(buffer, position).await
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> FsResult<usize> {
        self.inner.write(buffer, position).await
    }

    async fn stat(&self) -> FsResult<mount_rs_core::Stats> {
        self.inner.stat().await
    }

    async fn truncate(&self, length: u64) -> FsResult<()> {
        self.inner.truncate(length).await
    }

    async fn close(&self) -> FsResult<()> {
        self.inner.close().await
    }
}

#[async_trait]
impl FsDriver for FaultOnReadFs {
    fn capabilities(&self) -> Capabilities {
        let mut capabilities = self.inner.capabilities();
        capabilities.durable_writes = self.durable_writes;
        capabilities
    }

    async fn syncfs(&self) -> FsResult<()> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_syncfs.swap(false, Ordering::SeqCst) {
            return Err(
                mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio).with_syscall("syncfs")
            );
        }
        Ok(())
    }

    async fn stat(&self, path: &str) -> FsResult<mount_rs_core::Stats> {
        self.inner.stat(path).await
    }

    async fn readdir(&self, path: &str) -> FsResult<Vec<mount_rs_core::DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> FsResult<Arc<dyn FileHandle>> {
        let inner = self.inner.open(path, flags, mode).await?;
        Ok(Arc::new(FaultOnReadHandle {
            inner,
            fail_next_read: Arc::clone(&self.fail_next_read),
        }))
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> FsResult<()> {
        self.inner.rename(old_path, new_path).await
    }

    async fn mkdir(
        &self,
        path: &str,
        options: mount_rs_core::MkdirOptions,
    ) -> FsResult<Option<String>> {
        self.inner.mkdir(path, options).await
    }

    async fn rmdir(&self, path: &str) -> FsResult<()> {
        self.inner.rmdir(path).await
    }

    async fn unlink(&self, path: &str) -> FsResult<()> {
        if self.fail_unlink.swap(false, Ordering::SeqCst) {
            return Err(
                mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio).with_syscall("unlink")
            );
        }
        self.inner.unlink(path).await
    }
}

#[tokio::test]
async fn failed_multipart_assembly_releases_finalization_claim_for_retry() {
    let driver = FaultOnReadFs {
        inner: MemoryFs::empty(),
        fail_next_read: Arc::new(AtomicBool::new(false)),
        fail_unlink: Arc::new(AtomicBool::new(false)),
        durable_writes: false,
        sync_calls: Arc::new(AtomicUsize::new(0)),
        fail_syncfs: Arc::new(AtomicBool::new(false)),
    };
    let session = S3Session::new(driver.clone());
    let initiated = session
        .handle(request("POST", "/mountx/fault.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/fault.bin?uploadId={upload_id}&partNumber=1"),
            b"fault-retry bytes",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    let part_etag = header(&part, "etag").expect("part ETag");
    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
    );

    driver.fail_next_read.store(true, Ordering::SeqCst);
    let failed = session
        .handle(request(
            "POST",
            &format!("/mountx/fault.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        ))
        .await;
    assert_eq!(failed.status, 500);
    assert!(
        driver
            .inner
            .stat(&format!("/.mountx-multipart/{upload_id}/.finalizing"))
            .await
            .is_err()
    );

    let completed = session
        .handle(request(
            "POST",
            &format!("/mountx/fault.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        ))
        .await;
    assert_eq!(completed.status, 200);
    let object = session
        .handle(request("GET", "/mountx/fault.bin", [], &[]))
        .await;
    assert_eq!(object.status, 200);
    assert_eq!(object.body, b"fault-retry bytes");
}

#[tokio::test]
async fn durable_mutations_wait_for_and_report_syncfs_barriers() {
    let driver = FaultOnReadFs {
        inner: MemoryFs::empty(),
        fail_next_read: Arc::new(AtomicBool::new(false)),
        fail_unlink: Arc::new(AtomicBool::new(false)),
        durable_writes: true,
        sync_calls: Arc::new(AtomicUsize::new(0)),
        fail_syncfs: Arc::new(AtomicBool::new(false)),
    };
    let session = S3Session::new(driver.clone());

    let put = session
        .handle(request("PUT", "/mountx/durable.txt", b"durable", &[]))
        .await;
    assert_eq!(put.status, 200);
    assert_eq!(driver.sync_calls.load(Ordering::SeqCst), 1);

    driver.fail_syncfs.store(true, Ordering::SeqCst);
    let failed = session
        .handle(request("PUT", "/mountx/retry.txt", b"retry", &[]))
        .await;
    assert_eq!(failed.status, 500);
    assert_eq!(driver.sync_calls.load(Ordering::SeqCst), 2);

    let retried = session
        .handle(request("PUT", "/mountx/retry.txt", b"retry", &[]))
        .await;
    assert_eq!(retried.status, 200);
    assert_eq!(driver.sync_calls.load(Ordering::SeqCst), 3);

    let initiated = session
        .handle(request("POST", "/mountx/multipart.txt?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    assert_eq!(driver.sync_calls.load(Ordering::SeqCst), 4);

    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/multipart.txt?uploadId={upload_id}&partNumber=1"),
            b"part",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    assert_eq!(driver.sync_calls.load(Ordering::SeqCst), 5);
    let part_etag = header(&part, "etag").expect("part ETag");

    let complete = session
        .handle(request(
            "POST",
            &format!("/mountx/multipart.txt?uploadId={upload_id}"),
            format!(
                "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
            ),
            &[],
        ))
        .await;
    assert_eq!(complete.status, 200);
    assert_eq!(driver.sync_calls.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn multipart_completion_keeps_destination_inode_before_finalization_marker() {
    let driver = MemoryFs::empty();
    let session = S3Session::new(driver.clone());
    let initiated = session
        .handle(request("POST", "/mountx/nested/etag.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/nested/etag.bin?uploadId={upload_id}&partNumber=1"),
            b"stable multipart bytes",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    let part_etag = header(&part, "etag").expect("part ETag");

    let probe_path = format!("/.mountx-multipart/{upload_id}/allocation-probe");
    let probe = driver
        .open(&probe_path, "wx", 0o600)
        .await
        .expect("allocation probe opens");
    probe.close().await.expect("allocation probe closes");
    let probe_stats = driver
        .stat(&probe_path)
        .await
        .expect("allocation probe stats");
    driver
        .unlink(&probe_path)
        .await
        .expect("allocation probe removes");

    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
    );
    let completed = session
        .handle(request(
            "POST",
            &format!("/mountx/nested/etag.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        ))
        .await;
    assert_eq!(completed.status, 200);

    let object = driver
        .stat("/nested/etag.bin")
        .await
        .expect("completed object stats");
    // The parent directory is allocated before the reserved completion file;
    // the marker then consumes the following inode without shifting the
    // published object's identity.
    assert_eq!(object.ino, probe_stats.ino + 2);
}

#[tokio::test]
async fn multipart_complete_and_abort_race_has_one_terminal_winner() {
    let driver = MemoryFs::empty();
    let session = Arc::new(S3Session::new(driver.clone()));
    let initiated = session
        .handle(request("POST", "/mountx/race.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let upload_id = xml_field(&initiated.body, "UploadId");
    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/race.bin?uploadId={upload_id}&partNumber=1"),
            b"the whole object in one part",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    let part_etag = header(&part, "etag").expect("part ETag");
    let complete_body = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
    );

    let (completed, aborted) = tokio::join!(
        session.handle(request(
            "POST",
            &format!("/mountx/race.bin?uploadId={upload_id}"),
            complete_body.as_bytes(),
            &[],
        )),
        session.handle(request(
            "DELETE",
            &format!("/mountx/race.bin?uploadId={upload_id}"),
            [],
            &[],
        )),
    );
    let outcome = format!("{}/{}", completed.status, aborted.status);
    assert!(
        matches!(outcome.as_str(), "200/404" | "404/204"),
        "{outcome}"
    );

    let object = session
        .handle(request("GET", "/mountx/race.bin", [], &[]))
        .await;
    if outcome == "200/404" {
        assert!(String::from_utf8_lossy(&aborted.body).contains("<Code>NoSuchUpload</Code>"));
        assert_eq!(object.status, 200);
        assert_eq!(object.body, b"the whole object in one part");
    } else {
        assert!(String::from_utf8_lossy(&completed.body).contains("<Code>NoSuchUpload</Code>"));
        assert_eq!(object.status, 404);
    }
    assert!(
        driver
            .stat(&format!("/.mountx-multipart/{upload_id}"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn session_close_sweeps_all_bucket_staging_and_remains_idempotent() {
    let first = MemoryFs::empty();
    let second = MemoryFs::empty();
    let session = S3Session::from_buckets([
        (
            "mountx".to_owned(),
            Arc::new(first.clone()) as Arc<dyn FsDriver>,
        ),
        (
            "other".to_owned(),
            Arc::new(second.clone()) as Arc<dyn FsDriver>,
        ),
    ]);

    for (target, body) in [
        ("/mountx/first.bin", b"first bucket".as_slice()),
        ("/other/second.bin", b"second bucket".as_slice()),
    ] {
        let initiated = session
            .handle(request("POST", &format!("{target}?uploads"), [], &[]))
            .await;
        assert_eq!(initiated.status, 200, "{target}");
        let upload_id = xml_field(&initiated.body, "UploadId");
        let part_target = target.replace("?uploads", "");
        let part = session
            .handle(request(
                "PUT",
                &format!("{part_target}?uploadId={upload_id}&partNumber=1"),
                body,
                &[],
            ))
            .await;
        assert_eq!(part.status, 200, "{target}");
    }

    session.close().await.expect("first close sweeps staging");
    session.close().await.expect("second close is idempotent");
    assert!(first.stat("/.mountx-multipart").await.is_err());
    assert!(second.stat("/.mountx-multipart").await.is_err());
    assert_eq!(
        session
            .handle(request(
                "PUT",
                "/mountx/after-close.txt",
                b"still live",
                &[]
            ))
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn multipart_staging_is_bounded_reaped_and_deleted_honestly() {
    let driver = MemoryFs::empty();
    let session = S3Session::new_with_options(
        driver.clone(),
        S3SessionOptions {
            multipart_staging_max_bytes: 64,
            multipart_staging_ttl_ms: 60_000,
            ..S3SessionOptions::default()
        },
    );

    let initiated = session
        .handle(request("POST", "/mountx/quota.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let quota_upload_id = xml_field(&initiated.body, "UploadId");
    let oversized = session
        .handle(request(
            "PUT",
            &format!("/mountx/quota.bin?uploadId={quota_upload_id}&partNumber=1"),
            vec![b'x'; 128],
            &[],
        ))
        .await;
    assert_eq!(oversized.status, 503);
    assert!(String::from_utf8_lossy(&oversized.body).contains("<Code>SlowDown</Code>"));

    let cleanup_body =
        format!("<Delete><Object><Key>.mountx-multipart/{quota_upload_id}</Key></Object></Delete>");
    let cleaned = session
        .handle(request("POST", "/mountx?delete", cleanup_body, &[]))
        .await;
    assert_eq!(cleaned.status, 200);
    assert!(String::from_utf8_lossy(&cleaned.body).contains(&format!(
        "<Deleted><Key>.mountx-multipart/{quota_upload_id}</Key></Deleted>"
    )));
    assert!(
        driver
            .stat(&format!("/.mountx-multipart/{quota_upload_id}"))
            .await
            .is_err()
    );

    let initiated = session
        .handle(request("POST", "/mountx/delete.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let delete_upload_id = xml_field(&initiated.body, "UploadId");
    let part = session
        .handle(request(
            "PUT",
            &format!("/mountx/delete.bin?uploadId={delete_upload_id}&partNumber=1"),
            b"part",
            &[],
        ))
        .await;
    assert_eq!(part.status, 200);
    let delete_part_body = format!(
        "<Delete><Object><Key>.mountx-multipart/{delete_upload_id}/part-1</Key></Object></Delete>"
    );
    let deleted_part = session
        .handle(request("POST", "/mountx?delete", delete_part_body, &[]))
        .await;
    assert_eq!(deleted_part.status, 200);
    assert!(
        String::from_utf8_lossy(&deleted_part.body).contains(&format!(
            "<Deleted><Key>.mountx-multipart/{delete_upload_id}/part-1</Key></Deleted>"
        ))
    );
    assert!(
        driver
            .stat(&format!("/.mountx-multipart/{delete_upload_id}"))
            .await
            .is_err()
    );

    let staging = driver
        .open("/.mountx-put-test", "w", 0o666)
        .await
        .expect("stream staging file");
    staging
        .write(b"x", Some(0))
        .await
        .expect("write stream staging file");
    staging.close().await.expect("close stream staging file");
    let delete_stream_body = "<Delete><Object><Key>.mountx-put-test</Key></Object></Delete>";
    let deleted_stream = session
        .handle(request("POST", "/mountx?delete", delete_stream_body, &[]))
        .await;
    assert_eq!(deleted_stream.status, 200);
    assert!(
        String::from_utf8_lossy(&deleted_stream.body)
            .contains("<Deleted><Key>.mountx-put-test</Key></Deleted>")
    );
    assert!(driver.stat("/.mountx-put-test").await.is_err());

    let initiated = session
        .handle(request("POST", "/mountx/reap.bin?uploads", [], &[]))
        .await;
    assert_eq!(initiated.status, 200);
    let reap_upload_id = xml_field(&initiated.body, "UploadId");
    let upload_directory = format!("/.mountx-multipart/{reap_upload_id}");
    driver
        .utimes(&upload_directory, 0, 0)
        .await
        .expect("set unavailable mtime");
    let active_parts = session
        .handle(request(
            "GET",
            &format!("/mountx/reap.bin?uploadId={reap_upload_id}"),
            [],
            &[],
        ))
        .await;
    assert_eq!(active_parts.status, 200);
    assert!(driver.stat(&upload_directory).await.is_ok());

    let old_mtime = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as i64
        - 120_000;
    driver
        .utimes(&upload_directory, old_mtime, old_mtime)
        .await
        .expect("age multipart staging");
    let parts = session
        .handle(request(
            "GET",
            &format!("/mountx/reap.bin?uploadId={reap_upload_id}"),
            [],
            &[],
        ))
        .await;
    assert_eq!(parts.status, 404);
    assert!(String::from_utf8_lossy(&parts.body).contains("<Code>NoSuchUpload</Code>"));
    assert!(driver.stat(&upload_directory).await.is_err());
}

#[tokio::test]
async fn http_server_reports_live_connections_and_cleans_them_after_disconnect() {
    let session = Arc::new(S3Session::new(MemoryFs::empty()));
    let server = S3Server::start(session, S3ServerOptions::default())
        .await
        .expect("loopback listener");
    assert_eq!(server.connections(), 0);

    let stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    timeout(Duration::from_secs(1), async {
        loop {
            if server.connections() >= 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("accepted connection count");
    assert!(server.connections() >= 1);

    drop(stream);
    timeout(Duration::from_secs(1), async {
        loop {
            if server.connections() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disconnected connection count");
    server.close().await.expect("clean shutdown");
    assert_eq!(server.connections(), 0);
}

#[tokio::test]
async fn session_assertion_tracking_stays_clean_across_concurrent_replies() {
    let session = Arc::new(S3Session::new(MemoryFs::empty()));
    let (missing_a, missing_b) = tokio::join!(
        session.handle(S3Request::new("GET", "/mountx/missing-a", Vec::new())),
        session.handle(S3Request::new("GET", "/mountx/missing-b", Vec::new())),
    );
    assert_eq!(missing_a.status, 404);
    assert_eq!(missing_b.status, 404);
    let stats = session.stats().await;
    assert_eq!(stats.requests, 2);
    assert_eq!(stats.replies, 2);
    assert_eq!(stats.errors, 2);
    assert_eq!(stats.assertions, 0);
    assert!(session.assertions().is_empty());

    let disabled = S3Session::new_with_options(
        MemoryFs::empty(),
        S3SessionOptions {
            debug: false,
            ..S3SessionOptions::default()
        },
    );
    let response = disabled
        .handle(S3Request::new("GET", "/mountx/missing", Vec::new()))
        .await;
    assert_eq!(response.status, 404);
    assert_eq!(disabled.stats().await.assertions, 0);
    assert!(disabled.assertions().is_empty());
}

#[tokio::test]
async fn http_server_reports_peer_for_connection_io_failure() {
    let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
    let notified = Arc::new(Notify::new());
    let callback_reports = Arc::clone(&reports);
    let callback_notified = Arc::clone(&notified);
    let hooks = S3ServerHooks {
        on_transport_error: Some(Arc::new(move |error| {
            callback_reports.lock().expect("S3 hook lock").push(error);
            callback_notified.notify_waiters();
        })),
    };
    let server = S3Server::start_with_hooks(
        Arc::new(S3Session::new(MemoryFs::empty())),
        S3ServerOptions::default(),
        hooks,
    )
    .await
    .expect("loopback listener");

    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    stream
        .write_all(b"GET /mountx/incomplete HTTP/1.1\r\nHost: ")
        .await
        .expect("write partial request");
    let stream = stream.into_std().expect("convert client stream");
    SockRef::from(&stream)
        .set_linger(Some(Duration::ZERO))
        .expect("set reset-on-close");
    drop(stream);

    timeout(Duration::from_secs(1), async {
        loop {
            if !reports.lock().expect("S3 report lock").is_empty() {
                break;
            }
            notified.notified().await;
        }
    })
    .await
    .expect("peer transport callback");
    let report = reports.lock().expect("S3 report lock")[0].clone();
    assert_eq!(report.kind, S3TransportErrorKind::Connection);
    assert!(
        report
            .peer
            .as_deref()
            .is_some_and(|peer| peer.starts_with("127.0.0.1:"))
    );
    server.close().await.expect("clean shutdown");
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
async fn real_http_signed_body_digest_mismatch_is_rejected() {
    let credentials = Credentials::new("AKIAMOUNTX7DIGEST", "test-secret-key");
    let memory = MemoryFs::empty();
    let session = Arc::new(S3Session::new_with_options(
        memory.clone(),
        S3SessionOptions {
            credentials: Some(credentials.clone()),
            region: Some("us-east-1".to_owned()),
            ..S3SessionOptions::default()
        },
    ));
    let server = S3Server::start(session, S3ServerOptions::default())
        .await
        .expect("loopback listener");
    let expected = b"the signed payload";
    let tampered = b"the forged payload";
    assert_eq!(expected.len(), tampered.len());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as i64;
    let amz_date = format_amz_date(now);
    let payload_hash = sha256_hex(expected);
    let headers = vec![
        HeaderEntry::new("host", server.address().to_string()),
        HeaderEntry::new("x-amz-date", &amz_date),
        HeaderEntry::new("x-amz-content-sha256", &payload_hash),
        HeaderEntry::new("content-length", tampered.len().to_string()),
    ];
    let signed_names = headers
        .iter()
        .map(|header| header.name.clone())
        .collect::<Vec<_>>();
    let authorization = sign_request(SignRequest {
        method: "PUT",
        path: "/mountx/digest-mismatch.txt",
        query: &[],
        headers: &headers,
        signed_headers: &signed_names,
        credentials: &credentials,
        region: "us-east-1",
        timestamp_ms: now,
        payload_hash: &payload_hash,
    });
    let mut wire_headers = headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    wire_headers.push(("authorization".to_owned(), authorization));
    let stored = wire_request(
        &server,
        "PUT",
        "/mountx/digest-mismatch.txt",
        &wire_headers,
        expected,
    )
    .await;
    assert_eq!(stored.status, 200);

    let response = wire_request(
        &server,
        "PUT",
        "/mountx/digest-mismatch.txt",
        &wire_headers,
        tampered,
    )
    .await;
    assert_eq!(response.status, 403);
    assert!(String::from_utf8_lossy(&response.body).contains("<Code>SignatureDoesNotMatch</Code>"));
    let handle = memory
        .open("/digest-mismatch.txt", "r", 0)
        .await
        .expect("existing object remains readable");
    let mut preserved = vec![0_u8; expected.len()];
    assert_eq!(
        handle.read(&mut preserved, Some(0)).await.unwrap(),
        expected.len()
    );
    handle.close().await.unwrap();
    assert_eq!(preserved, expected);
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
            ..S3ServerOptions::default()
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(S3BindError::UnauthenticatedNonLoopback { host }) if host == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    ));
}

#[tokio::test]
async fn credentialed_server_refuses_non_loopback_without_tls() {
    let session = Arc::new(S3Session::new_with_options(
        MemoryFs::empty(),
        S3SessionOptions {
            credentials: Some(Credentials::new("AKIAMOUNTX7REMOTE", "test-secret-key")),
            region: Some("us-east-1".to_owned()),
            ..S3SessionOptions::default()
        },
    ));
    let result = S3Server::start(
        session,
        S3ServerOptions {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 0,
            ..S3ServerOptions::default()
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(S3BindError::NonLoopbackUnsupported { host })
            if host == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    ));
}

#[derive(Clone)]
struct ProbeSignals {
    first_write: Arc<Notify>,
    first_read: Arc<Notify>,
    second_read: Arc<Notify>,
    release_read: Arc<Notify>,
    writes: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
    opens: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
}

impl ProbeSignals {
    fn new() -> Self {
        Self {
            first_write: Arc::new(Notify::new()),
            first_read: Arc::new(Notify::new()),
            second_read: Arc::new(Notify::new()),
            release_read: Arc::new(Notify::new()),
            writes: Arc::new(AtomicUsize::new(0)),
            reads: Arc::new(AtomicUsize::new(0)),
            opens: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Clone)]
struct ProbeFs {
    inner: MemoryFs,
    signals: ProbeSignals,
}

struct ProbeHandle {
    inner: Arc<dyn FileHandle>,
    signals: ProbeSignals,
}

#[async_trait]
impl FileHandle for ProbeHandle {
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> FsResult<usize> {
        let count = self.inner.read(buffer, position).await?;
        if count > 0 {
            let previous = self.signals.reads.fetch_add(1, Ordering::Relaxed);
            if previous == 0 {
                self.signals.first_read.notify_one();
            } else if previous == 1 {
                self.signals.second_read.notify_one();
                self.signals.release_read.notified().await;
            }
        }
        Ok(count)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> FsResult<usize> {
        let count = self.inner.write(buffer, position).await?;
        if count > 0 {
            self.signals.writes.fetch_add(1, Ordering::Relaxed);
            self.signals.first_write.notify_one();
        }
        Ok(count)
    }

    async fn stat(&self) -> FsResult<mount_rs_core::Stats> {
        self.inner.stat().await
    }

    async fn truncate(&self, length: u64) -> FsResult<()> {
        self.inner.truncate(length).await
    }

    async fn close(&self) -> FsResult<()> {
        self.signals.closes.fetch_add(1, Ordering::Relaxed);
        self.inner.close().await
    }
}

#[async_trait]
impl FsDriver for ProbeFs {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn stat(&self, path: &str) -> FsResult<mount_rs_core::Stats> {
        self.inner.stat(path).await
    }

    async fn readdir(&self, path: &str) -> FsResult<Vec<mount_rs_core::DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> FsResult<Arc<dyn FileHandle>> {
        let inner = self.inner.open(path, flags, mode).await?;
        self.signals.opens.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(ProbeHandle {
            inner,
            signals: self.signals.clone(),
        }))
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> FsResult<()> {
        self.inner.rename(old_path, new_path).await
    }
}

#[derive(Clone)]
struct ShortReadFs {
    inner: MemoryFs,
    path: String,
    deliver: u64,
}

struct ShortReadHandle {
    inner: Arc<dyn FileHandle>,
    deliver: u64,
}

#[async_trait]
impl FileHandle for ShortReadHandle {
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> FsResult<usize> {
        let Some(position) = position else {
            return self.inner.read(buffer, None).await;
        };
        if position >= self.deliver {
            return Ok(0);
        }
        let remaining = self.deliver - position;
        let limit = remaining.min(buffer.len() as u64) as usize;
        self.inner.read(&mut buffer[..limit], Some(position)).await
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> FsResult<usize> {
        self.inner.write(buffer, position).await
    }

    async fn stat(&self) -> FsResult<mount_rs_core::Stats> {
        self.inner.stat().await
    }

    async fn truncate(&self, length: u64) -> FsResult<()> {
        self.inner.truncate(length).await
    }

    async fn close(&self) -> FsResult<()> {
        self.inner.close().await
    }
}

#[async_trait]
impl FsDriver for ShortReadFs {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn stat(&self, path: &str) -> FsResult<mount_rs_core::Stats> {
        self.inner.stat(path).await
    }

    async fn readdir(&self, path: &str) -> FsResult<Vec<mount_rs_core::DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> FsResult<Arc<dyn FileHandle>> {
        let inner = self.inner.open(path, flags, mode).await?;
        if path == self.path && flags.contains('r') {
            Ok(Arc::new(ShortReadHandle {
                inner,
                deliver: self.deliver,
            }))
        } else {
            Ok(inner)
        }
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> FsResult<()> {
        self.inner.rename(old_path, new_path).await
    }
}

#[derive(Clone)]
struct ConditionalPutRaceSignals {
    armed: Arc<AtomicBool>,
    target_stats: Arc<AtomicUsize>,
    target_opens: Arc<AtomicUsize>,
    first_stat: Arc<Notify>,
    release_first_stat: Arc<Notify>,
    second_stat: Arc<Notify>,
    first_open: Arc<Notify>,
    release_first_open: Arc<Notify>,
}

impl ConditionalPutRaceSignals {
    fn new() -> Self {
        Self {
            armed: Arc::new(AtomicBool::new(false)),
            target_stats: Arc::new(AtomicUsize::new(0)),
            target_opens: Arc::new(AtomicUsize::new(0)),
            first_stat: Arc::new(Notify::new()),
            release_first_stat: Arc::new(Notify::new()),
            second_stat: Arc::new(Notify::new()),
            first_open: Arc::new(Notify::new()),
            release_first_open: Arc::new(Notify::new()),
        }
    }

    fn arm(&self) {
        self.target_stats.store(0, Ordering::SeqCst);
        self.target_opens.store(0, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct ConditionalPutRaceFs {
    inner: MemoryFs,
    target: String,
    signals: ConditionalPutRaceSignals,
}

#[async_trait]
impl FsDriver for ConditionalPutRaceFs {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn stat(&self, path: &str) -> FsResult<mount_rs_core::Stats> {
        if self.signals.armed.load(Ordering::SeqCst) && path == self.target {
            let call = self.signals.target_stats.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                self.signals.first_stat.notify_one();
                self.signals.release_first_stat.notified().await;
            } else if call == 1 {
                self.signals.second_stat.notify_one();
            }
        }
        self.inner.stat(path).await
    }

    async fn readdir(&self, path: &str) -> FsResult<Vec<mount_rs_core::DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> FsResult<Arc<dyn FileHandle>> {
        if self.signals.armed.load(Ordering::SeqCst)
            && path == self.target
            && flags.contains('w')
            && self.signals.target_opens.fetch_add(1, Ordering::SeqCst) == 0
        {
            self.signals.first_open.notify_one();
            self.signals.release_first_open.notified().await;
        }
        self.inner.open(path, flags, mode).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> FsResult<()> {
        self.inner.rename(old_path, new_path).await
    }
}

#[tokio::test]
async fn conditional_puts_serialize_compare_and_swap_checks() {
    let signals = ConditionalPutRaceSignals::new();
    let driver = ConditionalPutRaceFs {
        inner: MemoryFs::empty(),
        target: "/cas-race.txt".to_owned(),
        signals: signals.clone(),
    };
    let session = Arc::new(S3Session::new(driver));
    let seeded = session
        .handle(request("PUT", "/mountx/cas-race.txt", b"seed", &[]))
        .await;
    assert_eq!(seeded.status, 200);
    let etag = header(&seeded, "etag").expect("seed ETag");
    signals.arm();

    let first_session = Arc::clone(&session);
    let first_etag = etag.clone();
    let first = tokio::spawn(async move {
        first_session
            .handle(request(
                "PUT",
                "/mountx/cas-race.txt",
                b"first conditional body",
                &[("if-match", first_etag.as_str())],
            ))
            .await
    });
    let second_session = Arc::clone(&session);
    let second_etag = etag;
    let second = tokio::spawn(async move {
        second_session
            .handle(request(
                "PUT",
                "/mountx/cas-race.txt",
                b"second conditional body",
                &[("if-match", second_etag.as_str())],
            ))
            .await
    });

    timeout(Duration::from_secs(1), signals.first_stat.notified())
        .await
        .expect("first conditional stat");
    signals.release_first_stat.notify_one();
    timeout(Duration::from_secs(1), signals.first_open.notified())
        .await
        .expect("first conditional write open");
    assert!(
        timeout(Duration::from_millis(100), signals.second_stat.notified())
            .await
            .is_err(),
        "a second conditional precondition must not observe the old ETag while the first write is pending"
    );
    signals.release_first_open.notify_one();

    let first = timeout(Duration::from_secs(1), first)
        .await
        .expect("first conditional PUT completion")
        .expect("first conditional PUT task");
    let second = timeout(Duration::from_secs(1), second)
        .await
        .expect("second conditional PUT completion")
        .expect("second conditional PUT task");
    let mut statuses = [first.status, second.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 412]);
}

#[tokio::test]
async fn real_http_fragmented_upload_reaches_driver_before_body_end() {
    let signals = ProbeSignals::new();
    let session = Arc::new(S3Session::new(ProbeFs {
        inner: MemoryFs::empty(),
        signals: signals.clone(),
    }));
    let server = S3Server::start(Arc::clone(&session), S3ServerOptions::default())
        .await
        .expect("loopback listener");
    let payload = vec![0x5a; 4 * 1024 * 1024];
    let first_fragment = 64 * 1024;
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request_head = format!(
        "PUT /mountx/fragmented-upload.bin HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        server.address(),
        payload.len()
    );
    stream
        .write_all(request_head.as_bytes())
        .await
        .expect("write request head");
    stream
        .write_all(&payload[..first_fragment])
        .await
        .expect("write first request fragment");
    timeout(Duration::from_secs(2), signals.first_write.notified())
        .await
        .expect("driver write before complete request body");
    for fragment in payload[first_fragment..].chunks(64 * 1024) {
        stream
            .write_all(fragment)
            .await
            .expect("write request fragment");
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let response = parse_wire_response(raw);
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers.get("content-length").map(String::as_str),
        Some("0")
    );
    assert!(signals.writes.load(Ordering::Relaxed) > 0);
    let stats = session.stats().await;
    assert_eq!(stats.request_bytes, payload.len() as u64);
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn real_http_fragmented_aws_chunked_upload_decodes_before_terminal_frame() {
    let signals = ProbeSignals::new();
    let memory = MemoryFs::empty();
    let session = Arc::new(S3Session::new(ProbeFs {
        inner: memory.clone(),
        signals: signals.clone(),
    }));
    let server = S3Server::start(session, S3ServerOptions::default())
        .await
        .expect("loopback listener");
    let payload = vec![0x42; 2 * 1024 * 1024];
    let encoded =
        unsigned_chunked_body_with_trailer(&payload, "x-amz-checksum-crc32", "1B2M2Y8=", true);
    let first_fragment = 64 * 1024;
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request_head = format!(
        "PUT /mountx/fragmented-chunked.bin HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Encoding: aws-chunked\r\nX-Amz-Content-Sha256: {}\r\nX-Amz-Trailer: x-amz-checksum-crc32\r\nX-Amz-Decoded-Content-Length: {}\r\n\r\n",
        server.address(),
        encoded.len(),
        STREAMING_UNSIGNED_PAYLOAD_TRAILER,
        payload.len()
    );
    stream
        .write_all(request_head.as_bytes())
        .await
        .expect("write request head");
    stream
        .write_all(&encoded[..first_fragment])
        .await
        .expect("write first encoded fragment");
    timeout(Duration::from_secs(2), signals.first_write.notified())
        .await
        .expect("decoded driver write before terminal frame");
    for fragment in encoded[first_fragment..].chunks(31_337) {
        stream
            .write_all(fragment)
            .await
            .expect("write encoded fragment");
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let response = parse_wire_response(raw);
    assert_eq!(response.status, 200);

    let handle = memory
        .open("/fragmented-chunked.bin", "r", 0)
        .await
        .expect("stored object");
    let mut stored = vec![0_u8; payload.len()];
    let count = handle
        .read(&mut stored, Some(0))
        .await
        .expect("read object");
    handle.close().await.expect("close object");
    assert_eq!(count, payload.len());
    assert_eq!(stored, payload);
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn real_http_download_sends_first_chunk_before_next_driver_read() {
    let memory = MemoryFs::empty();
    let payload = vec![0x31; 2 * 1024 * 1024];
    let handle = memory
        .open("/fragmented-download.bin", "w", 0o666)
        .await
        .unwrap();
    handle.write(&payload, Some(0)).await.unwrap();
    handle.close().await.unwrap();
    let signals = ProbeSignals::new();
    let session = Arc::new(S3Session::new(ProbeFs {
        inner: memory,
        signals: signals.clone(),
    }));
    let server = S3Server::start(Arc::clone(&session), S3ServerOptions::default())
        .await
        .expect("loopback listener");
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request = format!(
        "GET /mountx/fragmented-download.bin HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.address()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    loop {
        let mut buffer = [0_u8; 8192];
        let count = timeout(Duration::from_secs(2), stream.read(&mut buffer))
            .await
            .expect("response bytes before next driver read")
            .expect("read response");
        assert!(count > 0, "response ended before first body chunk");
        raw.extend_from_slice(&buffer[..count]);
        if let Some(offset) = raw.windows(4).position(|window| window == b"\r\n\r\n")
            && raw.len() > offset + 4
        {
            break;
        }
    }
    timeout(Duration::from_secs(2), signals.first_read.notified())
        .await
        .expect("driver performed first read");
    signals.release_read.notify_one();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read response body");
    let response = parse_wire_response(raw);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, payload);
    let stats = session.stats().await;
    assert_eq!(stats.response_bytes, payload.len() as u64);
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn http_server_closes_abandoned_download_handle() {
    let memory = MemoryFs::empty();
    let payload = vec![0x27; 2 * 1024 * 1024];
    let handle = memory
        .open("/abandoned.bin", "w", 0o666)
        .await
        .expect("open object");
    handle.write(&payload, Some(0)).await.expect("write object");
    handle.close().await.expect("close object");
    let signals = ProbeSignals::new();
    let session = Arc::new(S3Session::new(ProbeFs {
        inner: memory,
        signals: signals.clone(),
    }));
    let server = S3Server::start(Arc::clone(&session), S3ServerOptions::default())
        .await
        .expect("loopback listener");

    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request = format!(
        "GET /mountx/abandoned.bin HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\n\r\n",
        server.address()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write abandoned download request");
    let mut raw = Vec::new();
    loop {
        let mut buffer = [0_u8; 8192];
        let count = timeout(Duration::from_secs(2), stream.read(&mut buffer))
            .await
            .expect("response bytes before abandonment")
            .expect("read response");
        assert!(count > 0, "response ended before the first body chunk");
        raw.extend_from_slice(&buffer[..count]);
        if let Some(offset) = raw.windows(4).position(|window| window == b"\r\n\r\n")
            && raw.len() > offset + 4
        {
            break;
        }
    }
    timeout(Duration::from_secs(2), signals.second_read.notified())
        .await
        .expect("download entered the parked second read");
    drop(stream);

    timeout(Duration::from_secs(2), async {
        loop {
            if signals.opens.load(Ordering::Relaxed) > 0
                && signals.closes.load(Ordering::Relaxed) >= signals.opens.load(Ordering::Relaxed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("abandoned download handle closed");
    assert_eq!(signals.opens.load(Ordering::Relaxed), 1);
    assert_eq!(signals.closes.load(Ordering::Relaxed), 1);

    let next = wire_request(
        &server,
        "GET",
        "/mountx/abandoned.bin",
        &[("range".to_owned(), "bytes=0-3".to_owned())],
        &[],
    )
    .await;
    assert_eq!(next.status, 206);
    assert_eq!(next.body, payload[..4]);
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn http_server_close_allows_inflight_response_to_finish() {
    let memory = MemoryFs::empty();
    let payload = vec![0x52; 2 * 1024 * 1024];
    let handle = memory
        .open("/inflight-download.bin", "w", 0o666)
        .await
        .expect("open object");
    handle.write(&payload, Some(0)).await.expect("write object");
    handle.close().await.expect("close object");
    let signals = ProbeSignals::new();
    let session = Arc::new(S3Session::new(ProbeFs {
        inner: memory,
        signals: signals.clone(),
    }));
    let server = S3Server::start(Arc::clone(&session), S3ServerOptions::default())
        .await
        .expect("loopback listener");
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request = format!(
        "GET /mountx/inflight-download.bin HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.address()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write in-flight request");
    timeout(Duration::from_secs(2), signals.second_read.notified())
        .await
        .expect("response entered parked read");

    let mut raw = Vec::new();
    let response = async {
        stream.read_to_end(&mut raw).await.expect("read response");
    };
    let closing = server.close();
    signals.release_read.notify_one();
    timeout(Duration::from_secs(2), async {
        let (close_result, ()) = tokio::join!(closing, response);
        close_result.expect("in-flight response close");
    })
    .await
    .expect("in-flight response finished before drain deadline");
    let response = parse_wire_response(raw);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, payload);
}

#[tokio::test]
async fn http_server_close_aborts_stalled_response_at_drain_deadline() {
    let memory = MemoryFs::empty();
    let payload = vec![0x41; 2 * 1024 * 1024];
    let handle = memory
        .open("/stalled-download.bin", "w", 0o666)
        .await
        .expect("open stalled object");
    handle
        .write(&payload, Some(0))
        .await
        .expect("write stalled object");
    handle.close().await.expect("close stalled object");

    let signals = ProbeSignals::new();
    let session = Arc::new(S3Session::new(ProbeFs {
        inner: memory,
        signals: signals.clone(),
    }));
    let server = S3Server::start(
        Arc::clone(&session),
        S3ServerOptions {
            drain_timeout: Duration::from_millis(50),
            ..S3ServerOptions::default()
        },
    )
    .await
    .expect("loopback listener");
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request = format!(
        "GET /mountx/stalled-download.bin HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.address()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    timeout(Duration::from_secs(2), signals.first_read.notified())
        .await
        .expect("stalled response reached the first driver read");

    let started = Instant::now();
    server
        .close()
        .await
        .expect("close cuts a response past the drain deadline");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "bounded close took too long: {:?}",
        started.elapsed()
    );
    timeout(Duration::from_secs(1), async {
        loop {
            if server.connections() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stalled connection was dropped after close");
}

#[tokio::test]
async fn http_server_aborts_short_streamed_response_without_reusing_connection() {
    const WHOLE: usize = 256 * 1024;
    const DELIVERED: u64 = 64 * 1024;
    let memory = MemoryFs::empty();
    let payload = (0..WHOLE)
        .map(|index| ((index * 31 + (index >> 8)) & 0xff) as u8)
        .collect::<Vec<_>>();
    for path in ["/short.bin", "/whole.bin"] {
        let handle = memory.open(path, "w", 0o666).await.expect("open object");
        handle.write(&payload, Some(0)).await.expect("write object");
        handle.close().await.expect("close object");
    }
    let reports = Arc::new(StdMutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&reports);
    let session = Arc::new(S3Session::new(ShortReadFs {
        inner: memory,
        path: "/short.bin".to_owned(),
        deliver: DELIVERED,
    }));
    let server = S3Server::start_with_hooks(
        session,
        S3ServerOptions::default(),
        S3ServerHooks {
            on_transport_error: Some(Arc::new(move |error| {
                observed
                    .lock()
                    .expect("framing reports lock")
                    .push(error.message);
            })),
        },
    )
    .await
    .expect("loopback listener");

    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request = format!(
        "GET /mountx/short.bin HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\n\r\n",
        server.address()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    let _ = timeout(Duration::from_secs(2), stream.read_to_end(&mut raw))
        .await
        .expect("short response connection terminated");
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response headers");
    let head = String::from_utf8_lossy(&raw[..separator]).to_ascii_lowercase();
    assert!(head.contains(&format!("content-length: {WHOLE}")));
    assert!(raw.len() - separator - 4 < WHOLE);

    timeout(Duration::from_secs(1), async {
        loop {
            if reports
                .lock()
                .expect("framing reports lock")
                .iter()
                .any(|message| message.contains("out of frame"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("short response framing report");
    let framing_message = {
        let reports = reports.lock().expect("framing reports lock");
        let framing_reports = reports
            .iter()
            .filter(|message| message.contains("out of frame"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(framing_reports.len(), 1);
        framing_reports[0].clone()
    };
    assert!(framing_message.contains(&format!("declared {WHOLE} bytes and produced {DELIVERED}")));

    let next = wire_request(&server, "GET", "/mountx/whole.bin", &[], &[]).await;
    assert_eq!(next.status, 200);
    assert_eq!(next.body, payload);
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn http_server_answers_pipelined_requests_in_order() {
    let memory = MemoryFs::empty();
    for (path, payload) in [
        ("/pipe-one.txt", b"the first reply".as_slice()),
        ("/pipe-two.txt", b"the second reply".as_slice()),
    ] {
        let handle = memory.open(path, "w", 0o666).await.expect("open object");
        handle.write(payload, Some(0)).await.expect("write object");
        handle.close().await.expect("close object");
    }
    let server = S3Server::start(Arc::new(S3Session::new(memory)), S3ServerOptions::default())
        .await
        .expect("loopback listener");

    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request = format!(
        "GET /mountx/pipe-one.txt HTTP/1.1\r\nHost: {}\r\n\r\nGET /mountx/pipe-two.txt HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.address(),
        server.address()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write pipelined requests");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read pipelined responses");
    let raw = String::from_utf8_lossy(&raw);
    assert_eq!(raw.matches("HTTP/1.1 200").count(), 2);
    let first = raw.find("the first reply").expect("first response body");
    let second = raw.find("the second reply").expect("second response body");
    assert!(first < second, "pipelined replies were reordered");
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn http_server_accepts_expect_continue_upload() {
    let memory = MemoryFs::empty();
    let server = S3Server::start(
        Arc::new(S3Session::new(memory.clone())),
        S3ServerOptions::default(),
    )
    .await
    .expect("loopback listener");
    let payload = b"a body the client held back until it was invited";
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request_head = format!(
        "PUT /mountx/continue.txt HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n",
        server.address(),
        payload.len()
    );
    stream
        .write_all(request_head.as_bytes())
        .await
        .expect("write expect-continue request head");

    let mut interim = Vec::new();
    timeout(Duration::from_secs(2), async {
        loop {
            let mut buffer = [0_u8; 1024];
            let count = stream
                .read(&mut buffer)
                .await
                .expect("read continue response");
            assert!(count > 0, "server closed before sending 100 Continue");
            interim.extend_from_slice(&buffer[..count]);
            if interim.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
    })
    .await
    .expect("bounded wait for 100 Continue");
    assert!(String::from_utf8_lossy(&interim).starts_with("HTTP/1.1 100 Continue"));

    stream
        .write_all(payload)
        .await
        .expect("write expect-continue body");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read final response");
    let response = parse_wire_response(raw);
    assert_eq!(response.status, 200);

    let handle = memory
        .open("/continue.txt", "r", 0)
        .await
        .expect("stored expect-continue object");
    let mut stored = vec![0_u8; payload.len()];
    let count = handle
        .read(&mut stored, Some(0))
        .await
        .expect("read stored expect-continue object");
    handle.close().await.expect("close stored object");
    assert_eq!(count, payload.len());
    assert_eq!(stored, payload);
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn http_server_rejects_transfer_encoding_without_content_length() {
    let memory = MemoryFs::empty();
    let server = S3Server::start(Arc::new(S3Session::new(memory)), S3ServerOptions::default())
        .await
        .expect("loopback listener");
    let payload = b"framed by the transport";
    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request_head = format!(
        "PUT /mountx/te.txt HTTP/1.1\r\nHost: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        server.address()
    );
    let mut request = request_head.into_bytes();
    request.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
    request.extend_from_slice(payload);
    request.extend_from_slice(b"\r\n0\r\n\r\n");
    stream
        .write_all(&request)
        .await
        .expect("write transfer-encoded request");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read transfer-encoding refusal");
    let response = parse_wire_response(raw);
    assert_eq!(response.status, 411);
    assert!(String::from_utf8_lossy(&response.body).contains("<Code>MissingContentLength</Code>"));
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn http_server_carries_head_length_without_a_body() {
    let memory = MemoryFs::empty();
    let payload = vec![0x3a; 4096];
    let handle = memory
        .open("/sized.bin", "w", 0o666)
        .await
        .expect("open object");
    handle.write(&payload, Some(0)).await.expect("write object");
    handle.close().await.expect("close object");
    let server = S3Server::start(Arc::new(S3Session::new(memory)), S3ServerOptions::default())
        .await
        .expect("loopback listener");

    let response = wire_request(&server, "HEAD", "/mountx/sized.bin", &[], &[]).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers.get("content-length").map(String::as_str),
        Some("4096")
    );
    assert!(response.body.is_empty());
    server.close().await.expect("clean shutdown");
}

#[tokio::test]
async fn http_server_drains_rejected_body_before_reusing_connection() {
    let memory = MemoryFs::empty();
    let payload = b"the next reply";
    let handle = memory
        .open("/after-rejected-body.txt", "w", 0o666)
        .await
        .expect("open object");
    handle.write(payload, Some(0)).await.expect("write object");
    handle.close().await.expect("close object");
    let server = S3Server::start(Arc::new(S3Session::new(memory)), S3ServerOptions::default())
        .await
        .expect("loopback listener");

    let mut stream = TcpStream::connect(server.address())
        .await
        .expect("connect gateway");
    let request = format!(
        "PUT /mountx HTTP/1.1\r\nHost: {}\r\nContent-Length: 4\r\n\r\njunkGET /mountx/after-rejected-body.txt HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.address(),
        server.address()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write rejected request and follow-up");
    let mut raw = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut raw))
        .await
        .expect("bounded response drain")
        .expect("read rejected request responses");
    let raw = String::from_utf8_lossy(&raw);
    let rejected = raw.find("HTTP/1.1 501").expect("rejected response");
    let follow_up = raw.find("HTTP/1.1 200").expect("follow-up response");
    assert!(rejected < follow_up, "follow-up response was reordered");
    assert!(raw[follow_up..].contains("the next reply"));
    server.close().await.expect("clean shutdown");
}

#[derive(Debug)]
struct WireResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn parse_wire_response(raw: Vec<u8>) -> WireResponse {
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
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    WireResponse {
        status,
        headers,
        body: raw[separator + 4..].to_vec(),
    }
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
