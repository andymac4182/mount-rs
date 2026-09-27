import assert from "node:assert/strict"
import test from "node:test"
import {
  NATIVE_DIAGNOSTICS_SCHEMA, RUSTFS_API_MEASUREMENT, RUSTFS_LOCAL_MEASUREMENT,
  STORAGE_BYTE_SEMANTICS, STORAGE_CALL_SEMANTICS, STORAGE_INSTRUMENTED_OPERATION_NAMES,
  STORAGE_OPERATION_FAMILIES, STORAGE_OPERATION_NAMES, STORAGE_ROW_SEMANTICS,
  TIDB_DIAGNOSTIC_COVERAGE, OBJECT_STORE_LOCAL_NAMES, validateRawPhaseDiagnostics,
  FOUNDATIONDB_DIAGNOSTIC_COVERAGE, FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE,
  storageInstrumentedOperationNames,
} from "./diagnostics.mjs"

// Missing implementation must fail assertions; these controls open no addon or backend.
let projectOwnedLayoutPhaseMetrics = () => ({ status: "unavailable" })
try { ({ projectOwnedLayoutPhaseMetrics } = await import("./owned-layout-metrics.mjs")) }
catch (error) { if (error.code !== "ERR_MODULE_NOT_FOUND") throw error }
const zeros = (fields) => Object.fromEntries(fields.map((field) => [field, "0"]))
const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const claims = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]
function model({ foundationdb = false } = {}) {
  const row = (name) => ({ name, ...zeros(["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"]), latency_log2_us: Array(32).fill("0") })
  const entries = STORAGE_OPERATION_NAMES.map(row)
  Object.assign(entries.find((r) => r.name === "tidb.sql.inode_read"), { calls: "600", success: "600", returned_rows: "400", returned_row_observations: "600", elapsed_ns: "9007199254740993", latency_log2_us: ["600", ...Array(31).fill("0")] })
  const instance = { id: "7", opened_during_phase: false,
    ...zeros(["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]),
    raw_api: { schema: "mount-rs.object-store-api.v1", scope: "one_object_store_block_store_instance", saturated_start: false, saturated_end: false,
      ...zeros(["in_flight_start", "in_flight_end", "pending_claims_start", "pending_claims_end"]), claims: zeros(claims),
      entries: rawNames.map((name) => ({ name, ...zeros(["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end"]), exact_phase_max_ns: "unavailable", latency_log2_us: Array(32).fill("0") })) } }
  const phase = { name: "workload-4096bytes", quiescent: true, elapsed_ms: 1201, benchmark_measured_elapsed_ms: 1200, observer_snapshot_ms: 1,
    native: { schema_version: NATIVE_DIAGNOSTICS_SCHEMA, complete: true, issues: [], storage: { in_flight_start: "0", in_flight_end: "0", entries },
      measurement: { rustfs: "live_store_logical_calls_and_cache_hits; not_http_attempts", rustfs_api: structuredClone(RUSTFS_API_MEASUREMENT),
        storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS, storage_rows: STORAGE_ROW_SEMANTICS,
        storage_operations: [...STORAGE_OPERATION_NAMES], storage_families: structuredClone(STORAGE_OPERATION_FAMILIES),
        storage_instrumented_operations: foundationdb ? storageInstrumentedOperationNames(FOUNDATIONDB_DIAGNOSTIC_COVERAGE) : [...STORAGE_INSTRUMENTED_OPERATION_NAMES],
        tidb_coverage: structuredClone(TIDB_DIAGNOSTIC_COVERAGE), foundationdb_coverage: structuredClone(foundationdb ? FOUNDATIONDB_DIAGNOSTIC_COVERAGE : FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE),
        latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, i) => i === 0 ? { lower_inclusive_us: "0", upper_exclusive_us: "1" } : i === 31 ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true } : { lower_inclusive_us: String(2 ** (i - 1)), upper_exclusive_us: String(2 ** i) }) } },
      rustfs: { scope: "process_live_instances", complete: true, internal_successful_retries: "unavailable", missing_instance_ids: [], instance_ids_start: ["7"], instance_ids_end: ["7"], instances: [instance] } } }
  return phase
}
function local(phase) {
  phase.native.measurement.rustfs_local = structuredClone(RUSTFS_LOCAL_MEASUREMENT)
  const entries = OBJECT_STORE_LOCAL_NAMES.map((name) => ({ name, ...zeros(["calls", "success", "error", "cancelled", "elapsed_ns", "input_bytes", "output_bytes", "latency_max_ns_start", "latency_max_ns_end"]), exact_phase_max_ns: "unavailable", latency_log2_us: Array(32).fill("0") }))
  Object.assign(entries[0], { calls: "4", success: "4", elapsed_ns: "4000", input_bytes: "16384", output_bytes: "128", latency_max_ns_end: "1000", latency_log2_us: ["4", ...Array(31).fill("0")] })
  phase.native.rustfs.instances[0].local_work = { status: "observed", complete: true, schema: "mount-rs.object-store-local.v1", scope: "one_object_store_block_store_instance", saturated_start: false, saturated_end: false, in_flight_start: "0", in_flight_end: "0", entries }
}
const stringify = (value) => JSON.stringify(value)

