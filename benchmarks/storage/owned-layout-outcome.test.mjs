import assert from "node:assert/strict"
import test from "node:test"
import { summarizeInterval } from "./backing-observer.mjs"
import {
  NATIVE_DIAGNOSTICS_SCHEMA, RUSTFS_API_MEASUREMENT, STORAGE_BYTE_SEMANTICS,
  STORAGE_CALL_SEMANTICS, STORAGE_INSTRUMENTED_OPERATION_NAMES, STORAGE_OPERATION_FAMILIES,
  STORAGE_OPERATION_NAMES, STORAGE_ROW_SEMANTICS, TIDB_DIAGNOSTIC_COVERAGE,
  validateRawPhaseDiagnostics,
} from "./diagnostics.mjs"

// The fallback lets missing implementation fail on behavior rather than import.
// All inputs below are modeled evidence; no addon, network or fixture is opened.
let assessOwnedLayoutRunnerOutcome = () => ({ runner_safe_to_continue: false, floor_qualified: false })
try {
  ;({ assessOwnedLayoutRunnerOutcome } = await import("./owned-layout-outcome.mjs"))
} catch (error) { if (error.code !== "ERR_MODULE_NOT_FOUND") throw error }

const providerId = "mount-rs-split-tidb-rustfs"
const zeros = (names) => Object.fromEntries(names.map((name) => [name, "0"]))
const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const claims = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]

function nativePhase(elapsed) {
  const storage = STORAGE_OPERATION_NAMES.map((name, index) => ({
    name, ...zeros(["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"]),
    ...(index === 0 ? { calls: "400", success: "400", elapsed_ns: "400000" } : {}),
    latency_log2_us: [index === 0 ? "400" : "0", ...Array(31).fill("0")],
  }))
  const instance = {
    id: "1", opened_during_phase: false,
    ...zeros(["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]),
    raw_api: {
      schema: "mount-rs.object-store-api.v1", scope: "one_object_store_block_store_instance",
      saturated_start: false, saturated_end: false, in_flight_start: "0", in_flight_end: "0",
      pending_claims_start: "0", pending_claims_end: "0", claims: zeros(claims),
      entries: rawNames.map((name) => ({ name,
        ...zeros(["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end"]),
        exact_phase_max_ns: "unavailable", latency_log2_us: Array(32).fill("0"),
      })),
    },
  }
  return {
    name: "workload-4096bytes", quiescent: true, elapsed_ms: elapsed + 1,
    benchmark_measured_elapsed_ms: elapsed, observer_snapshot_ms: 1,
    native: {
      schema_version: NATIVE_DIAGNOSTICS_SCHEMA, complete: true, issues: [],
      storage: { in_flight_start: "0", in_flight_end: "0", entries: storage },
      measurement: {
        rustfs: "live_store_logical_calls_and_cache_hits; not_http_attempts",
        rustfs_api: structuredClone(RUSTFS_API_MEASUREMENT),
        storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS,
        storage_rows: STORAGE_ROW_SEMANTICS, storage_operations: [...STORAGE_OPERATION_NAMES],
        storage_families: structuredClone(STORAGE_OPERATION_FAMILIES),
        storage_instrumented_operations: [...STORAGE_INSTRUMENTED_OPERATION_NAMES],
        tidb_coverage: structuredClone(TIDB_DIAGNOSTIC_COVERAGE),
        latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, bucket) => bucket === 0
          ? { lower_inclusive_us: "0", upper_exclusive_us: "1" }
          : bucket === 31 ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true }
            : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) }) },
      },
      rustfs: { scope: "process_live_instances", complete: true, internal_successful_retries: "unavailable",
        missing_instance_ids: [], instance_ids_start: ["1"], instance_ids_end: ["1"], instances: [instance] },
    },
  }
}

