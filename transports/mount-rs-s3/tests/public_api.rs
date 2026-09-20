//! Public S3 helper fixtures transcribed from the upstream S3 tests.
//!
//! These tests deliberately use the crate barrel rather than private modules.
//! The XML values come from the upstream S3 API-reference fixtures and the
//! SigV4 values come from the official aws-sig-v4-test-suite vector
//! `get-vanilla`.

use mount_rs_s3::{
    AWS_CHUNKED_ENCODING, CredentialScope, EMPTY_PAYLOAD_SHA256, HeaderEntry, MAX_KEY_BYTES,
    MAX_KEYS, MAX_PART_SIZE, MAX_PARTS, MAX_PRESIGNED_EXPIRES, MAX_XML_BYTES, MIN_PART_SIZE,
    QUERY_ALGORITHM, QUERY_CREDENTIAL, QUERY_DATE, QUERY_EXPIRES, QUERY_SIGNATURE,
    QUERY_SIGNED_HEADERS, QueryEntry, S3_SERVICE, S3_XMLNS, SIGV4_ALGORITHM, SIGV4_TERMINATOR,
    STREAMING_PAYLOAD, UNSIGNED_PAYLOAD, XML_DECLARATION, XML_MAX_BYTES, XML_MAX_DEPTH,
    XML_MAX_DEPTH_CEILING, XML_MAX_ELEMENTS, canonical_query, canonical_request, canonical_uri,
    copy_object_xml, credential_scope, delete_result_xml, is_presigned, list_buckets_xml,
    parse_authorization_header, parse_complete_document, parse_delete_document,
    parse_presigned_query, s3_error, signature_of, signatures_match, string_to_sign, xml_escape,
};

#[test]
fn exported_constants_match_the_upstream_wire_values() {
    assert_eq!(S3_XMLNS, "http://s3.amazonaws.com/doc/2006-03-01/");
    assert_eq!(
        XML_DECLARATION,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>"
    );
    assert_eq!(AWS_CHUNKED_ENCODING, "aws-chunked");
    assert_eq!(UNSIGNED_PAYLOAD, "UNSIGNED-PAYLOAD");
    assert_eq!(STREAMING_PAYLOAD, "STREAMING-AWS4-HMAC-SHA256-PAYLOAD");
    assert_eq!(SIGV4_ALGORITHM, "AWS4-HMAC-SHA256");
    assert_eq!(S3_SERVICE, "s3");
    assert_eq!(SIGV4_TERMINATOR, "aws4_request");
    assert_eq!(QUERY_ALGORITHM, "X-Amz-Algorithm");
    assert_eq!(QUERY_CREDENTIAL, "X-Amz-Credential");
    assert_eq!(QUERY_DATE, "X-Amz-Date");
    assert_eq!(QUERY_EXPIRES, "X-Amz-Expires");
    assert_eq!(QUERY_SIGNED_HEADERS, "X-Amz-SignedHeaders");
    assert_eq!(QUERY_SIGNATURE, "X-Amz-Signature");
    assert_eq!(MAX_KEYS, 1000);
    assert_eq!(MIN_PART_SIZE, 5 * 1024 * 1024);
    assert_eq!(MAX_PART_SIZE, 5 * 1024 * 1024 * 1024);
    assert_eq!(MAX_PARTS, 10_000);
    assert_eq!(MAX_KEY_BYTES, 1024);
    assert_eq!(MAX_XML_BYTES, 16 * 1024 * 1024);
    assert_eq!(XML_MAX_BYTES, 4 * 1024 * 1024);
    assert_eq!(XML_MAX_DEPTH, 32);
    assert_eq!(XML_MAX_DEPTH_CEILING, 256);
    assert_eq!(XML_MAX_ELEMENTS, 100_000);
    assert_eq!(MAX_PRESIGNED_EXPIRES, 7 * 24 * 60 * 60);
}

#[test]
fn upstream_xml_response_fixtures_are_byte_stable() {
    assert_eq!(
        copy_object_xml(
            "9b2cf535f27731c974343645a3985328",
            "2009-10-28T22:32:00.000Z"
        ),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_owned()
            + "<CopyObjectResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">"
            + "<LastModified>2009-10-28T22:32:00.000Z</LastModified>"
            + "<ETag>&quot;9b2cf535f27731c974343645a3985328&quot;</ETag>"
            + "</CopyObjectResult>"
    );

    let deleted = vec!["sample1.txt".to_owned()];
    let errors = vec![("sample2.txt".to_owned(), s3_error("AccessDenied"))];
    assert_eq!(
        delete_result_xml(&deleted, &errors, false),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_owned()
            + "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">"
            + "<Deleted><Key>sample1.txt</Key></Deleted>"
            + "<Error><Key>sample2.txt</Key><Code>AccessDenied</Code>"
            + "<Message>Access Denied</Message></Error></DeleteResult>"
    );

    assert_eq!(
        list_buckets_xml(&[("quotes".to_owned(), "2006-02-03T16:45:09.000Z".to_owned(),)]),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_owned()
            + "<ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">"
            + "<Owner><ID>mountx-gateway</ID><DisplayName>mountx</DisplayName></Owner>"
            + "<Buckets><Bucket><Name>quotes</Name>"
            + "<CreationDate>2006-02-03T16:45:09.000Z</CreationDate></Bucket></Buckets>"
            + "</ListAllMyBucketsResult>"
    );
}