test("modeled raw phase independently satisfies original validator", () => assert.deepEqual(validateRawPhaseDiagnostics(model(), "rustfs"), ["7"]))
test("retains exact SQL calls, returned-row observations and histograms", () => {
  const source = model(), before = structuredClone(source), value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.status, "observed")
  const row = value.storage.entries.find((r) => r.name === "tidb.sql.inode_read")
  assert.equal(row.calls, "600"); assert.equal(row.returned_rows, "400"); assert.equal(row.returned_row_observations, "600")
  assert.equal(row.elapsed_ns, "9007199254740993"); assert.equal(row.latency_log2_us[0], "600")
  assert.deepEqual(source, before)
  assert.equal(Object.isFrozen(value), true); assert.equal(Object.isFrozen(row.latency_log2_us), true)
})
test("retains legitimate zero backend GETs without interpreting missing local work as zero", () => {
  const value = projectOwnedLayoutPhaseMetrics(model())
  assert.equal(value.rustfs.instances[0].raw_api.entries[1].calls, "0")
  assert.equal(value.rustfs.instances[0].local_work.status, "unavailable")
  assert.equal(Object.hasOwn(value.rustfs.instances[0].local_work, "entries"), false)
  assert.equal(value.unavailable.physical_device_iops, true)
  assert.equal(value.unavailable.http_attempts_and_internal_retries, true)
})
test("preserves audited FoundationDB source coverage and precise returned payload counters", () => {
  const source = model({ foundationdb: true })
  const entry = source.native.storage.entries.find((row) => row.name === "foundationdb.read.get")
  Object.assign(entry, { calls: "1", success: "1", bytes: "9007199254740993", elapsed_ns: "9007199254740995", latency_log2_us: ["1", ...Array(31).fill("0")] })
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.status, "observed")
  assert.equal(value.storage.entries.length, 91)
  assert.deepEqual(value.storage.foundationdb_coverage, FOUNDATIONDB_DIAGNOSTIC_COVERAGE)
  assert.deepEqual(value.storage.instrumented_operations.slice(-7), FOUNDATIONDB_DIAGNOSTIC_COVERAGE.operations)
  const projected = value.storage.entries.find((row) => row.name === "foundationdb.read.get")
  assert.equal(projected.bytes, "9007199254740993"); assert.equal(projected.elapsed_ns, "9007199254740995")
  assert.equal(projected.returned_rows, "0"); assert.equal(projected.returned_row_observations, "0")
  assert.ok(value.storage.foundationdb_coverage.unavailable.includes("native_operation_settlement_after_future_drop"))
})
test("feature-off fixed FoundationDB rows retain explicit unavailable coverage", () => {
  const value = projectOwnedLayoutPhaseMetrics(model())
  assert.equal(value.status, "observed"); assert.equal(value.storage.entries.length, 91)
  assert.deepEqual(value.storage.foundationdb_coverage, FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE)
  assert.equal(value.storage.instrumented_operations.some((name) => name.startsWith("foundationdb.")), false)
  assert.match(value.storage.scope, /audited_instrumented_operations_separately_declared/u)
})
test("retains digest, encoding, copy and wait evidence with original limits", () => {
  const source = model(); local(source)
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.rustfs.instances[0].local_work.status, "observed")
  assert.equal(value.rustfs.instances[0].local_work.entries[0].input_bytes, "16384")
  assert.equal(value.rustfs.instances[0].local_work.entries[0].output_bytes, "128")
  assert.equal(value.rustfs.instances[0].local_work.entries[0].exact_phase_max_ns, "unavailable")
})
test("invalid optional local evidence stays unavailable while raw counters remain observed", () => {
  const source = model(); local(source); source.native.rustfs.instances[0].local_work.entries[0].output_bytes = "127"
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.status, "observed"); assert.equal(value.rustfs.instances[0].local_work.status, "unavailable")
})
test("captures process CPU work and observer cost, fixed memory endpoints, without allocation claims", () => {
  const source = model()
  source.process = { cpu_work: { user_us: "100", system_us: "20" }, cpu_observer: { user_us: "3", system_us: "2" }, resources_work: { voluntary_context_switches: "9", involuntary_context_switches: "1" }, memory_end_bytes: { rss: "1000", heapTotal: "200", heapUsed: "100", external: "90", arrayBuffers: "80", PRIVATE_ENDPOINT: "300" }, native_allocation_count: "100", js_allocation_count: "200" }
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.process.status, "observed"); assert.equal(value.process.cpu_observer.user_us, "3")
  assert.equal(value.process.memory_end_bytes.rss, "1000"); assert.equal(Object.hasOwn(value.process.memory_end_bytes, "PRIVATE_ENDPOINT"), false)
  assert.equal(value.process.native_allocation_count, "unavailable"); assert.equal(value.process.js_allocation_count, "unavailable")
})
test("retains forwarding box sites as a partial allocation measure and closed core event counters", () => {
  const source = model()
  source.native.storage.forwarding_boxes = { sites: "napi_dynamic_provider_forwarding_future", calls: "600", requested_object_bytes: "19200" }
  source.native.measurement.forwarding_boxes = "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations"
  source.native.measurement.profile = "existing_core_profile_counters"
  source.native.profile = { entries: [{ name: "provider.namespace_serialized_bytes", calls: "400", units: "1638400", elapsed_ns: "90" }] }
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.forwarding_boxes.status, "observed"); assert.equal(value.forwarding_boxes.calls, "600")
  assert.equal(value.forwarding_boxes.sites, "napi_dynamic_provider_forwarding_future")
  assert.equal(value.core_profile.status, "observed"); assert.equal(value.core_profile.entries[0].units, "1638400")
})
test("unknown forwarding site cannot become an allocation measurement", () => {
  const source = model()
  source.native.measurement.forwarding_boxes = "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations"
  source.native.storage.forwarding_boxes = { sites: "PRIVATE_SITE", calls: "1", requested_object_bytes: "10" }
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.forwarding_boxes.status, "unavailable"); assert.equal(stringify(value).includes("PRIVATE_SITE"), false)
})
test("unknown core profile labels do not copy private values", () => {
  const source = model(); source.native.measurement.profile = "existing_core_profile_counters"
  source.native.profile = { entries: [{ name: "PRIVATE_PATH", calls: "1", elapsed_ns: "0", units: "2" }] }
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.core_profile.status, "unavailable"); assert.equal(stringify(value).includes("PRIVATE_PATH"), false)
})
test("arbitrary diagnostic fields and private metadata are never serialized", () => {
  const source = model(); source.native.private_url = "PRIVATE_SECRET"; source.native.rustfs.instances[0].raw_api.error = "PRIVATE_SECRET"
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.status, "observed"); assert.equal(stringify(value).includes("PRIVATE_SECRET"), false)
  assert.equal(Object.hasOwn(value, "floor_qualified"), false)
})
const faults = [
  ["missing phase", () => null],
  ["wrong workload", (p) => { p.name = "PRIVATE_PATH"; return p }],
  ["incomplete phase", (p) => { p.native.complete = false; return p }],
  ["pending storage", (p) => { p.native.storage.in_flight_end = "1"; return p }],
  ["saturated raw", (p) => { p.native.rustfs.instances[0].raw_api.saturated_end = true; return p }],
  ["unknown label", (p) => { p.native.storage.entries[0].name = "PRIVATE_SQL"; return p }],
  ["noncanonical counter", (p) => { p.native.storage.entries[0].calls = "00"; return p }],
  ["u64 overflow", (p) => { p.native.storage.entries[0].calls = "18446744073709551616"; return p }],
  ["histogram inconsistency", (p) => { p.native.storage.entries[0].latency_log2_us[0] = "1"; return p }],
  ["sparse rows", (p) => { delete p.native.storage.entries[0]; return p }],
  ["sparse zero histogram", (p) => { p.native.storage.entries[0].latency_log2_us = Array(32); return p }],
  ["sparse instance identities", (p) => { p.native.rustfs.instance_ids_start = Array(1); return p }],
  ["missing FoundationDB coverage", (p) => { delete p.native.measurement.foundationdb_coverage; return p }],
  ["partial FoundationDB source audit", (p) => { p.native.measurement.foundationdb_coverage = structuredClone(FOUNDATIONDB_DIAGNOSTIC_COVERAGE); p.native.measurement.foundationdb_coverage.operations.pop(); return p }],
  ["private FoundationDB source metadata", (p) => { p.native.measurement.foundationdb_coverage.private_key = "PRIVATE_SECRET"; return p }],
  ["legacy78 row inventory", (p) => { p.native.storage.entries.length = 78; p.native.measurement.storage_operations.length = 78; delete p.native.measurement.foundationdb_coverage; return p }],
  ["legacy85 row inventory", (p) => { p.native.storage.entries.length = 85; p.native.measurement.storage_operations.length = 85; delete p.native.measurement.storage_families.blob_cache; return p }],
  ["cyclic input", (p) => { p.native.storage.circular = p; return p }],
  ["overcap evidence", (p) => { p.native.secret = "PRIVATE".repeat(1_300_000); return p }],
]
for (const [name, mutate] of faults) test(`${name} produces a closed unavailable receipt`, () => {
  const value = projectOwnedLayoutPhaseMetrics(mutate(model()))
  assert.equal(value.status, "unavailable"); assert.equal(value.reason, "PHASE_METRICS_UNAVAILABLE")
  assert.equal(stringify(value).includes("PRIVATE"), false)
})
test("getters cannot be invoked or copy opaque thrown errors", () => {
  const source = model(); let calls = 0
  Object.defineProperty(source.native.storage.entries[0], "calls", { get() { calls++; throw Error("PRIVATE_SECRET") }, enumerable: true })
  assert.equal(projectOwnedLayoutPhaseMetrics(source).status, "unavailable"); assert.equal(calls, 0)
})
test("invalid optional process counters do not silently become zero", () => {
  const source = model(); source.process = { cpu_work: { user_us: "-1" } }
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.status, "observed"); assert.equal(value.process.status, "unavailable")
})

