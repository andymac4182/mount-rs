use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mount_rs_core::{Capabilities, FileHandle, FsDriver, MemoryFs, Result as FsResult};
use mount_rs_s3::{
    CredentialScope, Credentials, EMPTY_PAYLOAD_SHA256, HeaderEntry, PresignRequest, S3BindError,
    S3Request, S3Response, S3Server, S3ServerOptions, S3Session, S3SessionOptions,
    STREAMING_PAYLOAD, STREAMING_PAYLOAD_TRAILER, STREAMING_UNSIGNED_PAYLOAD_TRAILER, SignRequest,
    canonical_query, format_amz_date, presign_request, sha256_hex, sign_chunk, sign_request,
    sign_trailer,
};
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
    assert!(memory.stat("/digest-mismatch.txt").await.is_err());
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

#[derive(Clone)]
struct ProbeSignals {
    first_write: Arc<Notify>,
    first_read: Arc<Notify>,
    release_read: Arc<Notify>,
    writes: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

impl ProbeSignals {
    fn new() -> Self {
        Self {
            first_write: Arc::new(Notify::new()),
            first_read: Arc::new(Notify::new()),
            release_read: Arc::new(Notify::new()),
            writes: Arc::new(AtomicUsize::new(0)),
            reads: Arc::new(AtomicUsize::new(0)),
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
        Ok(Arc::new(ProbeHandle {
            inner,
            signals: self.signals.clone(),
        }))
    }
}

#[tokio::test]
async fn real_http_fragmented_upload_reaches_driver_before_body_end() {
    let signals = ProbeSignals::new();
    let session = Arc::new(S3Session::new(ProbeFs {
        inner: MemoryFs::empty(),
        signals: signals.clone(),
    }));
    let server = S3Server::start(session, S3ServerOptions::default())
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
    let server = S3Server::start(session, S3ServerOptions::default())
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
