use super::*;
use mount_rs_core::diagnostics::object_store::{self, ClientRole, HttpMethod, Observer};
use serde_json::{Value, json};
use std::cell::Cell;

fn metadata() -> Capture {
    Capture {
        pid: 321,
        sequence: 17,
        observed_unix_ms: 9_007_199_254_740_993,
        context: CaptureContext::WorkerBoundary,
        generation: Some(2),
    }
}

fn frames(sample: &Sample) -> Vec<Value> {
    let mut bytes = Vec::new();
    sample.write(&mut bytes).unwrap();
    let lines: Vec<_> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
    assert_eq!(
        lines.len(),
        FRAME_COUNT,
        "enabled sample did not emit seven complete records"
    );
    lines
        .into_iter()
        .map(|line| {
            assert!(line.len() <= RECORD_LIMIT);
            assert!(line.starts_with(PREFIX));
            assert_eq!(line.last(), Some(&b'\n'));
            serde_json::from_slice(&line[PREFIX.len()..]).unwrap()
        })
        .collect()
}

fn wire(values: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(PREFIX);
        bytes.extend_from_slice(&serde_json::to_vec(value).unwrap());
        bytes.push(b'\n');
    }
    bytes
}

#[test]
fn one_real_bank_capture_is_bound_to_all_seven_frames_before_later_changes() {
    let observer = Observer::isolated();
    let client = observer.client(ClientRole::PrimaryDataMixed);
    client.attempt(HttpMethod::Get, Some(3)).headers(200).eof();
    let sampled = observer.snapshot().unwrap();
    let calls = Cell::new(0);
    let sample = capture(true, metadata(), || {
        calls.set(calls.get() + 1);
        observer.snapshot()
    })
    .unwrap()
    .unwrap();
    assert_eq!(calls.get(), 1, "actual bank snapshot must be captured once");
    assert_eq!(sample.snapshot(), &sampled);
    drop(client);
    assert_eq!(observer.snapshot().unwrap().clients[0].live, 0);
    let records = frames(&sample);
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record["schema"], SCHEMA);
        assert_eq!(record["capture"], serde_json::to_value(metadata()).unwrap());
        assert_eq!(record["index"], index);
        assert_eq!(record["count"], FRAME_COUNT);
        assert_eq!(record["scope"], "process_cumulative");
        assert_eq!(record["coverage"]["capture_atomic"], false);
        assert_eq!(record["coverage"]["application_drain_proven"], false);
        assert_eq!(record["coverage"]["physical_iops_available"], false);
    }
    assert_eq!(records[0]["snapshot"]["live"], 1);
    assert_eq!(records[0]["snapshot"]["http"][0]["attempts_started"], 1);
    assert_eq!(decode(&wire(&records)).unwrap(), sample);
}

#[test]
fn disabled_and_unavailable_capture_publish_no_records_and_no_measured_zero() {
    let invalid = Capture {
        pid: 0,
        sequence: 0,
        ..metadata()
    };
    assert!(
        capture(false, invalid, || panic!("disabled snapshot closure"))
            .unwrap()
            .is_none()
    );
    let calls = Cell::new(0);
    assert!(
        capture(true, metadata(), || {
            calls.set(calls.get() + 1);
            None
        })
        .unwrap()
        .is_none()
    );
    assert_eq!(calls.get(), 1);
}

fn maximum_http() -> object_store::HttpSnapshot {
    object_store::HttpSnapshot {
        attempts_started: u64::MAX,
        attempts_inflight: u64::MAX,
        header_responses: u64::MAX,
        transport_errors: u64::MAX,
        cancelled_before_headers: u64::MAX,
        offered_bytes: u64::MAX,
        offered_known: u64::MAX,
        offered_unknown: u64::MAX,
        known_extra_future_boxes: u64::MAX,
        known_extra_response_body_boxes: u64::MAX,
        bodies_started: u64::MAX,
        bodies_inflight: u64::MAX,
        body_eof: u64::MAX,
        body_errors: u64::MAX,
        body_dropped: u64::MAX,
        body_bytes: u64::MAX,
        body_chunks: u64::MAX,
        dispatch_elapsed_ns: u64::MAX,
        dispatch_max_ns: u64::MAX,
        body_elapsed_ns: u64::MAX,
        body_max_ns: u64::MAX,
        status: [u64::MAX; 6],
    }
}

