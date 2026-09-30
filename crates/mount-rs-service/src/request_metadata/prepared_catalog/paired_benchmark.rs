//! Explicit release-only, paired grant traversal measurement.
//! No catalog I/O, JWT verification, transport, storage, or cluster qualification.
use super::{PreparedCatalog, PreparedCatalogCache};
use crate::auth::{authorize_drive, authorize_prepared_drive};
use crate::catalog::{CatalogSnapshot, Permission};
use crate::dispatch::SessionIdentity;
use crate::request_metadata::{
    AUDIT_CANDIDATE_VISITS, AuditRecord, Buffered, MatchingGrants, claim_pointer, policy_matches,
    write_audit,
};
use mount_rs_remote_protocol::OperationName;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use serde_json::{Value, json};
use std::hint::black_box;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAIRS: usize = 8;
const PILOT_CYCLES: usize = 48;
const TARGET_WINDOW_NS: u128 = 25_000_000;
const MIN_CYCLES: usize = 24;
const MAX_CYCLES: usize = 3_072;
const INTERNAL_BOUND: Duration = Duration::from_secs(55);

// Exact pre-index audit traversal/predicate. Unlike the candidate's cfg(test)
// iterator, it adds no per-grant TLS observation to the full-map reference.
struct FullScanGrants<'a> {
    catalog: &'a CatalogSnapshot,
    identity: &'a SessionIdentity,
    drive_id: &'a str,
}
impl Serialize for FullScanGrants<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for (id, grant) in &self.catalog.grants {
            if grant.partition_id == self.identity.partition_id
                && grant.policy_id == self.identity.policy_id
                && grant.drives.contains_key(self.drive_id)
                && grant.claim_conditions.iter().all(|(pointer, expected)| {
                    claim_pointer(&self.identity.claims, pointer).and_then(Value::as_str)
                        == Some(expected.as_str())
                })
            {
                sequence.serialize_element(id)?;
            }
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct FullScanAuditRecord<'a> {
    event: &'static str,
    partition_id: &'a str,
    drive_id: &'a str,
    grant_ids: FullScanGrants<'a>,
    operation: OperationName,
    request_id: u64,
    outcome: &'static str,
}
fn write_full_scan<W: Write>(
    writer: &mut W,
    record: &FullScanAuditRecord<'_>,
) -> Result<(), serde_json::Error> {
    // Same stack buffer, serializer, LF and flush sequence as write_audit.
    let mut buffered = Buffered {
        writer,
        bytes: [0; 4096],
        len: 0,
    };
    serde_json::to_writer(&mut buffered, record)?;
    buffered.write_all(b"\n").map_err(serde_json::Error::io)?;
    buffered.flush().map_err(serde_json::Error::io)
}