function backing(elapsed, status) {
  const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
  const allowlist = roles.map((role, index) => ({ role, cid: String(index + 1).repeat(64),
    labels: role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "model-rustfs" } : { "mount-rs.tidb.run": "model-tidb" } }))
  const boundary = (id, read) => ({ type: "boundary", id, kind: "phase", complete: true, issues: [],
    samples: allowlist.map((entry) => ({ cid: entry.cid,
      identity: { ...entry, image: `sha256:${"a".repeat(64)}`, running: true, started_at: "2026-09-27T00:00:00Z", restart_count: "0" },
      inspect_window: { dispatch_ms: 0, response_ms: 1 }, stats_window: { dispatch_ms: 1, response_ms: 2 },
      stats: { cid: entry.cid, read_ns: read, cpu_usage_ns: read, block_bytes: { "1:1:Read": read },
        block_operations: { "1:1:Read": read }, network_rx_bytes: { eth0: read }, network_tx_bytes: { eth0: read } },
    })),
  })
  const begin = boundary("workload-4096bytes:begin", "1000000000")
  const end = boundary("workload-4096bytes:end", "3000000000")
  const interval = { type: "interval", ...summarizeInterval(allowlist, begin, end, { workload_elapsed_ms: elapsed }),
    native_quiescent: true, owned_operations_settled: true, native_evidence_state: "complete" }
  const terminal = () => ({ status, native_quiescent: true, owned_operations_settled: true,
    native_profiling_enabled: true, workload_native_evidence_complete: true, operation_deadline_failed: false,
    cleanup_complete: true, prior_native_uncertainty: false, safe_to_continue_pair: true,
    quiescence_scope: "runner_workload_native_diagnostics_and_owned_operation_settlement" })
  return { schema: "mount-rs.runner-backing-observer.v1", complete: true, issues: [], terminal: terminal(),
    events: ["beginPhase", "endPhase", "finalize"].map((hook) => ({ hook, status: "ok",
      ...(hook === "finalize" ? {} : { phase: "workload-4096bytes" }),
      ...(hook === "endPhase" ? { owned_operations_settled: true, native_quiescent: true, native_evidence_state: "complete", pending_count: 0 } : {}) })),
    backing_evidence: { schema: "mount-rs.backing-observer.v1", complete: true, issues: [], dropped_entries: 0,
      terminal: terminal(), allowlist,
      cost: { in_flight: 0, pending_stages: Object.fromEntries(["headers", "body", "retirement"].map((stage) => [stage, { count: 0, age_ms: 0 }])) },
      journal: [{ type: "version", api_version: "1.51", mode: "stream=true;first-frame", platform: "linux", body_sha256: "b".repeat(64) }, begin, end, interval],
    },
  }
}