fn maximum_snapshot() -> object_store::Snapshot {
    object_store::Snapshot {
        clients: [object_store::ClientSnapshot {
            constructed: u64::MAX,
            released: u64::MAX,
            live: u64::MAX,
            http: [maximum_http(); 6],
        }; 6],
        bundles: object_store::BundleSnapshot {
            builds_started: u64::MAX,
            builds_inflight: u64::MAX,
            committed: u64::MAX,
            failed: u64::MAX,
            abandoned: u64::MAX,
            live: u64::MAX,
            released: u64::MAX,
            elapsed_ns: u64::MAX,
            max_ns: u64::MAX,
        },
        cache: object_store::CacheSnapshot {
            created: u64::MAX,
            released: u64::MAX,
            live: u64::MAX,
            resident_entries: u64::MAX,
            payload_bytes: u64::MAX,
            unknown_live: u64::MAX,
        },
        saturated: true,
        concurrent_activity: true,
    }
}

#[test]
fn every_maximum_u64_field_survives_all_seven_bounded_records() {
    let expected = maximum_snapshot();
    let capture_id = Capture {
        pid: u32::MAX,
        sequence: u64::MAX,
        observed_unix_ms: u64::MAX,
        context: CaptureContext::Periodic,
        generation: Some(u64::MAX),
    };
    let sample = capture(true, capture_id, || Some(expected))
        .unwrap()
        .unwrap();
    let records = frames(&sample);
    assert_eq!(records.len(), 7);
    for record in &records {
        assert_eq!(record["capture"]["sequence"].as_u64(), Some(u64::MAX));
        assert_eq!(
            record["capture"]["observed_unix_ms"].as_u64(),
            Some(u64::MAX)
        );
        assert_eq!(record["quality"]["saturated"], true);
        assert_eq!(record["quality"]["concurrent_activity"], true);
    }
    assert_eq!(
        records[0]["snapshot"]["http"][0]["body_bytes"].as_u64(),
        Some(u64::MAX)
    );
    assert!(!records[0]["snapshot"]["http"][0]["body_bytes"].is_string());
    assert_eq!(decode(&wire(&records)).unwrap().snapshot(), &expected);
    assert_eq!(decode(&wire(&records)).unwrap().capture(), &capture_id);
}

#[test]
fn complete_indexed_set_can_arrive_out_of_order() {
    let sample = capture(true, metadata(), || Some(maximum_snapshot()))
        .unwrap()
        .unwrap();
    let mut records = frames(&sample);
    records.reverse();
    assert_eq!(decode(&wire(&records)).unwrap(), sample);
}