// Independent fixed recorder contract; not imported from the projector.
const causalProfileNames = [
  "wire.json_encode_bytes",
  "wire.json_decode_bytes",
  "catalog.load",
  "catalog.queue_wait",
  "catalog.pool_wait",
  "catalog.backing_verify",
  "catalog.connect_configure",
  "catalog.query_document_bytes",
  "catalog.decode_validate_bytes",
  "catalog.close",
  "catalog.pager_hits",
  "catalog.pager_misses",
  "catalog.pager_writes",
  "catalog.pager_unavailable",
  "service.dispatch",
  "service.authorization",
  "service.handle_lock_wait",
  "service.audit",
  "filesystem.gate_wait",
  "filesystem.mutation_batch_attempted_requests",
  "filesystem.snapshot_nodes",
  "filesystem.metadata_refresh",
  "filesystem.changed_namespace_nodes",
  "filesystem.write_fallback",
  "filesystem.old_chunk_read_bytes",
  "provider.metadata.load",
  "provider.metadata.load_if_changed",
  "provider.blocks.get_bytes",
  "provider.blocks.put_bytes",
  "provider.blocks.flush",
  "provider.blocks.verify_authority",
  "provider.metadata.publish_cas_nodes",
  "provider.metadata.cas_conflict",
  "provider.namespace_returned_bytes",
  "provider.namespace_serialized_bytes",
  "provider.inode.snapshot_if_changed",
  "provider.inode.snapshot_returned_nodes",
  "provider.inode.snapshot_unchanged",
  "filesystem.inode_path_guard",
  "provider.inode.load",
  "provider.inode.load_if_changed",
  "provider.inode.publish_cas",
  "provider.inode.cas_conflict",
  "provider.compact_anchor_returned_bytes",
  "provider.compact_anchor_serialized_bytes",
  "provider.inode_returned_bytes",
  "provider.inode_serialized_bytes",
  "filesystem.block_put.initial",
  "filesystem.block_put.initial.success",
  "filesystem.block_put.initial.error",
  "filesystem.block_put.initial.cancelled",
  "filesystem.block_put.fallback",
  "filesystem.block_put.fallback.success",
  "filesystem.block_put.fallback.error",
  "filesystem.block_put.fallback.cancelled",
  "filesystem.block_put.retry_rewrite",
  "filesystem.block_put.retry_rewrite.success",
  "filesystem.block_put.retry_rewrite.error",
  "filesystem.block_put.retry_rewrite.cancelled",
  "filesystem.block_put.chunker_reprepare",
  "filesystem.block_put.chunker_reprepare.success",
  "filesystem.block_put.chunker_reprepare.error",
  "filesystem.block_put.chunker_reprepare.cancelled",
  "filesystem.gate_wait.read",
  "filesystem.gate_hold.read",
  "filesystem.gate_wait.read.cancelled",
  "filesystem.gate_wait.metadata",
  "filesystem.gate_hold.metadata",
  "filesystem.gate_wait.metadata.cancelled",
  "filesystem.gate_wait.write_prepare",
  "filesystem.gate_hold.write_prepare",
  "filesystem.gate_wait.write_prepare.cancelled",
  "filesystem.gate_wait.write_commit",
  "filesystem.gate_hold.write_commit",
  "filesystem.gate_wait.write_commit.cancelled",
  "filesystem.gate_wait.write_fallback",
  "filesystem.gate_hold.write_fallback",
  "filesystem.gate_wait.write_fallback.cancelled",
  "filesystem.gate_wait.whole_file_replay",
  "filesystem.gate_hold.whole_file_replay",
  "filesystem.gate_wait.whole_file_replay.cancelled",
  "filesystem.gate_wait.mutation_batch",
  "filesystem.gate_hold.mutation_batch",
  "filesystem.gate_wait.mutation_batch.cancelled",
  "filesystem.gate_wait.maintenance",
  "filesystem.gate_hold.maintenance",
  "filesystem.gate_wait.maintenance.cancelled",
  "filesystem.gate_phase.refresh",
  "filesystem.gate_phase.recovery",
  "filesystem.gate_phase.block_rewrite",
  "filesystem.gate_phase.publication",
  "filesystem.gate_phase.cas_backoff",
  "filesystem.mutation.enqueue_requests",
  "filesystem.mutation.dequeue_requests",
  "filesystem.mutation.queue_wait_requests",
  "filesystem.mutation.coalescing_yields",
  "filesystem.mutation.attempt_requests",
  "filesystem.mutation.attempt.success",
  "filesystem.mutation.attempt.conflict",
  "filesystem.mutation.attempt.no_publication",
  "filesystem.mutation.attempt.error",
  "filesystem.mutation.attempt.cancelled",
  "filesystem.mutation.request.committed",
  "filesystem.mutation.request.conflict",
  "filesystem.mutation.request.cancelled",
  "filesystem.mutation.request.receiver_closed",
  "filesystem.mutation.request.error",
  "filesystem.mutation.request.reply_sent",
  "filesystem.mutation.create_guard.evaluated",
  "filesystem.mutation.create_guard.passed",
  "filesystem.mutation.create_guard.conflict",
  "filesystem.mutation.create_guard.revision_mismatch",
  "filesystem.mutation.create_guard.allocation_mismatch",
  "filesystem.mutation.create_guard.path_present",
  "compact.namespace.materialize_nodes",
  "filesystem.mutation.candidate_clone_nodes",
  "compact.structure.delta_capture_nodes",
  "compact.structure.expected_guard_nodes",
  "sqlite.compact.authority_query",
  "sqlite.compact.authority_path",
  "sqlite.compact.anchor_query_bytes",
  "sqlite.compact.anchor_decode_bytes",
  "sqlite.compact.guard_selected_rows",
  "sqlite.compact.guard_full_rows",
  "sqlite.compact.guard_selected_decode_bytes",
  "sqlite.compact.guard_full_decode_bytes",
  "sqlite.compact.read_lock_wait",
  "sqlite.compact.read_begin",
  "filesystem.refresh.replace_probe",
  "filesystem.refresh.create_capture",
  "filesystem.refresh.batch_capture",
  "filesystem.refresh.path_structure",
  "filesystem.refresh.read_before",
  "filesystem.refresh.read_after",
  "blob_cache.ram.hit_bytes",
  "blob_cache.disk.hit_bytes",
]