function model({ floor = true, layout = "legacy" } = {}) {
  const iops = floor ? 1001 : 394, elapsed = 1200000 / iops, status = floor ? "ok" : "failed"
  const rawSamples = Array.from({ length: 400 }, (_, index) => ({ provider: providerId,
    fileSizeBytes: 4096, iteration: index + 1, concurrencySlot: index % 64, path: `/model/${index}`,
    status: "ok", success: true, writeSucceeded: true, readSucceeded: true, deleteSucceeded: true,
    payloadVerified: true, bytesExpected: 4096, bytesReturned: 4096, cleanupSucceeded: true,
    cleanupFailure: false, timedOut: false, cleanupDeferred: false,
    timeoutCount: 0, timeoutOperations: [], lateOperations: [], errors: [],
  }))
  const result = { provider: providerId, status, layout, workload: "lifecycle", sizeMiB: 1,
    fileSizeBytes: 4096, writePayloadBytes: 4096, iterationsRequested: 400, concurrency: 64, chunkSizeBytes: 65536,
    summary: { elapsedMs: elapsed, iops, iopsTarget: 1000, iopsTargetMet: floor,
      successfulIterations: 400, failedIterations: 0, successfulOperations: 1200, attemptedOperations: 1200,
      operationsPerLifecycle: 3, timeoutCount: 0, cleanupFailureCount: 0,
      operationSuccess: { write: 400, read: 400, delete: 400, verifiedReads: 400 } },
    rawSamples,
    ...(floor ? {} : { failures: [{ operation: "iops-target", error: { name: "IopsTargetError", code: "IOPS_TARGET_NOT_MET", actual: iops, target: 1000, message: "PRIVATE_FLOOR_MESSAGE" } }] }),
  }
  return {
    schemaVersion: "mount-rs.storage-benchmark.v1", status,
    config: { layout, workload: "lifecycle", sizesMiB: [1], payloadSizesBytes: [4096], payloadBytes: 4096,
      iterations: 400, concurrency: 64, chunkSizeBytes: 65536, minIops: 1000,
      requireConfigured: true, storageDiagnosticsEnabled: true },
    configurationFailures: [], counts: { providersRequested: 1, providersFailed: floor ? 0 : 1,
      providersSkipped: 0, configurationFailures: 0, sizeResults: 1, sizeResultsFailed: floor ? 0 : 1, sizeResultsSkipped: 0 },
    providers: [{ provider: providerId, status, sizes: [result], missingConfiguration: [],
      ...(layout === "compact" ? { layoutSelection: { requested: "compact", selected: "compact",
        selectionEvidence: "createChunkedDriver-constructor-accepted",
        persistedMarkerEvidence: "metadata-MRC5-and-matching-block-authority-observed",
        persistedReceipt: { schema: "mount-rs.compact-layout-receipt.v1", marker: "MRC5",
          backingId: "a".repeat(32), structuralGeneration: "9", blockAuthorityVerified: true } } } : {}),
      cleanup: { pathsAttempted: 0, remainingPaths: 0, failures: [], pendingOperations: [], resource: { status: "ok" } },
      backingResourceCoverage: { create: "not_selected", "workload-4096bytes": "captured", cleanup: "not_selected", shutdown: "not_selected" },
      storageDiagnostics: { enabled: true, phases: [nativePhase(elapsed)] }, backingObserver: backing(elapsed, status) }],
    results: [result],
  }
}

test("the pure RustFS model independently satisfies the current raw diagnostic contract", () => {
  assert.deepEqual(validateRawPhaseDiagnostics(model().providers[0].storageDiagnostics.phases[0], "rustfs"), ["1"])
})

for (const layout of ["legacy", "compact"]) test(`clean ${layout} runner preserves qualification and continuation separately`, () => {
  const input = model({ layout }), before = structuredClone(input), outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.floor_qualified, true)
  assert.equal(outcome.runner_safe_to_continue, true)
  assert.equal(outcome.validated_runner_outcome, true)
  assert.equal(outcome.raw_workload_observed, true)
  assert.equal(outcome.sole_floor_failure, false)
  assert.deepEqual([outcome.status, outcome.provider_status, outcome.result_status, outcome.observer_status], ["ok", "ok", "ok", "ok"])
  assert.deepEqual(input, before)
})

test("sole original floor failure remains failed while permitting safe continuation", () => {
  const input = model({ floor: false }), outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.floor_qualified, false)
  assert.equal(outcome.sole_floor_failure, true)
  assert.equal(outcome.runner_safe_to_continue, true)
  assert.equal(outcome.raw_workload_observed, true)
  assert.deepEqual([outcome.status, outcome.provider_status, outcome.result_status, outcome.observer_status], ["failed", "failed", "failed", "failed"])
  assert.equal(outcome.result_failure_count, 1)
  assert.equal(outcome.sample_error_count, 0)
  assert.equal(input.results[0].failures[0].error.code, "IOPS_TARGET_NOT_MET")
})

