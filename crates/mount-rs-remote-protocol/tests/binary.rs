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
    let request = IoRequest {
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
    let request = IoRequest {
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
