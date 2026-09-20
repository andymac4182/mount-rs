//! Pinned AWS-chunked fixtures transcribed from upstream `test/s3/chunked.test.ts`.

use mount_rs_s3::{
    AwsChunkedDecoder, AwsChunkedParams, CHUNKED_MAX_FRAME, ChunkedLimits, ChunkedRefusal,
    ChunkedSignature, CredentialScope, HeaderEntry, STREAMING_PAYLOAD, STREAMING_PAYLOAD_TRAILER,
    STREAMING_UNSIGNED_PAYLOAD_TRAILER, canonical_request, chunk_string_to_sign,
    decode_aws_chunked, is_chunked_error, sha256_hex, sign_chunked, sign_trailer_chunked,
    signature_of, streaming_payload_kind, trailer_string_to_sign,
};

const DOC_SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const DOC_AMZ_DATE: &str = "20130524T000000Z";
const DOC_SEED_SIGNATURE: &str = "4f232c4386841ef735655705268965c44a0e4690baa4adea153f7db9fa80a0a9";
const DOC_CHUNK_SIGNATURES: [&str; 3] = [
    "ad80c730a21e5b8d04586a2213dd63b9a0e99e0e2307b0ade35a65485a288648",
    "0055627c9e194cb4542bae2aa5492e3c1575bbb81b612b7d234b86a503ef5497",
    "b6c6ea8a5354eaf15b3cb7646744f4275b71ea724fed81ceb9323e279d449df9",
];
fn doc_scope() -> CredentialScope {
    CredentialScope {
        date: "20130524".to_owned(),
        region: "us-east-1".to_owned(),
        service: "s3".to_owned(),
    }
}

fn doc_signature() -> ChunkedSignature {
    ChunkedSignature {
        seed: DOC_SEED_SIGNATURE.to_owned(),
        amz_date: DOC_AMZ_DATE.to_owned(),
        scope: doc_scope(),
        secret_access_key: DOC_SECRET_KEY.to_owned(),
        key: None,
    }
}