function recomputeModeledInterval(benchmark, mutate) {
  const evidence = benchmark.providers[0].backingObserver.backing_evidence
  const boundaries = evidence.journal.filter((entry) => entry.type === "boundary")
  mutate(boundaries)
  Object.assign(evidence.journal.find((entry) => entry.type === "interval"),
    summarizeInterval(evidence.allowlist, boundaries[0], boundaries[1], { workload_elapsed_ms: benchmark.results[0].summary.elapsedMs }))
}

test("missing physical counters preserve settled logical qualification and honest coverage gaps", () => {
  const benchmark = model()
  recomputeModeledInterval(benchmark, (boundaries) => {
    for (const boundary of boundaries) for (const sample of boundary.samples) sample.stats.block_operations = null
    for (const boundary of boundaries) boundary.samples[0].stats.block_bytes = null
  })
  const outcome = assessOwnedLayoutRunnerOutcome(benchmark)
  assert.equal(outcome.runner_safe_to_continue, true)
  assert.equal(outcome.floor_qualified, true)
  assert.equal(outcome.validated_runner_outcome, true)
  assert.equal(benchmark.providers[0].backingObserver.backing_evidence.journal[3].metrics.block_operations.total, null)
})
for (const [name, mutate] of [
  ["counter reset", (boundaries) => { boundaries[1].samples[0].stats.block_bytes["1:1:Read"] = "0" }],
  ["changed device keys", (boundaries) => { boundaries[1].samples[0].stats.block_bytes = { "2:2:Read": "3000000000" } }],
  ["missing container", (boundaries) => { boundaries[0].samples.pop() }],
  ["nonincreasing timestamp", (boundaries) => { boundaries[1].samples[0].stats.read_ns = "1000000000" }],
  ["known reset hidden by unavailable interface", (boundaries) => { boundaries[0].samples[0].stats.network_rx_bytes = { a: null, b: "100" }; boundaries[1].samples[0].stats.network_rx_bytes = { a: null, b: "0" } }],
  ["device component outside u64", (boundaries) => { boundaries[0].samples[0].stats.block_bytes = { "18446744073709551616:1:Read": "100" }; boundaries[1].samples[0].stats.block_bytes = { "18446744073709551616:1:Read": "101" } }],
]) test(`partial accounting cannot excuse ${name}`, () => {
  const benchmark = model()
  recomputeModeledInterval(benchmark, (boundaries) => {
    for (const boundary of boundaries) for (const sample of boundary.samples) sample.stats.block_operations = null
    mutate(boundaries)
  })
  assert.equal(assessOwnedLayoutRunnerOutcome(benchmark).runner_safe_to_continue, false)
})

