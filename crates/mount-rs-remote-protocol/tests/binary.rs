use mount_rs_remote_protocol::binary::{Header, Kind, MAX_IO_BYTES};

#[test]
fn binary_header_rejects_oversized_io_before_body() {
    assert!(Header::new(Kind::Write, 1, 64, MAX_IO_BYTES + 1, 0).is_err());
    assert!(Header::new(Kind::Read, 0, 64, 0, 1).is_err());
    assert!(Header::new(Kind::Read, 1, 64, 0, MAX_IO_BYTES + 1).is_err());
}

use mount_rs_remote_protocol::binary::{self, Incoming, IoRequest, IoResult};
use mount_rs_remote_protocol::{Message, Operation, OperationName};

#[tokio::test]
async fn raw_binary_roundtrip_and_direct_caller_read() {
    let request: IoRequest = IoRequest {
        drive_id: "data".into(),
        handle: 17,
        position: Some(9),
    };
    let data: Vec<u8> = (0..4096).map(|n| n as u8).collect();
    let mut encoded = Vec::new();
    binary::write_request(&mut encoded, 7, &request, 0, Some(&data))
        .await
        .unwrap();
    assert!(encoded.len() < data.len() + 128);
    let mut reader = encoded.as_slice();
    let header = Header::read(&mut reader).await.unwrap();
    assert_eq!(header.payload_len, 4096);
    match binary::read_body(&mut reader, header).await.unwrap() {
        Incoming::Write {
            request_id,
            request: r,
            data: d,
        } => {
            assert_eq!(request_id, 7);
            assert_eq!(r.handle, 17);
            assert_eq!(d, data);
        }
        _ => panic!("wrong kind"),
    }
    encoded.clear();
    binary::write_result(&mut encoded, 7, Ok(IoResult::Read(data.clone())))
        .await
        .unwrap();
    let mut caller = [0; 4096];
    assert_eq!(
        binary::read_result(&mut encoded.as_slice(), 7, true, &mut caller, 0)
            .await
            .unwrap()
            .unwrap(),
        4096
    );
    assert_eq!(caller.as_slice(), data);
}

