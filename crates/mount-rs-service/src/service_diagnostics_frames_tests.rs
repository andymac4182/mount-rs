use super::*;
use mount_rs_core::diagnostics::{profile, storage};
use serde_json::{Value, json};

fn sized_object(size: usize) -> Vec<u8> {
    let prefix = b"{\"padding\":\"";
    let suffix = b"\"}";
    assert!(size >= prefix.len() + suffix.len());
    let mut bytes = prefix.to_vec();
    bytes.resize(size - suffix.len(), b'x');
    bytes.extend_from_slice(suffix);
    bytes
}

fn frames(bytes: &[u8]) -> Vec<Value> {
    encode(bytes)
        .unwrap()
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| {
            assert!(line.len() <= LINE_LIMIT);
            assert_eq!(line.last(), Some(&b'\n'));
            serde_json::from_slice(line.strip_prefix(PREFIX).unwrap()).unwrap()
        })
        .collect()
}

fn wire(frames: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(PREFIX);
        bytes.extend_from_slice(&serde_json::to_vec(frame).unwrap());
        bytes.push(b'\n');
    }
    bytes
}

fn poisoned_by(bytes: &[u8]) {
    let mut decoder = Decoder::default();
    let mut rejected = false;
    for line in std::str::from_utf8(bytes).unwrap().split_inclusive('\n') {
        if decoder.push_line(line).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected, "malformed frame was accepted");
    assert!(decoder.finish().is_err());
    assert!(decoder.push_line("unrelated log\n").is_err());
    let valid = encode(b"{}").unwrap();
    assert!(
        decoder
            .push_line(std::str::from_utf8(&valid).unwrap())
            .is_err()
    );
}