const rejectionCases = [
  ["duplicate provider", (v) => v.providers.push(structuredClone(v.providers[0]))],
  ["duplicate result", (v) => v.results.push(structuredClone(v.results[0]))],
  ["R2 provider", (v) => { v.providers[0].provider = "mount-rs-split-tidb-r2" }],
  ["R2 result", (v) => { v.results[0].provider = "mount-rs-split-tidb-r2" }],
  ["changed lifecycle", (v) => { v.config.workload = "steady-overwrite" }],
  ["changed concurrency", (v) => { v.config.concurrency = 32 }],
  ["changed chunk size", (v) => { v.config.chunkSizeBytes = 4096 }],
  ["changed floor", (v) => { v.config.minIops = 1 }],
  ["changed payload", (v) => { v.config.payloadBytes = 8192 }],
  ["missing sample", (v) => v.results[0].rawSamples.pop()],
  ["sparse sample array", (v) => { v.results[0].rawSamples = Array(400) }],
  ["duplicate iteration", (v) => { v.results[0].rawSamples[1].iteration = 1 }],
  ["dirty verified bytes", (v) => { v.results[0].rawSamples[0].bytesReturned = 4095 }],
  ["dirty operation flag", (v) => { v.results[0].rawSamples[0].deleteSucceeded = false }],
  ["sample timeout despite successful flags", (v) => { v.results[0].rawSamples[0].timeoutOperations.push("PRIVATE_OPERATION") }],
  ["late native work despite successful flags", (v) => v.results[0].rawSamples[0].lateOperations.push({ status: "pending" })],
  ["sample cleanup deferred", (v) => { v.results[0].rawSamples[0].cleanupDeferred = true }],
  ["invented numerator", (v) => { v.results[0].summary.operationsPerLifecycle = 2 }],
  ["inconsistent IOPS", (v) => { v.results[0].summary.iops = 2000 }],
  ["incomplete operation count", (v) => { v.results[0].summary.operationSuccess.read = 399 }],
  ["hidden failure count", (v) => { v.counts.providersFailed = 1 }],
  ["setup failure", (v) => { v.providers[0].setupError = { message: "PRIVATE_URL" } }],
  ["layout failure", (v) => { v.providers[0].layoutInspectionError = { message: "PRIVATE_URL" } }],
  ["configuration failure", (v) => v.configurationFailures.push({ reason: "PRIVATE_REASON" })],
  ["hidden missing configuration", (v) => v.providers[0].missingConfiguration.push("PRIVATE_SETTING")],
  ["hidden provider skip", (v) => { v.providers[0].skipReason = "PRIVATE_SKIP_REASON" }],
  ["remaining path", (v) => { v.providers[0].cleanup.remainingPaths = 1 }],
  ["pending native work", (v) => v.providers[0].cleanup.pendingOperations.push({ path: "PRIVATE_PATH" })],
  ["deferred resource cleanup", (v) => { v.providers[0].cleanup.resource.status = "deferred" }],
  ["late failed shutdown", (v) => { v.providers[0].cleanup.resource.status = "failed"; v.providers[0].cleanup.resource.cleanupCompleted = true }],
  ["missing raw RustFS family", (v) => { const p = v.providers[0].storageDiagnostics.phases[0]; p.native.r2 = p.native.rustfs; delete p.native.rustfs }],
  ["duplicate workload phase", (v) => v.providers[0].storageDiagnostics.phases.push(structuredClone(v.providers[0].storageDiagnostics.phases[0]))],
  ["raw pending claim", (v) => { v.providers[0].storageDiagnostics.phases[0].native.rustfs.instances[0].raw_api.pending_claims_end = "1" }],
  ["raw histogram forgery", (v) => { v.providers[0].storageDiagnostics.phases[0].native.rustfs.instances[0].raw_api.entries[0].latency_log2_us[0] = "1" }],
  ["sparse raw zero histogram", (v) => { v.providers[0].storageDiagnostics.phases[0].native.rustfs.instances[0].raw_api.entries[0].latency_log2_us = Array(32) }],
  ["sparse native identities", (v) => { v.providers[0].storageDiagnostics.phases[0].native.rustfs.instance_ids_start = Array(1) }],
  ["native elapsed mismatch", (v) => { v.providers[0].storageDiagnostics.phases[0].benchmark_measured_elapsed_ms = 1 }],
  ["native window shorter than measured work", (v) => { v.providers[0].storageDiagnostics.phases[0].elapsed_ms = 0.1 }],
  ["missing claimed factory evidence", (v) => { delete v.providers[0].backingObserver.backing_evidence }],
  ["incomplete original observer", (v) => { v.providers[0].backingObserver.complete = false }],
  ["lost workload capture", (v) => { v.providers[0].backingResourceCoverage["workload-4096bytes"] = "incomplete" }],
  ["pending observer retirement", (v) => { v.providers[0].backingObserver.backing_evidence.cost.pending_stages.retirement.count = 1 }],
  ["forged enclosing interval", (v) => { v.providers[0].backingObserver.backing_evidence.journal[3].metrics.cpu_usage_ns.total = "1" }],
  ["nonquiescent end hook", (v) => { v.providers[0].backingObserver.events[1].native_quiescent = false }],
  ["extra workload boundary", (v) => v.providers[0].backingObserver.backing_evidence.journal.push(structuredClone(v.providers[0].backingObserver.backing_evidence.journal[1]))],
  ["observer status rewritten", (v) => { v.providers[0].backingObserver.terminal.status = "failed" }],
]
for (const [name, mutate] of rejectionCases) test(`${name} blocks continuation without copying private data`, () => {
  const input = model()
  mutate(input)
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.runner_safe_to_continue, false)
  assert.equal(outcome.validated_runner_outcome, false)
  assert.ok(outcome.failure_codes.length > 0)
  assert.doesNotMatch(JSON.stringify(outcome), /PRIVATE/u)
})

