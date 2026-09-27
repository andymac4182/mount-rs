import assert from "node:assert/strict"
import test from "node:test"
import {
  NATIVE_DIAGNOSTICS_SCHEMA, RUSTFS_API_MEASUREMENT, RUSTFS_LOCAL_MEASUREMENT,
  STORAGE_BYTE_SEMANTICS, STORAGE_CALL_SEMANTICS, STORAGE_INSTRUMENTED_OPERATION_NAMES,
  STORAGE_OPERATION_FAMILIES, STORAGE_OPERATION_NAMES, STORAGE_ROW_SEMANTICS,
  TIDB_DIAGNOSTIC_COVERAGE, OBJECT_STORE_LOCAL_NAMES, validateRawPhaseDiagnostics,
} from "./diagnostics.mjs"

// Missing implementation must fail assertions; these controls open no addon or backend.
let projectOwnedLayoutPhaseMetrics = () => ({ status: "unavailable" })
try { ({ projectOwnedLayoutPhaseMetrics } = await import("./owned-layout-metrics.mjs")) }
catch (error) { if (error.code !== "ERR_MODULE_NOT_FOUND") throw error }
const zeros = (fields) => Object.fromEntries(fields.map((field) => [field, "0"]))
const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const claims = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]
function model() {
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
        storage_operations: [...STORAGE_OPERATION_NAMES], storage_families: structuredClone(STORAGE_OPERATION_FAMILIES), storage_instrumented_operations: [...STORAGE_INSTRUMENTED_OPERATION_NAMES], tidb_coverage: structuredClone(TIDB_DIAGNOSTIC_COVERAGE),
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