#[test]
fn full_maximum_u64_banks_preserve_every_original_json_byte() {
    let exact = u64::MAX;
    let storage = storage::Snapshot {
        in_flight: exact,
        forwarding_boxes: storage::ForwardingBoxes {
            sites: "napi_dynamic_provider_forwarding_future",
            calls: exact,
            requested_object_bytes: exact,
        },
        entries: storage::operation_names()
            .iter()
            .map(|name| storage::Entry {
                name,
                in_flight: exact,
                returned_rows: exact,
                returned_row_observations: exact,
                calls: exact,
                success: exact,
                error: exact,
                cancelled: exact,
                bytes: exact,
                elapsed_ns: exact,
                latency_log2_us: [exact; 32],
            })
            .collect(),
    };
    let mut profile = profile::snapshot();
    assert_eq!(storage.entries.len(), 116);
    assert_eq!(profile.entries.len(), 136);
    for entry in &mut profile.entries {
        entry.calls = exact;
        entry.elapsed_ns = exact;
        entry.units = exact;
    }
    let record = serde_json::to_vec(&json!({
        "schema":"mount-rs.cli-service-diagnostics.v2", "pid":321,
        "transport":"quic", "capture_context":"shutdown",
        "snapshot":{"counter":exact},
        "process_diagnostics":{
            "storage":{"available":true,"snapshot":storage},
            "profile":{"available":true,"snapshot":profile},
            "coverage":{"operation_names":storage::operation_names()}
        }
    }))
    .unwrap();
    assert!(record.len() > LINE_LIMIT);
    let encoded = encode(&record).unwrap();
    assert!(
        encoded
            .split_inclusive(|b| *b == b'\n')
            .all(|line| line.len() <= LINE_LIMIT)
    );
    assert_eq!(decode(&encoded).unwrap(), record);
    let value: Value = serde_json::from_slice(&decode(&encoded).unwrap()).unwrap();
    assert_eq!(value["snapshot"]["counter"].as_u64(), Some(exact));
    assert_eq!(
        value["process_diagnostics"]["storage"]["snapshot"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        116
    );
    assert_eq!(
        value["process_diagnostics"]["profile"]["snapshot"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        136
    );
}

#[test]
fn escaping_unicode_and_whitespace_are_lossless_across_chunk_boundaries() {
    let value = json!({"text":"🦀\n\"\\é".repeat(2000),"exact":9_007_199_254_740_993_u64});
    let mut bytes = b" \n".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(&value).unwrap());
    bytes.extend_from_slice(b" \t\n");
    assert_eq!(decode(&encode(&bytes).unwrap()).unwrap(), bytes);
}

#[test]
fn exact_chunk_and_logical_record_boundaries_round_trip() {
    for size in [
        2,
        CHUNK_BYTES - 1,
        CHUNK_BYTES,
        CHUNK_BYTES + 1,
        RECORD_LIMIT,
    ] {
        let bytes = if size == 2 {
            b"{}".to_vec()
        } else {
            sized_object(size)
        };
        let encoded = encode(&bytes).unwrap();
        assert_eq!(decode(&encoded).unwrap(), bytes);
        assert_eq!(frames(&bytes).len(), size.div_ceil(CHUNK_BYTES));
    }
    assert!(encode(&sized_object(RECORD_LIMIT + 1)).is_err());
}

#[test]
fn encoding_requires_one_valid_utf8_json_object() {
    for bytes in [
        &b""[..],
        b"[]",
        b"null",
        b"1",
        b"{}{}",
        b"{",
        b"{\"x\":\"\xff\"}",
    ] {
        assert!(encode(bytes).is_err());
    }
}

#[test]
fn streaming_ignores_other_lines_and_emits_only_complete_groups() {
    let record = sized_object(CHUNK_BYTES + 1);
    let encoded = encode(&record).unwrap();
    let lines: Vec<_> = std::str::from_utf8(&encoded).unwrap().lines().collect();
    let mut decoder = Decoder::default();
    assert!(
        decoder
            .push_line("service_diagnostics legacy record")
            .unwrap()
            .is_none()
    );
    assert!(decoder.push_line(lines[0]).unwrap().is_none());
    assert!(decoder.finish().is_err());
    assert!(decoder.push_line("unrelated log").unwrap().is_none());
    assert_eq!(decoder.push_line(lines[1]).unwrap().unwrap(), record);
    assert!(decoder.finish().is_ok());
    assert!(decoder.push_line(lines[0]).unwrap().is_none());
    assert!(decoder.push_line(lines[1]).unwrap().is_some());
    assert!(decoder.finish().is_ok());
}

#[test]
fn single_group_decode_rejects_missing_truncated_and_multiple_groups() {
    let encoded = encode(&sized_object(CHUNK_BYTES + 1)).unwrap();
    let first = encoded.iter().position(|b| *b == b'\n').unwrap() + 1;
    assert!(decode(&encoded[..first]).is_err());
    assert!(decode(&encoded[..encoded.len() - 1]).is_err());
    assert!(decode(b"unrelated log\n").is_err());
    assert!(decode(&[encoded.clone(), encoded].concat()).is_err());
}

#[test]
fn duplicate_reordered_and_interleaved_groups_poison_the_decoder() {
    let a = frames(&sized_object(CHUNK_BYTES + 1));
    let b = frames(&sized_object(CHUNK_BYTES + 2));
    for group in [
        vec![a[0].clone(), a[0].clone()],
        vec![a[1].clone(), a[0].clone()],
        vec![a[0].clone(), b[0].clone()],
        vec![a[0].clone(), b[1].clone()],
    ] {
        poisoned_by(&wire(&group));
    }
}

#[test]
fn invalid_metadata_unknown_fields_and_base64_cannot_resynchronize() {
    let original = frames(b"{}").remove(0);
    for (key, value) in [
        ("schema", json!("mount-rs.service-diagnostic-frame.v99")),
        ("index", json!(1)),
        ("index", json!(-1)),
        ("count", json!(0)),
        ("count", json!(129)),
        ("count", json!(u64::MAX)),
        ("count", json!(1.5)),
        ("count", json!("1")),
        ("bytes", json!(0)),
        ("bytes", json!(RECORD_LIMIT + 1)),
        ("bytes", json!(3)),
        ("digest", json!("bad")),
        ("payload_base64", json!("!!!!")),
        ("payload_base64", json!("eA==")),
        ("extra", json!(true)),
    ] {
        let mut frame = original.clone();
        frame[key] = value;
        poisoned_by(&wire(&[frame]));
    }
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("digest");
    poisoned_by(&wire(&[missing]));
}

#[test]
fn changed_digest_and_valid_base64_payload_corruption_are_rejected() {
    let original = frames(&sized_object(CHUNK_BYTES + 1));
    let mut stale = original.clone();
    for frame in &mut stale {
        frame["digest"] = json!("0".repeat(64));
    }
    poisoned_by(&wire(&stale));
    let mut conflicting = original.clone();
    conflicting[1]["digest"] = json!("0".repeat(64));
    poisoned_by(&wire(&conflicting));
    let mut corrupted = frames(b"{}");
    corrupted[0]["payload_base64"] = json!(BASE64.encode(b"[]"));
    poisoned_by(&wire(&corrupted));
}

#[test]
fn matching_digest_does_not_make_a_scalar_or_invalid_json_a_record() {
    for bytes in [&b"null"[..], b"[]", b"{", b"{\"x\":\"\xff\"}"] {
        let frame = json!({"schema":SCHEMA,"index":0,"count":1,"bytes":bytes.len(),
            "digest":digest_hex(bytes),"payload_base64":BASE64.encode(bytes)});
        poisoned_by(&wire(&[frame]));
    }
}

#[test]
fn malformed_lookalikes_duplicate_fields_and_oversized_lines_poison() {
    for line in [
        "service_diagnostics_frame",
        "service_diagnostics_frameX {}",
        "service_diagnostics_frame {",
        "service_diagnostics_frame {}\n\n",
    ] {
        poisoned_by(line.as_bytes());
    }
    let frame = frames(b"{}").remove(0);
    let raw = serde_json::to_string(&frame).unwrap();
    let duplicate = format!("service_diagnostics_frame {{\"index\":0,{}\n", &raw[1..]);
    poisoned_by(duplicate.as_bytes());
    let mut large = frame;
    large["payload_base64"] = json!("A".repeat(LINE_LIMIT));
    poisoned_by(&wire(&[large]));
}