#[test]
fn unknown_kind_version_reserved_bytes_and_lengths_rejected() {
    let h = Header::new(Kind::Write, 1, 64, 4096, 0)
        .unwrap()
        .encode()
        .unwrap();
    for (index, value) in [(0, b'X'), (3, b'1'), (4, 99), (5, 1), (31, 1)] {
        let mut invalid = h;
        invalid[index] = value;
        assert!(Header::decode(invalid).is_err());
    }
    let mut invalid = h;
    invalid[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(Header::decode(invalid).is_err());
    assert!(Header::new(Kind::ReadResult, 1, 0, 10, 9).is_err());
    assert!(Header::new(Kind::Error, 1, 65, 0, 0).is_err());
}

#[tokio::test]
async fn truncation_and_response_identity_fail_before_caller_mutation() {
    let encoded = Header::new(Kind::ReadResult, 9, 0, 4, 4)
        .unwrap()
        .encode()
        .unwrap();
    let mut caller = [42; 4];
    assert!(
        binary::read_result(&mut encoded.as_slice(), 8, true, &mut caller, 0)
            .await
            .is_err()
    );
    assert_eq!(caller, [42; 4]);
    assert!(Header::read(&mut &encoded[..31]).await.is_err());
    assert!(
        binary::read_result(&mut encoded.as_slice(), 9, true, &mut caller, 0)
            .await
            .is_err()
    );
    let mut small = [42; 3];
    assert!(
        binary::read_result(&mut encoded.as_slice(), 9, true, &mut small, 0)
            .await
            .is_err()
    );
    assert_eq!(small, [42; 3]);
    let mut wrong = Vec::new();
    binary::write_result(&mut wrong, 9, Ok(IoResult::Write(5)))
        .await
        .unwrap();
    assert!(
        binary::read_result(&mut wrong.as_slice(), 9, false, &mut [], 4)
            .await
            .is_err()
    );
    assert!(
        binary::read_result(&mut wrong.as_slice(), 9, true, &mut caller, 0)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oversize_writers_emit_nothing_and_controls_use_configured_capacity() {
    let request: IoRequest = IoRequest {
        drive_id: "d".into(),
        handle: 1,
        position: None,
    };
    let mut encoded = Vec::new();
    assert!(
        binary::write_request(
            &mut encoded,
            1,
            &request,
            0,
            Some(&vec![0; MAX_IO_BYTES + 1])
        )
        .await
        .is_err()
    );
    assert!(encoded.is_empty());
    let message = Message::Request {
        request_id: 1,
        drive_id: "d".into(),
        operation: Operation {
            name: OperationName::ReaddirBounded,
            body: serde_json::json!({"metadata":"a".repeat(128*1024)}),
        },
    };
    binary::write_control(&mut encoded, &message).await.unwrap();
    assert_eq!(
        binary::read_control(&mut encoded.as_slice()).await.unwrap(),
        message
    );
    encoded.clear();
    let message = Message::Renew {
        bearer: "a".repeat(binary::MAX_CONTROL_BYTES + 1),
    };
    assert!(binary::write_control(&mut encoded, &message).await.is_err());
    assert!(encoded.is_empty());
}

#[tokio::test]
async fn canonical_inline_metadata_boundaries_and_positions() {
    for drive in ["a", "AZaz09-_.", &"a".repeat(64)] {
        for position in [None, Some(0), Some(u64::MAX)] {
            let request = IoRequest {
                drive_id: drive,
                handle: u64::MAX,
                position,
            };
            let mut bytes = Vec::new();
            binary::write_request(&mut bytes, 1, &request, MAX_IO_BYTES, None)
                .await
                .unwrap();
            let mut reader = bytes.as_slice();
            let h = Header::read(&mut reader).await.unwrap();
            assert_eq!(h.control_len, 18 + drive.len());
            assert_eq!(reader[0], drive.len() as u8);
            assert_eq!(reader[1], u8::from(position.is_some()));
            assert_eq!(&reader[2..10], &u64::MAX.to_be_bytes());
            assert_eq!(&reader[10..18], &position.unwrap_or(0).to_be_bytes());
            let decoded = binary::read_io_metadata(&mut reader, h).await.unwrap();
            assert_eq!(decoded.drive_id.as_ref(), drive);
            assert_eq!(decoded.handle, u64::MAX);
            assert_eq!(decoded.position, position);
            assert!(reader.is_empty());
        }
    }
}

#[tokio::test]
async fn malformed_metadata_and_obsolete_json_are_rejected() {
    let request = IoRequest {
        drive_id: "abc",
        handle: 0,
        position: None,
    };
    let mut encoded = Vec::new();
    binary::write_request(&mut encoded, 1, &request, 0, None)
        .await
        .unwrap();
    let valid = &encoded[binary::HEADER_BYTES..];
    let h = Header::new(Kind::Read, 1, valid.len(), 0, 0).unwrap();
    for (index, byte) in [
        (0, 0),
        (0, 2),
        (0, 4),
        (0, 65),
        (1, 2),
        (1, 255),
        (10, 1),
        (18, b'/'),
        (18, 0xff),
    ] {
        let mut invalid = valid.to_vec();
        invalid[index] = byte;
        assert!(
            binary::read_io_metadata(&mut invalid.as_slice(), h)
                .await
                .is_err(),
            "accepted mutation at {index}"
        );
    }
    for end in 0..valid.len() {
        assert!(
            binary::read_io_metadata(&mut &valid[..end], h)
                .await
                .is_err()
        );
    }
    let json = br#"{"drive_id":"abc","handle":0,"position":null}"#;
    let json_header = Header::new(Kind::Read, 1, json.len(), 0, 0).unwrap();
    assert!(
        binary::read_io_metadata(&mut json.as_slice(), json_header)
            .await
            .is_err()
    );
    // Public Header fields cannot evade admission or overflow the stack buffer.
    for control_len in [0, 18, 83, usize::MAX] {
        let forged = Header { control_len, ..h };
        let mut reader = valid;
        assert!(binary::read_body(&mut reader, forged).await.is_err());
        assert_eq!(reader, valid);
    }
}

#[tokio::test]
async fn invalid_outbound_ids_emit_nothing() {
    for drive in ["", "a/b", "a b", "é", &"a".repeat(65)] {
        let mut bytes = Vec::new();
        let request = IoRequest {
            drive_id: drive,
            handle: 0,
            position: None,
        };
        assert!(
            binary::write_request(&mut bytes, 1, &request, 0, None)
                .await
                .is_err()
        );
        assert!(bytes.is_empty());
    }
}

#[tokio::test]
async fn maximum_raw_write_roundtrip_keeps_payload_separate() {
    let data = vec![0xa5; MAX_IO_BYTES];
    let request = IoRequest {
        drive_id: "drive",
        handle: 23,
        position: Some(u64::MAX),
    };
    let mut bytes = Vec::new();
    binary::write_request(&mut bytes, 3, &request, 0, Some(&data))
        .await
        .unwrap();
    assert_eq!(bytes.len(), binary::HEADER_BYTES + 23 + MAX_IO_BYTES);
    let mut reader = bytes.as_slice();
    let h = Header::read(&mut reader).await.unwrap();
    let metadata = binary::read_io_metadata(&mut reader, h).await.unwrap();
    assert_eq!(metadata.drive_id.as_ref(), "drive");
    assert_eq!(reader, data);
    let mut reader = &bytes[binary::HEADER_BYTES..];
    match binary::read_body(&mut reader, h).await.unwrap() {
        Incoming::Write { data: raw, .. } => assert_eq!(raw, data),
        _ => panic!("wrong kind"),
    }
    assert!(reader.is_empty());
    assert!(
        binary::read_body(&mut &bytes[binary::HEADER_BYTES..bytes.len() - 1], h)
            .await
            .is_err()
    );
}