test("causal core projection preserves fixed prefix, new zero rows and exact decimal strings", () => {
  assert.equal(causalProfileNames.length, 136)
  assert.equal(causalProfileNames[46], "provider.inode_serialized_bytes")
  assert.equal(causalProfileNames[47], "filesystem.block_put.initial")
  assert.deepEqual(causalProfileNames.slice(108, 114), [
    "filesystem.mutation.create_guard.evaluated",
    "filesystem.mutation.create_guard.passed",
    "filesystem.mutation.create_guard.conflict",
    "filesystem.mutation.create_guard.revision_mismatch",
    "filesystem.mutation.create_guard.allocation_mismatch",
    "filesystem.mutation.create_guard.path_present",
  ])
  assert.deepEqual(causalProfileNames.slice(114, 118), [
    "compact.namespace.materialize_nodes",
    "filesystem.mutation.candidate_clone_nodes",
    "compact.structure.delta_capture_nodes",
    "compact.structure.expected_guard_nodes",
  ])
  assert.deepEqual(causalProfileNames.slice(118, 134), [
    "sqlite.compact.authority_query",
    "sqlite.compact.authority_path",
    "sqlite.compact.anchor_query_bytes",
    "sqlite.compact.anchor_decode_bytes",
    "sqlite.compact.guard_selected_rows",
    "sqlite.compact.guard_full_rows",
    "sqlite.compact.guard_selected_decode_bytes",
    "sqlite.compact.guard_full_decode_bytes",
    "sqlite.compact.read_lock_wait",
    "sqlite.compact.read_begin",
    "filesystem.refresh.replace_probe",
    "filesystem.refresh.create_capture",
    "filesystem.refresh.batch_capture",
    "filesystem.refresh.path_structure",
    "filesystem.refresh.read_before",
    "filesystem.refresh.read_after",
  ])
  assert.deepEqual(causalProfileNames.slice(134), ["blob_cache.ram.hit_bytes", "blob_cache.disk.hit_bytes"])
  const source = model()
  source.native.measurement.profile = "existing_core_profile_counters"
  source.native.profile = { entries: causalProfileNames.map((name) => ({ name, calls: "0", elapsed_ns: "0", units: "0" })) }
  Object.assign(source.native.profile.entries[47], { calls: "9007199254740993", elapsed_ns: "9007199254740995", units: "18446744073709551615" })
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.deepEqual(value.core_profile.entries, source.native.profile.entries)
  assert.equal(value.core_profile.status, "observed")
  assert.equal(value.storage.entries.length, 91)
  source.native.profile.entries = source.native.profile.entries.slice(0, 134)
  const previousCacheSnapshot = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(previousCacheSnapshot.status, "observed")
  assert.equal(previousCacheSnapshot.entries.length, 134)
  assert.equal(previousCacheSnapshot.entries.some((row) => row.name.startsWith("blob_cache.")), false)
  assert.match(previousCacheSnapshot.scope, /absent_events_unavailable/u)
  source.native.profile.entries = source.native.profile.entries.slice(0, 118)
  const previousCompactSnapshot = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(previousCompactSnapshot.status, "observed")
  assert.equal(previousCompactSnapshot.entries.length, 118)
  assert.equal(previousCompactSnapshot.entries.some((row) => row.name === causalProfileNames[118]), false)
  assert.match(previousCompactSnapshot.scope, /absent_events_unavailable/u)
  source.native.profile.entries = source.native.profile.entries.slice(0, 114)
  const previousSnapshot = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(previousSnapshot.status, "observed")
  assert.equal(previousSnapshot.entries.length, 114)
  assert.equal(previousSnapshot.entries.some((row) => row.name === causalProfileNames[114]), false)
  assert.match(previousSnapshot.scope, /absent_events_unavailable/u)
  source.native.profile.entries = source.native.profile.entries.slice(0, 108)
  const oldSnapshot = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(oldSnapshot.status, "observed")
  assert.equal(oldSnapshot.entries.length, 108)
  assert.equal(oldSnapshot.entries.some((row) => row.name === causalProfileNames[108]), false)
  source.native.profile.entries = source.native.profile.entries.slice(0, 47)
  const partial = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(partial.status, "observed")
  assert.equal(partial.entries.length, 47)
  assert.equal(partial.entries.some((row) => row.name === "filesystem.block_put.initial"), false)
  assert.match(partial.scope, /absent_events_unavailable/u)
})
for (const kind of ["duplicate", "unknown", "numeric_counter"]) test(`causal core ${kind} cannot become measured evidence`, () => {
  const source = model()
  source.native.measurement.profile = "existing_core_profile_counters"
  source.native.profile = { entries: [{ name: "filesystem.block_put.initial", calls: "0", elapsed_ns: "0", units: "0" }] }
  if (kind === "duplicate") source.native.profile.entries.push({ ...source.native.profile.entries[0] })
  else if (kind === "unknown") source.native.profile.entries[0].name = "PRIVATE_CAUSAL_LABEL"
  else source.native.profile.entries[0].calls = 9007199254740992
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.core_profile.status, "unavailable")
  assert.equal(JSON.stringify(value).includes("PRIVATE_CAUSAL_LABEL"), false)
})

