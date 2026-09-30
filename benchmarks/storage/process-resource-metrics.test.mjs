import assert from "node:assert/strict"
import test from "node:test"
import {
  finishPhase, takePhaseSnapshot, validateRawPhaseDiagnostics,
  NATIVE_DIAGNOSTICS_SCHEMA, RUSTFS_API_MEASUREMENT,
  STORAGE_OPERATION_NAMES, STORAGE_INSTRUMENTED_OPERATION_NAMES,
  STORAGE_OPERATION_FAMILIES, STORAGE_CALL_SEMANTICS, STORAGE_BYTE_SEMANTICS,
  STORAGE_ROW_SEMANTICS, TIDB_DIAGNOSTIC_COVERAGE, FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE,
} from "./diagnostics.mjs"
import { projectOwnedLayoutPhaseMetrics } from "./owned-layout-metrics.mjs"

// These controls inject process endpoints and open no addon, backend or device.
const MEMORY_FIELDS = ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"]
const RESOURCE_ROWS = {
  minor_page_faults: "minorPageFault",
  major_page_faults: "majorPageFault",
  filesystem_input_operations: "fsRead",
  filesystem_output_operations: "fsWrite",
}
const MEASUREMENT = {
  schema: "mount-rs.process-resources.v1",
  filesystem_operations: "process_getrusage_block_accounting; not_syscalls_or_device_iops",
  page_faults: "process_getrusage_minor_and_major_faults",
  memory: "endpoint_gauges; signed_delta_not_allocation_churn",
  peak_rss: "process_lifetime_high_water_mark; not_phase_peak",
}
const PEAK_SCOPE = "process_lifetime_high_water_mark; not_phase_peak"
const zeros = (fields) => Object.fromEntries(fields.map((field) => [field, "0"]))
const memory = (rss) => ({ rss, heapTotal: 600, heapUsed: 300, external: 200, arrayBuffers: 100 })
const resources = (minorPageFault, majorPageFault, fsRead, fsWrite, maxRSS) => ({
  minorPageFault, majorPageFault, fsRead, fsWrite, maxRSS,
  voluntaryContextSwitches: minorPageFault, involuntaryContextSwitches: majorPageFault,
})
function endpoints() {
  return {
    resources: [resources(11, 20, 30, 40, 100), resources(13, 21, 31, 42, 101),
      resources(19, 25, 37, 50, 103), resources(22, 27, 40, 54, 104)],
    memory: [memory(1000), memory(700)],
    cpu: [{ user: 10, system: 20 }, { user: 12, system: 21 },
      { user: 112, system: 71 }, { user: 115, system: 73 }],
    wall: [0, 4, 104, 108],
  }
}
function phaseFromEndpoints(frames = endpoints()) {
  let resourceIndex = 0, memoryIndex = 0, cpuIndex = 0, wallIndex = 0
  const samplers = {
    platform: frames.platform ?? "linux",
    resources: () => frames.resources[resourceIndex++], memory: () => frames.memory[memoryIndex++],
    cpu: () => frames.cpu[cpuIndex++], now: () => frames.wall[wallIndex++],
  }
  const before = takePhaseSnapshot(() => "{}", samplers)
  if (frames.platformAfter !== undefined) samplers.platform = frames.platformAfter
  const after = takePhaseSnapshot(() => "{}", samplers)
  return finishPhase("workload-4096bytes", before, after)
}
// The existing public projector requires complete raw RustFS evidence. Keep
// that original contract valid so a RED assertion identifies process retention.
function projectionFixture(process) {
  const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
  const storageFields = ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"]
  const rawFields = ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end"]
  const instance = { id: "7", opened_during_phase: false,
    ...zeros(["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]),
    raw_api: { schema: "mount-rs.object-store-api.v1", scope: "one_object_store_block_store_instance", saturated_start: false, saturated_end: false,
      ...zeros(["in_flight_start", "in_flight_end", "pending_claims_start", "pending_claims_end"]),
      claims: zeros(["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]),
      entries: rawNames.map((name) => ({ name, ...zeros(rawFields), exact_phase_max_ns: "unavailable", latency_log2_us: Array(32).fill("0") })) } }
  return { name: "workload-4096bytes", quiescent: true, elapsed_ms: 100, benchmark_measured_elapsed_ms: 100, observer_snapshot_ms: 8, process,
    native: { schema_version: NATIVE_DIAGNOSTICS_SCHEMA, complete: true, issues: [],
      storage: { in_flight_start: "0", in_flight_end: "0", entries: STORAGE_OPERATION_NAMES.map((name) => ({ name, ...zeros(storageFields), latency_log2_us: Array(32).fill("0") })) },
      measurement: { rustfs: "live_store_logical_calls_and_cache_hits; not_http_attempts", rustfs_api: structuredClone(RUSTFS_API_MEASUREMENT),
        storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS, storage_rows: STORAGE_ROW_SEMANTICS,
        storage_operations: [...STORAGE_OPERATION_NAMES], storage_instrumented_operations: [...STORAGE_INSTRUMENTED_OPERATION_NAMES], storage_families: structuredClone(STORAGE_OPERATION_FAMILIES),
        tidb_coverage: structuredClone(TIDB_DIAGNOSTIC_COVERAGE), foundationdb_coverage: structuredClone(FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE),
        latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, i) => i === 0 ? { lower_inclusive_us: "0", upper_exclusive_us: "1" } : i === 31 ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true } : { lower_inclusive_us: String(2 ** (i - 1)), upper_exclusive_us: String(2 ** i) }) } },
      rustfs: { scope: "process_live_instances", complete: true, internal_successful_retries: "unavailable", missing_instance_ids: [], instance_ids_start: ["7"], instance_ids_end: ["7"], instances: [instance] } } }
}
function measuredProcess() {
  return {
    cpu_work: { user_us: "100", system_us: "50" }, cpu_observer: { user_us: "5", system_us: "3" },
    resources_work: { voluntary_context_switches: "6", involuntary_context_switches: "4",
      minor_page_faults: "6", major_page_faults: "4", filesystem_input_operations: "6", filesystem_output_operations: "8" },
    resources_observer: { minor_page_faults: "5", major_page_faults: "3", filesystem_input_operations: "4", filesystem_output_operations: "6" },
    memory_start_bytes: { rss: "1000", heapTotal: "600", heapUsed: "300", external: "200", arrayBuffers: "100" },
    memory_end_bytes: { rss: "700", heapTotal: "600", heapUsed: "300", external: "200", arrayBuffers: "100" },
    memory_delta_bytes: { rss: "-300", heapTotal: "0", heapUsed: "0", external: "0", arrayBuffers: "0" },
    lifetime_peak_rss_bytes: { start: "103424", end: "105472", scope: PEAK_SCOPE },
    resource_measurement: structuredClone(MEASUREMENT),
    native_allocation_count: "unavailable", js_allocation_count: "unavailable",
  }
}
function assertAdditionsUnavailable(process) {
  for (const field of Object.keys(RESOURCE_ROWS)) {
    assert.equal(process.resources_work[field], "unavailable", `work ${field}`)
    assert.equal(process.resources_observer[field], "unavailable", `observer ${field}`)
  }
  for (const field of MEMORY_FIELDS) {
    assert.equal(process.memory_start_bytes[field], "unavailable", `start ${field}`)
    assert.equal(process.memory_delta_bytes[field], "unavailable", `delta ${field}`)
  }
  assert.equal(process.lifetime_peak_rss_bytes.start, "unavailable")
  assert.equal(process.lifetime_peak_rss_bytes.end, "unavailable")
  assert.equal(process.resource_measurement, "unavailable")
}