for (const scope of ["terminal", "backing_evidence.terminal"]) {
  for (const [field, value] of Object.entries({ native_quiescent: false, owned_operations_settled: false,
    native_profiling_enabled: false, workload_native_evidence_complete: false, cleanup_complete: false,
    operation_deadline_failed: true, prior_native_uncertainty: true, safe_to_continue_pair: false })) {
    test(`${scope} ${field} cannot be hidden behind forged safe booleans`, () => {
      const input = model(), observer = input.providers[0].backingObserver
      const terminal = scope === "terminal" ? observer.terminal : observer.backing_evidence.terminal
      terminal[field] = value
      const outcome = assessOwnedLayoutRunnerOutcome(input)
      assert.equal(outcome.runner_safe_to_continue, false)
      assert.equal(outcome.floor_qualified, true, "original successful logical floor remains independent")
    })
  }
}

test("sample failure cannot hide behind the sole original floor failure", () => {
  const input = model({ floor: false })
  input.results[0].rawSamples[0].errors.push({ operation: "read", error: { code: "PRIVATE_CODE", message: "PRIVATE_PASSWORD" } })
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.sole_floor_failure, false)
  assert.equal(outcome.runner_safe_to_continue, false)
  assert.equal(outcome.sample_error_count, 1)
  assert.equal(outcome.result_failure_count, 1)
  assert.doesNotMatch(JSON.stringify(outcome), /PRIVATE/u)
})

test("sparse below-floor samples cannot masquerade as 400 completed lifecycles", () => {
  const input = model({ floor: false })
  input.results[0].rawSamples = Array(400)
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.sole_floor_failure, false)
  assert.equal(outcome.runner_safe_to_continue, false)
  assert.equal(outcome.sample_error_count, null)
})

test("cleanup status accessor cannot leak an unapproved status between reads", () => {
  const input = model()
  let reads = 0
  Object.defineProperty(input.providers[0].cleanup.resource, "status", { get: () => ++reads === 2 ? "PRIVATE_RESOURCE_STATUS" : "ok" })
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.runner_safe_to_continue, false)
  assert.doesNotMatch(JSON.stringify(outcome), /PRIVATE/u)
})

test("dynamic status accessors cannot rewrite the original floor outcome", () => {
  const input = model({ floor: false })
  let reads = 0
  Object.defineProperty(input, "status", { get: () => ++reads === 1 ? "ok" : "failed" })
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.runner_safe_to_continue, false)
  assert.equal(outcome.sole_floor_failure, false)
  assert.equal(outcome.status, "unavailable")
})