test("partial projector retains six existing optional compact labels independently of exact bank coverage", () => {
  const source = model()
  source.native.measurement.profile = "existing_core_profile_counters"
  const names = ["compact.capture.guard_clones", "compact.capture.file_layout_clones", "compact.capture.extent_clones",
    "compact.create.namespace_cow_nodes", "compact.create.namespace_cow_layouts", "compact.create.namespace_cow_extents"]
  source.native.profile = { entries: names.map((name) => ({ name, calls: "1", elapsed_ns: "2", units: "9007199254740993" })) }
  const value = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(value.status, "observed")
  assert.deepEqual(value.entries, source.native.profile.entries)
  assert.match(value.scope, /absent_events_unavailable/u)
  source.native.profile.entries = [
    ...causalProfileNames.map((name) => ({ name, calls: "0", elapsed_ns: "0", units: "0" })),
    ...source.native.profile.entries,
  ]
  const complete = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(complete.status, "observed")
  assert.equal(complete.entries.length, 142)
  assert.deepEqual(complete.entries, source.native.profile.entries)
})

test("cache hit-byte events retain zero-byte hits and exact u64 payload units", () => {
  const source = model()
  source.native.measurement.profile = "existing_core_profile_counters"
  source.native.profile = { entries: [
    { name: "blob_cache.ram.hit_bytes", calls: "1", elapsed_ns: "0", units: "0" },
    { name: "blob_cache.disk.hit_bytes", calls: "9007199254740993", elapsed_ns: "0", units: "18446744073709551615" },
  ] }
  const before = structuredClone(source)
  const value = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(value.status, "observed")
  assert.deepEqual(value.entries, source.native.profile.entries)
  assert.deepEqual(source, before)
})

test("compact and refresh projection retains measured units and inclusive timing as exact strings", () => {
  const source = model()
  source.native.measurement.profile = "existing_core_profile_counters"
  source.native.profile = { entries: causalProfileNames.slice(114).map((name, index) => ({
    name, calls: String(index + 1), elapsed_ns: String(9007199254740993n + BigInt(index)),
    units: String(18446744073709551615n - BigInt(index)),
  })) }
  const before = structuredClone(source)
  const value = projectOwnedLayoutPhaseMetrics(source).core_profile
  assert.equal(value.status, "observed")
  assert.deepEqual(value.entries, source.native.profile.entries)
  assert.deepEqual(source, before)
  assert.equal(Object.isFrozen(value.entries[0]), true)
  assert.match(value.scope, /units_depend_on_fixed_event/u)
})