test("process projection fixture independently satisfies the existing raw validator", () => {
  assert.deepEqual(validateRawPhaseDiagnostics(projectionFixture(measuredProcess()), "rustfs"), ["7"])
})
test("process rows distinguish workload counters from both boundary observers", () => {
  const phase = phaseFromEndpoints()
  assert.equal(phase.native.complete, false, "process evidence does not require fabricated native evidence")
  assert.equal(phase.elapsed_ms, 100)
  assert.equal(phase.observer_snapshot_ms, 8)
  assert.deepEqual(phase.process.cpu_work, { user_us: "100", system_us: "50" })
  assert.deepEqual(phase.process.cpu_observer, { user_us: "5", system_us: "3" })
  assert.deepEqual(phase.process.resources_work, measuredProcess().resources_work)
  assert.deepEqual(phase.process.resources_observer, measuredProcess().resources_observer)
  assert.deepEqual(phase.process.resource_measurement, MEASUREMENT)
})
test("process memory retains both endpoint gauges and signed decreases without allocation claims", () => {
  const process = phaseFromEndpoints().process
  assert.deepEqual(process.memory_start_bytes, measuredProcess().memory_start_bytes)
  assert.deepEqual(process.memory_end_bytes, measuredProcess().memory_end_bytes)
  assert.deepEqual(process.memory_delta_bytes, measuredProcess().memory_delta_bytes)
  assert.deepEqual(process.lifetime_peak_rss_bytes, measuredProcess().lifetime_peak_rss_bytes)
  assert.equal(process.native_allocation_count, "unavailable")
  assert.equal(process.js_allocation_count, "unavailable")
})
test("zero fault and block accounting deltas are observed rather than unavailable", () => {
  const frames = endpoints()
  frames.resources = Array.from({ length: 4 }, () => resources(0, 0, 0, 0, 0))
  frames.memory = [memory(0), memory(0)]
  const process = phaseFromEndpoints(frames).process
  for (const field of Object.keys(RESOURCE_ROWS)) {
    assert.equal(process.resources_work[field], "0")
    assert.equal(process.resources_observer[field], "0")
  }
  assert.equal(process.memory_start_bytes.rss, "0")
  assert.equal(process.memory_delta_bytes.rss, "0")
  assert.equal(process.lifetime_peak_rss_bytes.end, "0")
})
for (const [label, mutate] of [
  ["missing", (frames) => { delete frames.resources[2].fsRead }],
  ["negative", (frames) => { frames.resources[2].fsRead = -1 }],
  ["fractional", (frames) => { frames.resources[2].fsRead = 37.5 }],
  ["unsafe integer", (frames) => { frames.resources[2].fsRead = Number.MAX_SAFE_INTEGER + 1 }],
  ["reset", (frames) => { frames.resources[2].fsRead = 30 }],
]) test(`an ${label} process counter remains unavailable only in its affected row`, () => {
  const frames = endpoints(); mutate(frames)
  const process = phaseFromEndpoints(frames).process
  assert.equal(process.resources_work.filesystem_input_operations, "unavailable")
  assert.equal(process.resources_work.filesystem_output_operations, "8")
  assert.equal(process.resources_work.minor_page_faults, "6")
})
test("an invalid observer endpoint does not contaminate a valid workload interval", () => {
  const frames = endpoints(); delete frames.resources[0].majorPageFault
  const process = phaseFromEndpoints(frames).process
  assert.equal(process.resources_work.major_page_faults, "4")
  assert.equal(process.resources_observer.major_page_faults, "unavailable")
  assert.equal(process.resources_observer.minor_page_faults, "5")
})
test("missing or invalid memory endpoints remain unavailable individually", () => {
  const frames = endpoints()
  delete frames.memory[0].external
  frames.memory[1].heapUsed = Number.MAX_SAFE_INTEGER + 1
  frames.memory[0].arrayBuffers = -1
  const process = phaseFromEndpoints(frames).process
  assert.equal(process.memory_start_bytes.external, "unavailable")
  assert.equal(process.memory_end_bytes.external, "200")
  assert.equal(process.memory_delta_bytes.external, "unavailable")
  assert.equal(process.memory_end_bytes.heapUsed, "unavailable")
  assert.equal(process.memory_delta_bytes.heapUsed, "unavailable")
  assert.equal(process.memory_start_bytes.arrayBuffers, "unavailable")
  assert.equal(process.memory_delta_bytes.rss, "-300")
})
test("safe Node RSS KiB values convert to exact bytes above Number precision", () => {
  const frames = endpoints()
  frames.resources[1].maxRSS = Number.MAX_SAFE_INTEGER - 2
  frames.resources[2].maxRSS = Number.MAX_SAFE_INTEGER
  const process = phaseFromEndpoints(frames).process
  assert.equal(process.lifetime_peak_rss_bytes.start, "9223372036854772736")
  assert.equal(process.lifetime_peak_rss_bytes.end, "9223372036854774784")
  assert.equal(process.lifetime_peak_rss_bytes.scope, PEAK_SCOPE)
})
for (const value of [undefined, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, 100]) {
  test(`invalid or regressed lifetime RSS ${String(value)} is not a phase peak`, () => {
    const frames = endpoints(); frames.resources[2].maxRSS = value
    const process = phaseFromEndpoints(frames).process
    assert.equal(process.lifetime_peak_rss_bytes.end, "unavailable")
    assert.equal(process.lifetime_peak_rss_bytes.scope, PEAK_SCOPE)
    assert.equal(process.resources_work.minor_page_faults, "6")
  })
}
test("public projection preserves additive process observations and closes private fields", () => {
  const process = measuredProcess()
  for (const field of ["resources_work", "resources_observer", "memory_start_bytes", "memory_end_bytes", "memory_delta_bytes", "lifetime_peak_rss_bytes"]) process[field].private_endpoint = "PRIVATE_SECRET"
  process.private_endpoint = "PRIVATE_SECRET"
  const source = projectionFixture(process), before = structuredClone(source)
  const value = projectOwnedLayoutPhaseMetrics(source)
  assert.equal(value.status, "observed")
  assert.equal(value.process.status, "observed")
  assert.equal(value.process.resources_work.minor_page_faults, "6")
  assert.deepEqual(value.process.resources_observer, measuredProcess().resources_observer)
  assert.deepEqual(value.process.memory_start_bytes, measuredProcess().memory_start_bytes)
  assert.deepEqual(value.process.memory_delta_bytes, measuredProcess().memory_delta_bytes)
  assert.deepEqual(value.process.lifetime_peak_rss_bytes, measuredProcess().lifetime_peak_rss_bytes)
  assert.deepEqual(value.process.resource_measurement, MEASUREMENT)
  assert.equal(JSON.stringify(value).includes("PRIVATE_SECRET"), false)
  assert.equal(Object.isFrozen(value.process.memory_delta_bytes), true)
  assert.deepEqual(source, before)
})
test("historical process evidence retains old observations and marks each new field unavailable", () => {
  const process = measuredProcess()
  for (const field of ["resources_observer", "memory_start_bytes", "memory_delta_bytes", "lifetime_peak_rss_bytes", "resource_measurement"]) delete process[field]
  for (const field of Object.keys(RESOURCE_ROWS)) delete process.resources_work[field]
  const value = projectOwnedLayoutPhaseMetrics(projectionFixture(process))
  assert.equal(value.status, "observed")
  assert.equal(value.process.status, "observed")
  assert.deepEqual(value.process.cpu_work, process.cpu_work)
  assert.equal(value.process.resources_work.voluntary_context_switches, "6")
  assert.equal(value.process.memory_end_bytes.rss, "700")
  assertAdditionsUnavailable(value.process)
})
for (const [label, mutate] of [
  ["wrong units", (p) => { p.resource_measurement.filesystem_operations = "physical_iops" }],
  ["unknown private metadata", (p) => { p.resource_measurement.private_endpoint = "PRIVATE_SECRET" }],
  ["missing schema", (p) => { delete p.resource_measurement.schema }],
]) test(`mismatched ${label} invalidates new observations without erasing historical evidence`, () => {
  const process = measuredProcess(); mutate(process)
  const value = projectOwnedLayoutPhaseMetrics(projectionFixture(process))
  assert.equal(value.status, "observed")
  assert.equal(value.process.status, "observed")
  assert.equal(value.process.cpu_work.user_us, "100")
  assert.equal(value.process.memory_end_bytes.rss, "700")
  assertAdditionsUnavailable(value.process)
  assert.equal(JSON.stringify(value).includes("PRIVATE_SECRET"), false)
})
test("public projection checks canonical unsigned counters and signed memory deltas per row", () => {
  const process = measuredProcess()
  process.resources_work.minor_page_faults = "00"
  process.resources_observer.major_page_faults = "-1"
  process.memory_start_bytes.external = "18446744073709551616"
  process.memory_delta_bytes.external = "PRIVATE_SECRET"
  const value = projectOwnedLayoutPhaseMetrics(projectionFixture(process))
  assert.equal(value.status, "observed")
  assert.equal(value.process.resources_work.minor_page_faults, "unavailable")
  assert.equal(value.process.resources_work.major_page_faults, "4")
  assert.equal(value.process.resources_observer.major_page_faults, "unavailable")
  assert.equal(value.process.memory_start_bytes.external, "unavailable")
  assert.equal(value.process.memory_delta_bytes.external, "unavailable")
  assert.equal(value.process.memory_delta_bytes.rss, "-300")
  assert.equal(JSON.stringify(value).includes("PRIVATE_SECRET"), false)
})
test("Windows fault and block fields with different POSIX meanings remain unavailable", () => {
  const frames = endpoints(); frames.platform = "win32"
  const process = phaseFromEndpoints(frames).process
  for (const field of Object.keys(RESOURCE_ROWS)) {
    assert.equal(process.resources_work[field], "unavailable", `Windows work ${field}`)
    assert.equal(process.resources_observer[field], "unavailable", `Windows observer ${field}`)
  }
  assert.equal(process.memory_end_bytes.rss, "700")
  assert.equal(process.memory_delta_bytes.rss, "-300")
  assert.equal(process.lifetime_peak_rss_bytes.end, "105472")
})
test("mismatched endpoint platforms invalidate OS fault and accounting rows", () => {
  const frames = endpoints(); frames.platform = "linux"; frames.platformAfter = "darwin"
  const process = phaseFromEndpoints(frames).process
  for (const field of Object.keys(RESOURCE_ROWS)) {
    assert.equal(process.resources_work[field], "unavailable", `platform mismatch work ${field}`)
    assert.equal(process.resources_observer[field], "unavailable", `platform mismatch observer ${field}`)
  }
  assert.equal(process.memory_end_bytes.rss, "700")
  assert.equal(process.memory_delta_bytes.rss, "-300")
  assert.deepEqual(process.cpu_work, { user_us: "100", system_us: "50" })
})
