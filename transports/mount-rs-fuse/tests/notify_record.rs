//! Differential wire and replay tests for the new pure FUSE helpers.

use mount_rs_fuse::RequestHeader;
use mount_rs_fuse::{notify, record, session};

use mount_rs_core::MemoryFs;
use notify::{
    FUSE_NOTIFY_INVAL_ENTRY, FUSE_NOTIFY_INVAL_INODE, FuseNotifyInvalEntryOut,
    FuseNotifyInvalInodeOut, decode_notify, decode_notify_inval_entry, decode_notify_inval_inode,
    encode_notify_inval_entry, encode_notify_inval_inode,
};
use record::{
    TranscriptDirection, TranscriptFrame, TranscriptRecorder, decode_transcript, encode_transcript,
    replay_transcript,
};
use std::sync::Arc;

fn fixture(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap();
            let low = (pair[1] as char).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

#[test]
fn notify_wire_fixtures_match_upstream() {
    // Fixtures are generated from the pinned src/fuse/notify.ts encoders.
    let inode = encode_notify_inval_inode(FuseNotifyInvalInodeOut {
        ino: 0x0102_0304_0506_0708,
        off: -1,
        len: 0x0a0b_0c0d_0e0f_1011,
    });
    assert_eq!(
        inode,
        fixture("280000000200000000000000000000000807060504030201ffffffffffffffff11100f0e0d0c0b0a")
    );
    let decoded = decode_notify(&inode).unwrap();
    assert_eq!(decoded.code, FUSE_NOTIFY_INVAL_INODE);
    assert_eq!(
        decode_notify_inval_inode(&decoded.body).unwrap(),
        FuseNotifyInvalInodeOut {
            ino: 0x0102_0304_0506_0708,
            off: -1,
            len: 0x0a0b_0c0d_0e0f_1011,
        }
    );

    let entry = encode_notify_inval_entry(FuseNotifyInvalEntryOut {
        parent: 0x0102_0304_0506_0708,
        name: "é/δ".to_owned(),
        flags: 7,
    })
    .unwrap();
    assert_eq!(
        entry,
        fixture("2600000003000000000000000000000008070605040302010500000007000000c3a92fceb400")
    );
    let decoded = decode_notify(&entry).unwrap();
    assert_eq!(decoded.code, FUSE_NOTIFY_INVAL_ENTRY);
    assert_eq!(
        decode_notify_inval_entry(&decoded.body).unwrap(),
        FuseNotifyInvalEntryOut {
            parent: 0x0102_0304_0506_0708,
            name: "é/δ".to_owned(),
            flags: 7,
        }
    );
}

#[test]
fn notify_validation_matches_upstream_errors() {
    let nul = encode_notify_inval_entry(FuseNotifyInvalEntryOut {
        parent: 1,
        name: "bad\0name".to_owned(),
        flags: 0,
    })
    .unwrap_err();
    assert_eq!(nul.to_string(), "notify entry name contains a NUL byte");

    let too_long = encode_notify_inval_entry(FuseNotifyInvalEntryOut {
        parent: 1,
        name: "x".repeat(1025),
        flags: 0,
    })
    .unwrap_err();
    assert!(too_long.to_string().contains("over FUSE_NAME_MAX (1024)"));
    assert!(decode_notify(&[0; 15]).is_err());
    assert!(decode_notify_inval_inode(&[0; 23]).is_err());
    assert!(decode_notify_inval_entry(&[0; 16]).is_err());
}

#[test]
fn transcript_wire_fixture_matches_upstream() {
    // This is the byte-for-byte v1 output of pinned src/fuse/record.ts.
    assert_eq!(record::TRANSCRIPT_MAGIC, u32::from_be_bytes(*b"UMFT"));
    let frames = [
        TranscriptFrame {
            direction: TranscriptDirection::In,
            timestamp: 0x0102_0304_0506_0708,
            bytes: vec![1, 2, 3],
        },
        TranscriptFrame {
            direction: TranscriptDirection::Out,
            timestamp: 9,
            bytes: vec![0xaa, 0xbb],
        },
    ];
    let encoded = encode_transcript(&frames);
    assert_eq!(
        encoded,
        fixture(
            "554d4654010000000000000003000000080706050403020101020301000000020000000900000000000000aabb"
        )
    );
    assert_eq!(decode_transcript(&encoded).unwrap(), frames);
}

#[test]
fn recorder_stops_at_a_frame_boundary_and_copies_bytes() {
    let mut recorder = TranscriptRecorder::with_limit(Some(3));
    let mut source = vec![1, 2];
    recorder.tap_at(TranscriptDirection::In, &source, 100);
    source[0] = 9;
    recorder.tap_at(TranscriptDirection::Out, &[3], 105);

    assert!(!recorder.truncated);
    assert_eq!(recorder.bytes, 3);
    assert_eq!(recorder.frames[0].bytes, [1, 2]);
    assert_eq!(recorder.frames[1].timestamp, 5);

    recorder.tap_at(TranscriptDirection::In, &[4], 110);
    assert!(recorder.truncated);
    assert_eq!(recorder.bytes, 3);
    assert_eq!(recorder.frames.len(), 2);

    let mut clocked = TranscriptRecorder::new();
    clocked.tap(TranscriptDirection::In, &[5]);
    assert_eq!(clocked.frames.len(), 1);
    assert!(!clocked.encode().is_empty());
}

#[test]
fn malformed_transcripts_have_named_errors() {
    assert!(
        decode_transcript(&[])
            .unwrap_err()
            .to_string()
            .contains("shorter")
    );
    assert_eq!(
        decode_transcript(b"NOPE\x01\0\0\0")
            .unwrap_err()
            .to_string(),
        "not a transcript: bad magic"
    );
    assert!(
        decode_transcript(b"UMFT\x02\0\0\0")
            .unwrap_err()
            .to_string()
            .contains("version 2")
    );
    let mut bad_direction = fixture("554d465401000000");
    bad_direction.extend([2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(
        decode_transcript(&bad_direction)
            .unwrap_err()
            .to_string()
            .contains("direction 2")
    );
}

fn request_frame(opcode: u32, unique: u64, nodeid: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = RequestHeader {
        len: (40 + body.len()) as u32,
        opcode,
        unique,
        nodeid,
        uid: 0,
        gid: 0,
        pid: 0,
        total_extlen: 0,
    }
    .encode()
    .to_vec();
    bytes.extend_from_slice(body);
    bytes
}

#[tokio::test]
async fn replay_reports_request_reply_and_error_behavior() {
    let init: Vec<u8> = [7_u32, 41, 65536, u32::MAX, u32::MAX]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let frames = vec![
        TranscriptFrame {
            direction: TranscriptDirection::Out,
            timestamp: 0,
            bytes: vec![0; 3],
        },
        TranscriptFrame {
            direction: TranscriptDirection::In,
            timestamp: 1,
            bytes: vec![0; 2],
        },
        TranscriptFrame {
            direction: TranscriptDirection::In,
            timestamp: 2,
            bytes: request_frame(26, 1, 0, &init),
        },
        TranscriptFrame {
            direction: TranscriptDirection::In,
            timestamp: 3,
            bytes: request_frame(2, 2, 1, &1_u64.to_le_bytes()),
        },
        TranscriptFrame {
            direction: TranscriptDirection::In,
            timestamp: 4,
            bytes: request_frame(41, 3, 0, &[]),
        },
        TranscriptFrame {
            direction: TranscriptDirection::Out,
            timestamp: 5,
            bytes: vec![16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        },
    ];
    let mut session = session::FuseSession::new(Arc::new(MemoryFs::empty()));
    let report = replay_transcript(&mut session, &frames).await;

    assert_eq!(report.requests, 4);
    assert_eq!(report.replies, 1);
    assert_eq!(report.no_reply, 2);
    assert_eq!(report.opcodes.get("INIT"), Some(&1));
    assert_eq!(report.opcodes.get("FORGET"), Some(&1));
    assert_eq!(report.opcodes.get("NOTIFY_REPLY"), Some(&1));
    assert_eq!(report.failures.len(), 3);
    assert!(
        report.failures[0]
            .reason
            .contains("recorded reply does not decode")
    );
    assert!(
        report.failures[1]
            .reason
            .contains("request does not decode")
    );
    assert!(
        report.failures[2]
            .reason
            .contains("recorded reply says len 16, frame is 17")
    );
}