for (const [name, mutate] of [
  ["missing selection", (v) => { delete v.providers[0].layoutSelection }],
  ["requested layout mismatch", (v) => { v.providers[0].layoutSelection.requested = "legacy" }],
  ["selected layout mismatch", (v) => { v.providers[0].layoutSelection.selected = "legacy" }],
  ["constructor unaccepted", (v) => { v.providers[0].layoutSelection.selectionEvidence = "provider-constructor-not-yet-accepted" }],
  ["persisted proof unobserved", (v) => { v.providers[0].layoutSelection.persistedMarkerEvidence = "not-observed-by-benchmark-runner" }],
  ["missing persisted receipt", (v) => { delete v.providers[0].layoutSelection.persistedReceipt }],
  ["invalid persisted marker", (v) => { v.providers[0].layoutSelection.persistedReceipt.marker = "MRC2" }],
  ["unverified block authority", (v) => { v.providers[0].layoutSelection.persistedReceipt.blockAuthorityVerified = false }],
]) test(`original compact ${name} blocks continuation independently of prior arm proof`, () => {
  const input = model({ layout: "compact" })
  mutate(input)
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.runner_safe_to_continue, false)
  assert.equal(outcome.original_layout_proof_observed, false)
})

test("original compact persisted receipt is validated without retaining its identity", () => {
  const outcome = assessOwnedLayoutRunnerOutcome(model({ layout: "compact" }))
  assert.equal(outcome.original_layout_proof_observed, true)
  assert.doesNotMatch(JSON.stringify(outcome), /backingId|structuralGeneration|MRC5/u)
})

test("legacy null inspection never becomes persisted exact legacy proof", () => {
  const input = model()
  input.providers[0].layoutSelection = { requested: "legacy", selected: "legacy",
    selectionEvidence: "createChunkedDriver-constructor-accepted", persistedMarkerEvidence: "null-proves-exact-legacy", persistedReceipt: null }
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.runner_safe_to_continue, false)
  assert.equal(outcome.original_layout_proof_observed, false)
})

for (const mutation of [
  (v) => v.results[0].failures.push({ operation: "setup", error: { code: "PRIVATE_CODE" } }),
  (v) => { v.results[0].failures[0].operation = "PRIVATE_OPERATION" },
  (v) => { v.results[0].failures[0].error.code = "PRIVATE_CODE" },
  (v) => { v.results[0].failures[0].error.actual = 1 },
  (v) => { v.results[0].failures[0].error.target = 1 },
  (v) => { v.results[0].summary.iopsTargetMet = true },
  (v) => { v.status = "ok" },
]) test("inconsistent original floor failure cannot authorize the next arm", () => {
  const input = model({ floor: false })
  mutation(input)
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  assert.equal(outcome.sole_floor_failure, false)
  assert.equal(outcome.floor_qualified, false)
  assert.equal(outcome.runner_safe_to_continue, false)
})

test("projection is closed, deeply frozen, and preserves supported original statuses", () => {
  const input = model({ floor: false })
  input.environment = { credentials: "PRIVATE_PASSWORD" }
  input.results[0].extra = "PRIVATE_EXTRA"
  input.providers[0].cleanup.resource.extra = "PRIVATE_URL"
  const outcome = assessOwnedLayoutRunnerOutcome(input)
  const walk = (value) => { if (value && typeof value === "object") { assert.equal(Object.isFrozen(value), true); Object.values(value).forEach(walk) } }
  walk(outcome)
  assert.throws(() => { outcome.status = "ok" }, TypeError)
  assert.doesNotMatch(JSON.stringify(outcome), /PRIVATE|"(?:environment|instances|path|message)"/u)
  input.status = "PRIVATE_STATUS"
  assert.equal(assessOwnedLayoutRunnerOutcome(input).status, "unavailable")
  assert.equal(outcome.status, "failed", "input mutation cannot rewrite an existing projection")
})

test("malformed inputs return frozen closed unavailable evidence", () => {
  for (const input of [undefined, null, [], 1, "PRIVATE_TEXT", {}, { get providers() { throw new Error("PRIVATE_FAILURE") } }]) {
    const outcome = assessOwnedLayoutRunnerOutcome(input)
    assert.equal(outcome.runner_safe_to_continue, false)
    assert.equal(outcome.raw_workload_observed, false)
    assert.ok(Object.isFrozen(outcome))
    assert.doesNotMatch(JSON.stringify(outcome), /PRIVATE/u)
  }
})