#[test]
fn decoder_rejects_missing_duplicate_conflicting_and_cross_capture_frames() {
    let sample = capture(true, metadata(), || Some(maximum_snapshot()))
        .unwrap()
        .unwrap();
    let original = frames(&sample);
    let mut cases = Vec::new();
    cases.push(original[..6].to_vec());
    let mut duplicate = original.clone();
    duplicate[1] = duplicate[0].clone();
    cases.push(duplicate);
    let mut cross = original.clone();
    cross[1]["capture"]["sequence"] = json!(18);
    cases.push(cross);
    let mut pid = original.clone();
    pid[1]["capture"]["pid"] = json!(123);
    cases.push(pid);
    let mut generation = original.clone();
    generation[1]["capture"]["generation"] = json!(3);
    cases.push(generation);
    let mut quality = original.clone();
    quality[1]["quality"]["saturated"] = json!(false);
    cases.push(quality);
    let mut wrong_role = original.clone();
    wrong_role[1]["role"] = original[0]["role"].clone();
    cases.push(wrong_role);
    let mut wrong_count = original.clone();
    wrong_count[1]["count"] = json!(8);
    cases.push(wrong_count);
    let mut wrong_scope = original.clone();
    wrong_scope[1]["scope"] = json!("PRIVATE_SCOPE");
    cases.push(wrong_scope);
    let mut swapped_methods = original.clone();
    swapped_methods[1]["methods"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    cases.push(swapped_methods);
    for records in cases {
        assert!(decode(&wire(&records)).is_err());
    }
    let mut bytes = wire(&original);
    bytes.pop();
    assert!(decode(&bytes).is_err());
    let mut bytes = wire(&original);
    bytes.extend_from_slice(&wire(&original[..1]));
    assert!(decode(&bytes).is_err());
}

#[test]
fn decoder_requires_exact_typed_fixed_rows_without_numeric_coercion() {
    let sample = capture(true, metadata(), || Some(maximum_snapshot()))
        .unwrap()
        .unwrap();
    let original = frames(&sample);
    for malformed in [
        json!("18446744073709551615"),
        json!(-1),
        json!(1.0),
        json!(true),
        Value::Null,
    ] {
        let mut records = original.clone();
        records[0]["snapshot"]["http"][0]["body_bytes"] = malformed;
        assert!(decode(&wire(&records)).is_err());
    }
    let mut records = original.clone();
    records[0]["snapshot"]["http"][0]["private_key"] = json!("PRIVATE_SENTINEL");
    let error = decode(&wire(&records)).unwrap_err();
    assert!(!error.to_string().contains("PRIVATE"));
    let mut records = original.clone();
    records[0]["snapshot"]["http"][0]
        .as_object_mut()
        .unwrap()
        .remove("body_bytes");
    assert!(decode(&wire(&records)).is_err());
    let mut records = original.clone();
    records[0]["snapshot"]["http"].as_array_mut().unwrap().pop();
    assert!(decode(&wire(&records)).is_err());
    let mut records = original.clone();
    records[0]["snapshot"]["http"][0]["status"]
        .as_array_mut()
        .unwrap()
        .push(json!(0));
    assert!(decode(&wire(&records)).is_err());
    let mut records = original.clone();
    records[6]["snapshot"]["cache"]["payload_bytes"] = json!(1.0);
    assert!(decode(&wire(&records)).is_err());
    let mut bytes = wire(&original);
    let raw = String::from_utf8(bytes.clone()).unwrap();
    let raw = raw.replacen("\"count\":7", "\"count\":7,\"count\":7", 1);
    bytes = raw.into_bytes();
    assert!(
        decode(&bytes).is_err(),
        "duplicate JSON fields must fail closed"
    );
    let over = [PREFIX, &vec![b'x'; RECORD_LIMIT], b"\n"].concat();
    assert!(decode(&over).is_err());
}

#[test]
fn bounded_serializer_never_publishes_partial_or_caller_error_text() {
    #[derive(Serialize)]
    struct Large {
        private: String,
    }
    let mut output = Vec::new();
    assert!(
        write_frame(
            &mut output,
            &Large {
                private: "PRIVATE_SENTINEL".repeat(RECORD_LIMIT)
            }
        )
        .is_err()
    );
    assert!(output.is_empty());
    struct Fails;
    impl Serialize for Fails {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("PRIVATE_SERIALIZER_ERROR"))
        }
    }
    assert!(write_frame(&mut output, &Fails).is_err());
    assert!(output.is_empty());
}

#[test]
fn sink_failure_leaves_an_unacceptable_partial_set() {
    struct Stops {
        accepted: Vec<u8>,
        frames: usize,
    }
    impl std::io::Write for Stops {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.frames == 2 {
                return Err(std::io::Error::other("blocked sink"));
            }
            self.frames += 1;
            self.accepted.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sample = capture(true, metadata(), || Some(maximum_snapshot()))
        .unwrap()
        .unwrap();
    let mut sink = Stops {
        accepted: Vec::new(),
        frames: 0,
    };
    assert!(sample.write(&mut sink).is_err());
    assert_eq!(sink.frames, 2);
    assert!(decode(&sink.accepted).is_err());
}

#[test]
fn zero_sequence_and_generation_remain_exact_startup_identity() {
    let identity = Capture {
        sequence: 0,
        generation: Some(0),
        ..metadata()
    };
    let sample = capture(true, identity, || Some(object_store::Snapshot::default()))
        .unwrap()
        .unwrap();
    let records = frames(&sample);
    assert_eq!(decode(&wire(&records)).unwrap().capture(), &identity);
}

#[test]
fn valid_json_at_frame_limit_is_accepted_and_one_byte_over_is_rejected() {
    let sample = capture(true, metadata(), || Some(maximum_snapshot()))
        .unwrap()
        .unwrap();
    let base = wire(&frames(&sample));
    let first_len = base.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    assert!(first_len < RECORD_LIMIT);
    let padded = |target: usize| {
        let mut bytes = base[..first_len - 1].to_vec();
        bytes.resize(target - 1, b' '); // Legal JSON whitespace before the frame LF.
        bytes.push(b'\n');
        bytes.extend_from_slice(&base[first_len..]);
        bytes
    };
    let exact = padded(RECORD_LIMIT);
    assert!(exact.len() < FRAME_COUNT * RECORD_LIMIT);
    assert_eq!(decode(&exact).unwrap(), sample);
    let over = padded(RECORD_LIMIT + 1);
    assert!(
        over.len() < FRAME_COUNT * RECORD_LIMIT,
        "must isolate per-record cap from aggregate cap"
    );
    assert!(
        decode(&over).is_err(),
        "valid JSON frame over16KiB must fail closed"
    );
}