#[test]
fn upstream_xml_request_fixtures_round_trip_through_bounded_parsers() {
    let delete = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
        "<Object><Key>sample1.txt</Key></Object>",
        "<Object><Key>sample2.txt</Key><VersionId>version-2</VersionId></Object>",
        "<Quiet>true</Quiet></Delete>"
    );
    assert_eq!(
        parse_delete_document(delete.as_bytes(), XML_MAX_BYTES).expect("valid Delete XML"),
        (
            vec!["sample1.txt".to_owned(), "sample2.txt".to_owned()],
            true
        )
    );

    let complete = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
        "<Part><PartNumber>1</PartNumber><ETag>&quot;a54357aff0632cce46d942af68356b38&quot;</ETag></Part>",
        "<Part><PartNumber>2</PartNumber><ETag>\"0c78aef83f66abc1fa1e8477f296d394\"</ETag></Part>",
        "</CompleteMultipartUpload>"
    );
    assert_eq!(
        parse_complete_document(complete.as_bytes(), XML_MAX_BYTES)
            .expect("valid CompleteMultipartUpload XML"),
        vec![
            (1, "\"a54357aff0632cce46d942af68356b38\"".to_owned()),
            (2, "\"0c78aef83f66abc1fa1e8477f296d394\"".to_owned()),
        ]
    );
    assert_eq!(xml_escape("&<>\"'"), "&amp;&lt;&gt;&quot;&apos;");
}

#[test]
fn official_sigv4_get_vanilla_fixture_matches_all_three_stages() {
    let scope = CredentialScope {
        date: "20150830".to_owned(),
        region: "us-east-1".to_owned(),
        service: "service".to_owned(),
    };
    let headers = vec![
        HeaderEntry::new("Host", "example.amazonaws.com"),
        HeaderEntry::new("X-Amz-Date", "20150830T123600Z"),
    ];
    let signed_headers = vec!["host".to_owned(), "x-amz-date".to_owned()];
    let canonical = canonical_request(
        "GET",
        "/",
        &[],
        &headers,
        &signed_headers,
        EMPTY_PAYLOAD_SHA256,
    );
    assert_eq!(
        canonical,
        "GET\n/\n\nhost:example.amazonaws.com\n".to_owned()
            + "x-amz-date:20150830T123600Z\n\nhost;x-amz-date\n"
            + EMPTY_PAYLOAD_SHA256
    );

    let to_sign = string_to_sign("20150830T123600Z", &scope, &canonical);
    assert_eq!(
        to_sign,
        "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/service/aws4_request\n".to_owned()
            + "bb579772317eb040ac9ed261061d46c1f17a8133879d6129b6e1c25292927e63"
    );
    assert_eq!(
        signature_of(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            &scope,
            "20150830T123600Z",
            &canonical,
        ),
        "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
    );
    assert_eq!(
        credential_scope(&scope),
        "20150830/us-east-1/service/aws4_request"
    );
    assert_eq!(canonical_uri("/example space/"), "/example%20space/");
    assert_eq!(
        canonical_query(&[
            QueryEntry::new("Param2", "value2"),
            QueryEntry::new("Param1", "value1"),
        ]),
        "Param1=value1&Param2=value2"
    );
}

#[test]
fn upstream_sigv4_parsers_preserve_wire_fields_and_case_rules() {
    let authorization = parse_authorization_header(concat!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, ",
        "SignedHeaders=host;x-amz-date, Signature=abc123"
    ))
    .expect("valid Authorization header");
    assert_eq!(authorization.algorithm, SIGV4_ALGORITHM);
    assert_eq!(authorization.access_key_id, "AKIDEXAMPLE");
    assert_eq!(authorization.scope.date, "20150830");
    assert_eq!(authorization.signed_headers, ["host", "x-amz-date"]);
    assert_eq!(authorization.signature, "abc123");

    let query = vec![
        QueryEntry::new("x-amz-algorithm", SIGV4_ALGORITHM),
        QueryEntry::new(
            "x-amz-credential",
            "AKIDEXAMPLE/20150830/us-east-1/service/aws4_request",
        ),
        QueryEntry::new("x-amz-date", "20150830T123600Z"),
        QueryEntry::new("x-amz-expires", "900"),
        QueryEntry::new("x-amz-signedheaders", "host"),
        QueryEntry::new("x-amz-signature", "fedcba"),
    ];
    assert!(is_presigned(&query));
    let parsed = parse_presigned_query(&query).expect("valid presigned query");
    assert_eq!(parsed.algorithm, SIGV4_ALGORITHM);
    assert_eq!(parsed.access_key_id, "AKIDEXAMPLE");
    assert_eq!(parsed.amz_date, "20150830T123600Z");
    assert_eq!(parsed.expires_in, 900);
    assert_eq!(parsed.signed_headers, ["host"]);
    assert_eq!(parsed.signature, "fedcba");
    assert!(signatures_match("5FA00FA31553B73E", "5fa00fa31553b73e"));
    assert!(!signatures_match("5fa0", "5fa00fa31553b73e"));
}