struct Query {
    identity: SessionIdentity,
    drive: String,
    expected_permission: Option<Permission>,
    expected_audit: Vec<u8>,
}
fn expected_audit(partition: &str, drive: &str, grants: &[&str]) -> Vec<u8> {
    // Fixture IDs contain only ASCII letters/digits; this independently fixes
    // the original record-field order, grant-ID order and terminating LF.
    let ids = grants
        .iter()
        .map(|id| format!("\"{id}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"event\":\"remote_access\",\"partition_id\":\"{partition}\",\"drive_id\":\"{drive}\",\"grant_ids\":[{ids}],\"operation\":\"handle_write\",\"request_id\":42,\"outcome\":\"ok\"}}\n"
    )
    .into_bytes()
}
fn query(index: usize) -> Query {
    let partition = format!("p{:05}", index / 2);
    let drive = format!("d{index:05}");
    let grant = format!("g{index:05}");
    Query {
        identity: SessionIdentity {
            partition_id: partition.clone(),
            policy_id: "policy".into(),
            issuer: "https://issuer.example.com".into(),
            subject: format!("c{index:05}"),
            signing_algorithm: "ES256".into(),
            claims: json!({"aud":"mount-rs","sub":format!("c{index:05}")}),
            expires_at: i64::MAX,
        },
        expected_audit: expected_audit(&partition, &drive, &[&grant]),
        drive,
        expected_permission: Some(Permission::Write),
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Full,
    Indexed,
}
fn audit_and_check(
    writer: &Mutex<Vec<u8>>,
    query: &Query,
    permission: Option<Permission>,
    write: impl FnOnce(&mut Vec<u8>) -> Result<(), serde_json::Error>,
) -> usize {
    assert_eq!(black_box(permission), query.expected_permission);
    let mut output = writer.lock().unwrap();
    output.clear();
    write(&mut output).unwrap();
    assert_eq!(output.as_slice(), query.expected_audit.as_slice());
    assert_eq!(output.last(), Some(&b'\n'));
    black_box(output.as_slice()).len()
}
fn cycle(
    mode: Mode,
    source: &Arc<CatalogSnapshot>,
    cache: &PreparedCatalogCache,
    query: &Query,
    writer: &Mutex<Vec<u8>>,
) -> usize {
    let policy = &source.issuer_policies[&query.identity.policy_id];
    assert!(policy_matches(
        black_box(policy),
        black_box(&query.identity)
    ));
    match mode {
        Mode::Full => {
            // Match the owned source Arc observation; no prepared-index work.
            let current = Arc::clone(black_box(source));
            let permission = authorize_drive(
                black_box(&current),
                &query.identity.policy_id,
                black_box(&query.identity.claims),
                &query.identity.partition_id,
                &query.drive,
            );
            audit_and_check(writer, query, permission, |output| {
                write_full_scan(
                    output,
                    &FullScanAuditRecord {
                        event: "remote_access",
                        partition_id: &query.identity.partition_id,
                        drive_id: &query.drive,
                        grant_ids: FullScanGrants {
                            catalog: &current,
                            identity: &query.identity,
                            drive_id: &query.drive,
                        },
                        operation: OperationName::HandleWrite,
                        request_id: 42,
                        outcome: "ok",
                    },
                )
            })
        }
        Mode::Indexed => {
            let read = cache.prepare(Arc::clone(black_box(source))).unwrap();
            assert!(!read.waited, "single-thread hot cache unexpectedly waited");
            let permission = authorize_prepared_drive(
                black_box(&read.catalog),
                &query.identity.policy_id,
                black_box(&query.identity.claims),
                &query.identity.partition_id,
                &query.drive,
            );
            audit_and_check(writer, query, permission, |output| {
                write_audit(
                    output,
                    &AuditRecord {
                        event: "remote_access",
                        partition_id: &query.identity.partition_id,
                        drive_id: &query.drive,
                        grant_ids: MatchingGrants {
                            catalog: &read.catalog,
                            identity: &query.identity,
                            drive_id: &query.drive,
                        },
                        operation: OperationName::HandleWrite,
                        request_id: 42,
                        outcome: "ok",
                    },
                )
            })
        }
    }
}
fn ensure_budget(deadline: Instant) {
    assert!(
        Instant::now() < deadline,
        "paired grant benchmark55s budget exhausted"
    );
}
fn batch(
    mode: Mode,
    source: &Arc<CatalogSnapshot>,
    cache: &PreparedCatalogCache,
    queries: &[Query; 3],
    writer: &Mutex<Vec<u8>>,
    cycles: usize,
    deadline: Instant,
) -> u64 {
    let mut bytes = 0u64;
    for index in 0..cycles {
        if index.is_multiple_of(64) {
            ensure_budget(deadline);
        }
        bytes += u64::try_from(cycle(mode, source, cache, &queries[index % 3], writer)).unwrap();
    }
    black_box(bytes)
}
fn process_cpu_us() -> (u64, u64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let micros = |time: libc::timeval| {
        u64::try_from(time.tv_sec).unwrap() * 1_000_000 + u64::try_from(time.tv_usec).unwrap()
    };
    (micros(usage.ru_utime), micros(usage.ru_stime))
}
fn native_window(
    mode: Mode,
    source: &Arc<CatalogSnapshot>,
    cache: &PreparedCatalogCache,
    queries: &[Query; 3],
    writer: &Mutex<Vec<u8>>,
    cycles: usize,
    deadline: Instant,
) -> Value {
    ensure_budget(deadline);
    let cpu_before = process_cpu_us();
    let started = Instant::now();
    let bytes = batch(mode, source, cache, queries, writer, cycles, deadline);
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap();
    let cpu_after = process_cpu_us();
    json!({"cycles":cycles,"audit_bytes":bytes,"elapsed_ns":elapsed_ns,
        "process_user_us":cpu_after.0-cpu_before.0,"process_system_us":cpu_after.1-cpu_before.1})
}
fn allocation_window(
    mode: Mode,
    source: &Arc<CatalogSnapshot>,
    cache: &PreparedCatalogCache,
    queries: &[Query; 3],
    writer: &Mutex<Vec<u8>>,
    cycles: usize,
    deadline: Instant,
) -> Value {
    ensure_budget(deadline);
    let mut bytes = 0;
    let allocated = crate::dispatch::allocation_tests::count(|| {
        bytes = batch(mode, source, cache, queries, writer, cycles, deadline);
    });
    assert_eq!(allocated, (0, 0), "warmed grant cycle allocated");
    json!({"cycles":cycles,"audit_bytes":bytes,"rust_alloc_calls":allocated.0,
        "rust_alloc_requested_bytes":allocated.1})
}
fn prepared_pair(
    source: &Arc<CatalogSnapshot>,
) -> (PreparedCatalogCache, Arc<PreparedCatalog>, Value) {
    let cache = PreparedCatalogCache::default();
    let mut prepared = None;
    let before = process_cpu_us();
    let started = Instant::now();
    let allocated = crate::dispatch::allocation_tests::count(|| {
        let read = cache.prepare(Arc::clone(source)).unwrap();
        assert!(!read.waited);
        prepared = Some(read.catalog);
    });
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap();
    let after = process_cpu_us();
    let prepared = prepared.unwrap();
    assert!(
        prepared.scopes.is_some(),
        "retained shared authority must build an actual index"
    );
    assert!(std::ptr::eq(prepared.snapshot(), source.as_ref()));
    let reused = cache.prepare(Arc::clone(source)).unwrap();
    assert!(!reused.waited);
    assert!(Arc::ptr_eq(&prepared, &reused.catalog));
    (
        cache,
        prepared,
        json!({"preparations":1,"elapsed_ns":elapsed_ns,
        "process_user_us":after.0-before.0,"process_system_us":after.1-before.1,
        "rust_alloc_calls":allocated.0,"rust_alloc_requested_bytes":allocated.1,
        "scope":"cold prepare includes current-thread allocation-meter overhead; authority construction/validation and later index drop excluded"}),
    )
}
fn correctness_oracles(deadline: Instant) {
    let mut authority = super::tests::target_authority(10);
    let read = authority.grants.get_mut("g00000").unwrap();
    read.drives.insert("d00000".into(), Permission::Read);
    let write = authority.grants.get_mut("g00001").unwrap();
    write.drives.insert("d00000".into(), Permission::Write);
    write
        .claim_conditions
        .insert("/sub".into(), "c00000".into());
    write
        .claim_conditions
        .insert("/role".into(), "writer".into());
    authority.validate().unwrap();
    let source = Arc::new(authority);
    let (cache, _, _) = prepared_pair(&source);
    let writer = Mutex::new(Vec::with_capacity(4096));
    let mut current = query(0);
    current.identity.claims["role"] = json!("writer");
    current.expected_audit = expected_audit("p00000", "d00000", &["g00000", "g00001"]);
    for mode in [Mode::Full, Mode::Indexed] {
        ensure_budget(deadline);
        cycle(mode, &source, &cache, &current, &writer);
    }
    // Reuse the same immutable source/index while changing only current claims.
    for role in [json!("reader"), json!(7), Value::Null] {
        current.identity.claims["role"] = role;
        current.expected_permission = Some(Permission::Read);
        current.expected_audit = expected_audit("p00000", "d00000", &["g00000"]);
        for mode in [Mode::Full, Mode::Indexed] {
            cycle(mode, &source, &cache, &current, &writer);
        }
    }
    current.identity.claims["sub"] = json!("unmatched");
    current.expected_permission = None;
    current.expected_audit = expected_audit("p00000", "d00000", &[]);
    for mode in [Mode::Full, Mode::Indexed] {
        cycle(mode, &source, &cache, &current, &writer);
    }
    // Preserve the original intentional distinction: empty conditions deny
    // authorization but satisfy the audit's all() predicate. Not a hot fixture.
    let mut empty = source.as_ref().clone();
    empty.grants.retain(|id, _| id == "g00000");
    empty
        .grants
        .get_mut("g00000")
        .unwrap()
        .claim_conditions
        .clear();
    let source = Arc::new(empty);
    let (cache, _, _) = prepared_pair(&source);
    current.expected_audit = expected_audit("p00000", "d00000", &["g00000"]);
    for mode in [Mode::Full, Mode::Indexed] {
        cycle(mode, &source, &cache, &current, &writer);
    }
}

#[test]
#[ignore = "explicit bounded paired release authorization/audit CPU and allocator measurement"]
fn grant_index_paired_release_cpu_and_allocations() {
    assert!(
        !black_box(cfg!(debug_assertions)),
        "grant benchmark requires --release"
    );
    let started = Instant::now();
    let deadline = started + INTERNAL_BOUND;
    AUDIT_CANDIDATE_VISITS.with(|visits| visits.set(0));
    assert_eq!(crate::dispatch::allocation_tests::count(|| {}), (0, 0));
    correctness_oracles(deadline);
    let mut scales = Vec::new();
    for grants in [10, 100, 1_000, 10_000] {
        ensure_budget(deadline);
        let source = Arc::new(super::tests::target_authority(grants));
        source.validate().unwrap();
        assert_eq!(source.grants.len(), grants);
        assert_eq!(source.partitions.len(), grants / 2);
        let queries = [query(0), query(grants / 2), query(grants - 1)];
        let writer = Mutex::new(Vec::with_capacity(4096));
        let (pilot_cache, pilot_prepared, pilot_cold) = prepared_pair(&source);
        for current in &queries {
            assert_eq!(
                pilot_prepared
                    .candidates(&current.identity.partition_id, "policy")
                    .len(),
                2
            );
            cycle(Mode::Full, &source, &pilot_cache, current, &writer);
            AUDIT_CANDIDATE_VISITS.with(|visits| visits.set(0));
            cycle(Mode::Indexed, &source, &pilot_cache, current, &writer);
            assert_eq!(AUDIT_CANDIDATE_VISITS.with(std::cell::Cell::get), 2);
        }
        let pilot_full = native_window(
            Mode::Full,
            &source,
            &pilot_cache,
            &queries,
            &writer,
            PILOT_CYCLES,
            deadline,
        );
        let pilot_indexed = native_window(
            Mode::Indexed,
            &source,
            &pilot_cache,
            &queries,
            &writer,
            PILOT_CYCLES,
            deadline,
        );
        let slowest_ns = u128::from(
            pilot_full["elapsed_ns"]
                .as_u64()
                .unwrap()
                .max(pilot_indexed["elapsed_ns"].as_u64().unwrap())
                .max(1),
        );
        let estimate = (TARGET_WINDOW_NS * PILOT_CYCLES as u128 / slowest_ns)
            .clamp(MIN_CYCLES as u128, MAX_CYCLES as u128);
        let cycles = usize::try_from(estimate).unwrap() / 3 * 3;
        assert!((MIN_CYCLES..=MAX_CYCLES).contains(&cycles));
        let mut pairs = Vec::with_capacity(PAIRS);
        for pair in 0..PAIRS {
            ensure_budget(deadline);
            let (cache, prepared, cold) = prepared_pair(&source);
            for current in &queries {
                assert_eq!(
                    prepared
                        .candidates(&current.identity.partition_id, "policy")
                        .len(),
                    2
                );
                cycle(Mode::Full, &source, &cache, current, &writer);
                cycle(Mode::Indexed, &source, &cache, current, &writer);
            }
            let order = if pair.is_multiple_of(2) {
                [Mode::Full, Mode::Indexed]
            } else {
                [Mode::Indexed, Mode::Full]
            };
            let mut native = [Value::Null, Value::Null];
            let mut allocations = [Value::Null, Value::Null];
            for mode in order {
                let slot = usize::from(matches!(mode, Mode::Indexed));
                native[slot] =
                    native_window(mode, &source, &cache, &queries, &writer, cycles, deadline);
            }
            for mode in order {
                let slot = usize::from(matches!(mode, Mode::Indexed));
                allocations[slot] =
                    allocation_window(mode, &source, &cache, &queries, &writer, cycles, deadline);
            }
            assert_eq!(native[0]["cycles"], native[1]["cycles"]);
            assert_eq!(native[0]["audit_bytes"], native[1]["audit_bytes"]);
            assert_eq!(allocations[0]["cycles"], allocations[1]["cycles"]);
            assert_eq!(allocations[0]["audit_bytes"], allocations[1]["audit_bytes"]);
            pairs.push(
                json!({"pair":pair,"first":if pair.is_multiple_of(2) {"full"} else {"indexed"},
                "cold":cold,"native":{"full":native[0],"indexed":native[1]},
                "allocation_only":{"full":allocations[0],"indexed":allocations[1]}}),
            );
        }
        scales.push(json!({"grants":grants,"partitions":grants/2,"scoped_candidates":2,
            "query_positions":[0,grants/2,grants-1],"cycles_per_window":cycles,"pairs":pairs,
            "calibration":{"cycles_per_mode":PILOT_CYCLES,"full":pilot_full,"indexed":pilot_indexed,"cold":pilot_cold}}));
    }
    ensure_budget(deadline);
    eprintln!(
        "MOUNT_RS_GRANT_INDEX_BENCHMARK {}",
        json!({
            "schema":"mount-rs.grant-index-paired.v1","complete":true,
            "elapsed_ns":u64::try_from(started.elapsed().as_nanos()).unwrap(),
            "release":!cfg!(debug_assertions),"pairs_per_scale":PAIRS,
            "timed_operations":"policy matching, source/cache Arc selection, public or prepared authorization, writer mutex, buffered audit, exact permission/bytes checks, periodic budget checks",
            "cpu_scope":"getrusage RUSAGE_SELF deltas; whole process, synchronous single-test execution",
            "allocation_scope":"existing current-thread System allocation/reallocation calls and requested bytes; separate synchronous windows; no deallocation/live/RSS or foreign allocator coverage",
            "cfg_test_scope":"indexed audit retains two TLS candidate-visit observations/cycle; full reference has none, avoiding artificial full-map TLS cost; cold preparation includes test hook and allocation-meter overhead",
            "exclusions":["catalog load and authority construction/validation","JWT verification","dispatcher freshness/wait paths","stderr OS audit I/O","transport/storage/provider/SQLite C","cluster capacity or full-production qualification"],
            "oracles":"validated target geometry, two candidates, exact authority Arc/cache reuse, full permission equivalence, LF-terminated audit bytes/grant order, reused-index current claims and empty-condition distinction",
            "scales":scales
        })
    );
}
