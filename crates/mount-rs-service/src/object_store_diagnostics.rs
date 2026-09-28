//! Application-owned bounded exports of the opt-in object-store process bank.
//! One captured fixed value is split into seven complete indexed frames. Handles
//! and samples retain no provider, service, endpoint, key or cached payload.
//! Encoding/decoding allocations and I/O are outside warmed bank update claims.
//! Cumulative observations are not transactional cuts or shutdown acknowledgments.
//! Client build durations cover inner HTTP connector connect wall time, not
//! whole-client/store construction, HTTP request duration, CPU time or physical I/O.

use mount_rs_core::diagnostics::object_store::{
    BundleSnapshot, CacheSnapshot, ClientBuildSnapshot, ClientSnapshot, HttpSnapshot, Snapshot,
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{self, Write},
};

pub const RECORD_LIMIT: usize = 16 * 1024;
pub const FRAME_COUNT: usize = 7;
pub const PREFIX: &[u8] = b"object_store_diagnostics ";
pub const SCHEMA: &str = "mount-rs.object-store-diagnostics.v2";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureContext {
    Periodic,
    Shutdown,
    /// Any owned fixture process boundary, including controller captures.
    /// This does not attribute the sample to a server or worker role.
    WorkerBoundary,
}

/// Caller-owned exact sample identity. Sequence zero and generation zero are
/// valid. Callers bind identity uniqueness and workload phases; clocks run there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub pid: u32,
    pub sequence: u64,
    pub observed_unix_ms: u64,
    pub context: CaptureContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    InvalidCapture,
    InvalidRecord,
    IncompleteSample,
}
impl fmt::Display for CodecError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::InvalidCapture => "invalid object-store capture identity",
            Self::InvalidRecord => "invalid object-store diagnostic record",
            Self::IncompleteSample => "incomplete or conflicting object-store diagnostic sample",
        })
    }
}
impl std::error::Error for CodecError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    capture: Capture,
    snapshot: Snapshot,
}

/// A disabled caller performs no validation, capture, clock, record or zero
/// export. An unavailable observer also produces no sample. Enabled capture
/// invokes the actual snapshot closure once and stores the copied fixed value.
pub fn capture(
    enabled: bool,
    identity: Capture,
    snapshot: impl FnOnce() -> Option<Snapshot>,
) -> Result<Option<Sample>, CodecError> {
    if !enabled {
        return Ok(None);
    }
    if identity.pid == 0 {
        return Err(CodecError::InvalidCapture);
    }
    Ok(snapshot().map(|snapshot| Sample {
        capture: identity,
        snapshot,
    }))
}