fn append_frame(
    body: &mut Vec<u8>,
    payload: &[u8],
    signature: Option<&ChunkedSignature>,
    previous: &mut String,
) {
    let mut header = format!("{:x}", payload.len());
    if let Some(signature) = signature {
        let chunk_signature = sign_chunked(signature, previous, &sha256_hex(payload));
        header.push_str(";chunk-signature=");
        header.push_str(&chunk_signature);
        *previous = chunk_signature;
    }
    body.extend_from_slice(header.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(payload);
    if !payload.is_empty() {
        body.extend_from_slice(b"\r\n");
    }
}

fn build_body(payloads: &[Vec<u8>], signature: Option<&ChunkedSignature>) -> Vec<u8> {
    let mut body = Vec::new();
    let mut previous = signature.map_or_else(String::new, |signature| signature.seed.clone());
    for payload in payloads {
        append_frame(&mut body, payload, signature, &mut previous);
    }
    append_frame(&mut body, &[], signature, &mut previous);
    body.extend_from_slice(b"\r\n");
    body
}

fn flatten(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.iter().flat_map(|part| part.iter().copied()).collect()
}

#[test]
fn streaming_sentinels_match_upstream_shapes() {
    assert_eq!(
        streaming_payload_kind(STREAMING_PAYLOAD),
        Some(mount_rs_s3::StreamingPayloadKind {
            signed: true,
            trailers: false,
        })
    );
    assert_eq!(
        streaming_payload_kind(STREAMING_PAYLOAD_TRAILER),
        Some(mount_rs_s3::StreamingPayloadKind {
            signed: true,
            trailers: true,
        })
    );
    assert_eq!(
        streaming_payload_kind(STREAMING_UNSIGNED_PAYLOAD_TRAILER),
        Some(mount_rs_s3::StreamingPayloadKind {
            signed: false,
            trailers: true,
        })
    );
    assert_eq!(streaming_payload_kind("UNSIGNED-PAYLOAD"), None);
}

#[test]
fn documented_aws_stream_fixture_matches_seed_chain_and_wire_length() {
    let scope = doc_scope();
    let headers = vec![
        HeaderEntry::new("Content-Encoding", "aws-chunked"),
        HeaderEntry::new("Content-Length", "66824"),
        HeaderEntry::new("Host", "s3.amazonaws.com"),
        HeaderEntry::new("x-amz-content-sha256", STREAMING_PAYLOAD),
        HeaderEntry::new("x-amz-date", DOC_AMZ_DATE),
        HeaderEntry::new("x-amz-decoded-content-length", "66560"),
        HeaderEntry::new("x-amz-storage-class", "REDUCED_REDUNDANCY"),
    ];
    let signed_headers = headers
        .iter()
        .map(|header| header.name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let canonical = canonical_request(
        "PUT",
        "/examplebucket/chunkObject.txt",
        &[],
        &headers,
        &signed_headers,
        STREAMING_PAYLOAD,
    );
    assert_eq!(
        signature_of(DOC_SECRET_KEY, &scope, DOC_AMZ_DATE, &canonical,),
        DOC_SEED_SIGNATURE
    );

    let payloads = vec![vec![b'a'; 65_536], vec![b'a'; 1_024]];
    let body = build_body(&payloads, Some(&doc_signature()));
    assert_eq!(body.len(), 66_824);
    let mut previous = DOC_SEED_SIGNATURE.to_owned();
    let mut signatures = Vec::new();
    for payload in payloads.iter().chain(std::iter::once(&Vec::new())) {
        let signature = sign_chunked(&doc_signature(), &previous, &sha256_hex(payload));
        signatures.push(signature.clone());
        previous = signature;
    }
    assert_eq!(signatures, DOC_CHUNK_SIGNATURES);

    let mut decoder = AwsChunkedDecoder::new(AwsChunkedParams {
        signature: Some(doc_signature()),
        decoded_length: Some(66_560),
        ..AwsChunkedParams::default()
    })
    .expect("valid documented seed");
    let output = decoder.write(&body).expect("valid documented body");
    decoder.end().expect("complete documented body");
    assert_eq!(flatten(&output), vec![b'a'; 66_560]);
    assert_eq!(decoder.decoded_bytes(), 66_560);
    assert!(decoder.terminated());
    assert!(decoder.trailers().is_empty());
}

#[test]
fn public_signing_helpers_match_the_pinned_strings() {
    let signature = doc_signature();
    let payload_hash = sha256_hex(vec![b'a'; 65_536]);
    let empty_hash = sha256_hex([]);
    assert_eq!(
        chunk_string_to_sign(&signature, DOC_SEED_SIGNATURE, &payload_hash),
        [
            "AWS4-HMAC-SHA256-PAYLOAD",
            DOC_AMZ_DATE,
            "20130524/us-east-1/s3/aws4_request",
            DOC_SEED_SIGNATURE,
            empty_hash.as_str(),
            payload_hash.as_str(),
        ]
        .join("\n")
    );
    let trailer_hash = sha256_hex("x");
    assert_eq!(
        trailer_string_to_sign(&signature, DOC_SEED_SIGNATURE, &trailer_hash),
        [
            "AWS4-HMAC-SHA256-TRAILER",
            DOC_AMZ_DATE,
            "20130524/us-east-1/s3/aws4_request",
            DOC_SEED_SIGNATURE,
            trailer_hash.as_str(),
        ]
        .join("\n")
    );
}

#[test]
fn fragmented_input_and_buffer_reuse_do_not_alias_outputs() {
    let payloads = vec![vec![b'a'; 40], vec![b'b'; 40]];
    let body = build_body(&payloads, Some(&doc_signature()));
    let mut decoder = AwsChunkedDecoder::new(AwsChunkedParams {
        signature: Some(doc_signature()),
        ..AwsChunkedParams::default()
    })
    .expect("valid seed");
    let mut output = Vec::new();
    let mut scratch = [0u8; 7];
    for piece in body.chunks(7) {
        scratch[..piece.len()].copy_from_slice(piece);
        output.extend(
            decoder
                .write(&scratch[..piece.len()])
                .expect("valid fragment"),
        );
        scratch.fill(0xff);
    }
    decoder.end().expect("complete body");
    assert_eq!(flatten(&output), [vec![b'a'; 40], vec![b'b'; 40]].concat());
    assert!(
        output
            .iter()
            .all(|part| !std::ptr::eq(part.as_ptr(), body.as_ptr()))
    );
}

#[test]
fn signed_and_unsigned_trailers_are_verified_or_framed() {
    let signature = doc_signature();
    let trailer_name = "x-amz-checksum-crc32";
    let trailer_value = "9jRczA==";
    let trailer_line = format!("{trailer_name}:{trailer_value}");
    let mut signed_body = build_body(&[vec![b'a'; 4]], Some(&signature));
    signed_body.truncate(signed_body.len() - 2);
    let mut previous = DOC_SEED_SIGNATURE.to_owned();
    previous = sign_chunked(&signature, &previous, &sha256_hex(vec![b'a'; 4]));
    previous = sign_chunked(&signature, &previous, &sha256_hex([]));
    signed_body.extend_from_slice(trailer_line.as_bytes());
    signed_body.extend_from_slice(b"\r\n");
    let trailer_signature = sign_trailer_chunked(
        &signature,
        &previous,
        &sha256_hex(format!("{trailer_line}\n")),
    );
    signed_body.extend_from_slice(
        format!("x-amz-trailer-signature:{trailer_signature}\r\n\r\n").as_bytes(),
    );
    let mut decoder = AwsChunkedDecoder::new(AwsChunkedParams {
        signature: Some(signature.clone()),
        trailers: vec!["X-Amz-Checksum-Crc32".to_owned()],
        ..AwsChunkedParams::default()
    })
    .expect("valid seed");
    assert_eq!(
        flatten(&decoder.write(&signed_body).expect("signed trailers")),
        vec![b'a'; 4]
    );
    decoder.end().expect("signed trailers complete");
    assert_eq!(decoder.trailers()[0].name, trailer_name);
    assert_eq!(decoder.trailers()[0].value, trailer_value);

    let mut unsigned_body = build_body(&[vec![b'c'; 4]], None);
    let terminal = unsigned_body.len() - 2;
    unsigned_body.truncate(terminal);
    unsigned_body.extend_from_slice(format!("{trailer_line}\r\n\r\n").as_bytes());
    let mut decoder = AwsChunkedDecoder::new(AwsChunkedParams {
        trailers: vec![trailer_name.to_owned()],
        ..AwsChunkedParams::default()
    })
    .expect("unsigned decoder");
    assert_eq!(
        flatten(&decoder.write(&unsigned_body).expect("unsigned trailers")),
        vec![b'c'; 4]
    );
    decoder.end().expect("unsigned trailers complete");
}

#[test]
fn refusal_categories_are_sticky_and_bounded() {
    let mut decoder = AwsChunkedDecoder::new(AwsChunkedParams {
        signature: Some(doc_signature()),
        ..AwsChunkedParams::default()
    })
    .expect("valid seed");
    let first = decoder
        .write(b";chunk-signature=bad\r\n")
        .expect_err("bad size");
    assert_eq!(first.reason, ChunkedRefusal::BadSize);
    assert!(is_chunked_error(&first));
    let second = decoder.write(b"0\r\n").expect_err("sticky refusal");
    assert_eq!(second, first);
    assert_eq!(decoder.end().expect_err("sticky refusal"), first);

    let body = build_body(&[vec![b'a'; 9]], Some(&doc_signature()));
    let mut decoder = AwsChunkedDecoder::new(AwsChunkedParams {
        signature: Some(doc_signature()),
        limits: ChunkedLimits {
            max_frame: 8,
            ..ChunkedLimits::default()
        },
        ..AwsChunkedParams::default()
    })
    .expect("valid seed");
    assert_eq!(
        decoder.write(&body).expect_err("frame cap").reason,
        ChunkedRefusal::TooLarge
    );
    assert_eq!(CHUNKED_MAX_FRAME, 8 * 1024 * 1024);

    let body = build_body(&[vec![b'a'; 4]], Some(&doc_signature()));
    let mut decoder = AwsChunkedDecoder::new(AwsChunkedParams {
        signature: Some(doc_signature()),
        decoded_length: Some(3),
        ..AwsChunkedParams::default()
    })
    .expect("valid seed");
    assert_eq!(
        decoder.write(&body).expect_err("length mismatch").reason,
        ChunkedRefusal::LengthMismatch
    );

    assert_eq!(
        decode_aws_chunked(b"1\r\na\r\n0\r\n\r\n", AwsChunkedParams::default(),)
            .expect("bounded convenience decoder"),
        b"a"
    );
}