macro_rules! fixed_label {
    ($name:ident, $label:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        enum $name {
            #[serde(rename = $label)]
            Expected,
        }
    };
}
fixed_label!(Schema, "mount-rs.object-store-diagnostics.v2");
fixed_label!(Scope, "process_cumulative");
fixed_label!(CacheScope, "all_generic_object_store_block_adapters");
fixed_label!(HttpScope, "observed_rustfs_http_services");
fixed_label!(ClientBuildScope, "inner_http_connector_connect_wall_time");
fixed_label!(DurationScope, "inclusive_overlapping_wall_time");
fixed_label!(
    AllocationScope,
    "known_wrapper_construction_sites_not_allocator_calls"
);
fixed_label!(
    OfferedScope,
    "known_offered_http_body_bytes_per_polled_attempt"
);
fixed_label!(
    BodyScope,
    "http_body_data_bytes_before_adapter_validation_and_retries"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Coverage {
    cache_scope: CacheScope,
    http_scope: HttpScope,
    client_build_scope: ClientBuildScope,
    duration_semantics: DurationScope,
    allocation_counts: AllocationScope,
    offered_bytes: OfferedScope,
    body_bytes: BodyScope,
    capture_atomic: bool,
    application_drain_proven: bool,
    physical_iops_available: bool,
}
const COVERAGE: Coverage = Coverage {
    cache_scope: CacheScope::Expected,
    http_scope: HttpScope::Expected,
    client_build_scope: ClientBuildScope::Expected,
    duration_semantics: DurationScope::Expected,
    allocation_counts: AllocationScope::Expected,
    offered_bytes: OfferedScope::Expected,
    body_bytes: BodyScope::Expected,
    capture_atomic: false,
    application_drain_proven: false,
    physical_iops_available: false,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Quality {
    saturated: bool,
    concurrent_activity: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Role {
    PrimaryDataMixed,
    PrimaryProbeMixed,
    QualificationData,
    QualificationProbe,
    StandaloneData,
    StandaloneProbe,
}
const ROLES: [Role; 6] = [
    Role::PrimaryDataMixed,
    Role::PrimaryProbeMixed,
    Role::QualificationData,
    Role::QualificationProbe,
    Role::StandaloneData,
    Role::StandaloneProbe,
];
impl Role {
    fn index(self) -> usize {
        match self {
            Self::PrimaryDataMixed => 0,
            Self::PrimaryProbeMixed => 1,
            Self::QualificationData => 2,
            Self::QualificationProbe => 3,
            Self::StandaloneData => 4,
            Self::StandaloneProbe => 5,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Method {
    Get,
    Head,
    Put,
    Delete,
    Post,
    Other,
}
const METHODS: [Method; 6] = [
    Method::Get,
    Method::Head,
    Method::Put,
    Method::Delete,
    Method::Post,
    Method::Other,
];

#[derive(Serialize)]
struct Aggregate<'a> {
    bundles: &'a BundleSnapshot,
    cache: &'a CacheSnapshot,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Frame<'a> {
    HttpRole {
        schema: Schema,
        capture: Capture,
        index: usize,
        count: usize,
        scope: Scope,
        coverage: Coverage,
        quality: Quality,
        role: Role,
        methods: [Method; 6],
        snapshot: &'a ClientSnapshot,
    },
    BundleCache {
        schema: Schema,
        capture: Capture,
        index: usize,
        count: usize,
        scope: Scope,
        coverage: Coverage,
        quality: Quality,
        snapshot: Aggregate<'a>,
    },
}

fn write_frame(writer: &mut impl Write, frame: &impl Serialize) -> io::Result<()> {
    // Existing helper stages one bounded record before any sink write. Its cap
    // includes prefix and LF. A sink failure can leave a partial set; decode
    // accepts only a complete set. Callers preserve their own service outcome.
    crate::startup::write_bounded::<RECORD_LIMIT>(writer, PREFIX, frame)
}

impl Sample {
    pub fn capture(&self) -> &Capture {
        &self.capture
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        let quality = Quality {
            saturated: self.snapshot.saturated,
            concurrent_activity: self.snapshot.concurrent_activity,
        };
        for (index, role) in ROLES.into_iter().enumerate() {
            write_frame(
                writer,
                &Frame::HttpRole {
                    schema: Schema::Expected,
                    capture: self.capture,
                    index,
                    count: FRAME_COUNT,
                    scope: Scope::Expected,
                    coverage: COVERAGE,
                    quality,
                    role,
                    methods: METHODS,
                    snapshot: &self.snapshot.clients[index],
                },
            )?;
        }
        write_frame(
            writer,
            &Frame::BundleCache {
                schema: Schema::Expected,
                capture: self.capture,
                index: 6,
                count: FRAME_COUNT,
                scope: Scope::Expected,
                coverage: COVERAGE,
                quality,
                snapshot: Aggregate {
                    bundles: &self.snapshot.bundles,
                    cache: &self.snapshot.cache,
                },
            },
        )
    }
}

macro_rules! incoming_row {
    ($name:ident, $snapshot:ident, $($field:ident : $ty:ty),+ $(,)?) => {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct $name { $($field: $ty,)+ }
        impl From<$name> for $snapshot {
            fn from(row: $name) -> Self { Self { $($field: row.$field,)+ } }
        }
    };
}
incoming_row!(IncomingHttp, HttpSnapshot,
    attempts_started:u64, attempts_inflight:u64, header_responses:u64,
    transport_errors:u64, cancelled_before_headers:u64, offered_bytes:u64,
    offered_known:u64, offered_unknown:u64, known_extra_future_boxes:u64,
    known_extra_response_body_boxes:u64, bodies_started:u64, bodies_inflight:u64,
    body_eof:u64, body_errors:u64, body_dropped:u64, body_bytes:u64, body_chunks:u64,
    dispatch_elapsed_ns:u64, dispatch_max_ns:u64, body_elapsed_ns:u64,
    body_max_ns:u64, status:[u64; 6]);
incoming_row!(IncomingBundle, BundleSnapshot,
    builds_started:u64, builds_inflight:u64, committed:u64, failed:u64,
    abandoned:u64, live:u64, released:u64, elapsed_ns:u64, max_ns:u64);
incoming_row!(IncomingCache, CacheSnapshot,
    created:u64, released:u64, live:u64, resident_entries:u64,
    payload_bytes:u64, unknown_live:u64);
incoming_row!(IncomingClientBuild, ClientBuildSnapshot,
    started:u64, inflight:u64, succeeded:u64, failed:u64, abandoned:u64,
    elapsed_ns:u64, max_ns:u64);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingClient {
    constructed: u64,
    released: u64,
    live: u64,
    build: IncomingClientBuild,
    http: [IncomingHttp; 6],
}
impl From<IncomingClient> for ClientSnapshot {
    fn from(row: IncomingClient) -> Self {
        Self {
            constructed: row.constructed,
            released: row.released,
            live: row.live,
            build: row.build.into(),
            http: row.http.map(Into::into),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingAggregate {
    bundles: IncomingBundle,
    cache: IncomingCache,
}

// Struct variants reject missing, duplicate and unknown fields, including all
// nested counters. u64 Deserialize rejects strings, floats and negative values.
// This synchronous decoder keeps the fixed snapshot inline; boxing the larger
// variant would add an allocation for every HTTP frame.
#[expect(clippy::large_enum_variant)]
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum IncomingFrame {
    HttpRole {
        schema: Schema,
        capture: Capture,
        index: usize,
        count: usize,
        scope: Scope,
        coverage: Coverage,
        quality: Quality,
        role: Role,
        methods: [Method; 6],
        snapshot: IncomingClient,
    },
    BundleCache {
        schema: Schema,
        capture: Capture,
        index: usize,
        count: usize,
        scope: Scope,
        coverage: Coverage,
        quality: Quality,
        snapshot: IncomingAggregate,
    },
}

/// Decode one complete sample, in any indexed order. There is no partial-sample
/// success or cross-capture merge. Caller validates the returned exact Capture
/// against its own PID/generation/phase boundary and identity uniqueness.
pub fn decode(bytes: &[u8]) -> Result<Sample, CodecError> {
    if bytes.len() > FRAME_COUNT * RECORD_LIMIT || !bytes.ends_with(b"\n") {
        return Err(CodecError::InvalidRecord);
    }
    let mut seen = [false; FRAME_COUNT];
    let mut captured = None;
    let mut quality = None;
    let mut snapshot = Snapshot::default();
    let mut records = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.len() > RECORD_LIMIT {
            return Err(CodecError::InvalidRecord);
        }
        let json = line.strip_prefix(PREFIX).ok_or(CodecError::InvalidRecord)?;
        let frame: IncomingFrame =
            serde_json::from_slice(json).map_err(|_| CodecError::InvalidRecord)?;
        let (identity, index, count, coverage, flags, role_data, aggregate) = match frame {
            IncomingFrame::HttpRole {
                schema: Schema::Expected,
                capture,
                index,
                count,
                scope: Scope::Expected,
                coverage,
                quality,
                role,
                methods,
                snapshot,
            } => {
                if index >= 6 || role.index() != index || methods != METHODS {
                    return Err(CodecError::InvalidRecord);
                }
                (
                    capture,
                    index,
                    count,
                    coverage,
                    quality,
                    Some(snapshot),
                    None,
                )
            }
            IncomingFrame::BundleCache {
                schema: Schema::Expected,
                capture,
                index,
                count,
                scope: Scope::Expected,
                coverage,
                quality,
                snapshot,
            } => {
                if index != 6 {
                    return Err(CodecError::InvalidRecord);
                }
                (
                    capture,
                    index,
                    count,
                    coverage,
                    quality,
                    None,
                    Some(snapshot),
                )
            }
        };
        if identity.pid == 0 {
            return Err(CodecError::InvalidCapture);
        }
        if count != FRAME_COUNT
            || coverage != COVERAGE
            || seen[index]
            || captured.is_some_and(|previous| previous != identity)
            || quality.is_some_and(|previous| previous != flags)
        {
            return Err(CodecError::IncompleteSample);
        }
        seen[index] = true;
        captured = Some(identity);
        quality = Some(flags);
        records += 1;
        if let Some(row) = role_data {
            snapshot.clients[index] = row.into();
        }
        if let Some(row) = aggregate {
            snapshot.bundles = row.bundles.into();
            snapshot.cache = row.cache.into();
        }
    }
    if records != FRAME_COUNT || seen.iter().any(|present| !present) {
        return Err(CodecError::IncompleteSample);
    }
    let flags = quality.ok_or(CodecError::IncompleteSample)?;
    snapshot.saturated = flags.saturated;
    snapshot.concurrent_activity = flags.concurrent_activity;
    Ok(Sample {
        capture: captured.ok_or(CodecError::IncompleteSample)?,
        snapshot,
    })
}

#[cfg(test)]
#[path = "object_store_diagnostics_tests.rs"]
mod tests;
