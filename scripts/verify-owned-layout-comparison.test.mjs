import assert from "node:assert/strict"
import test from "node:test"
import { createHash } from "node:crypto"
import { chmod, link, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import Module, { createRequire, syncBuiltinESMExports } from "node:module"
import childProcess from "node:child_process"
import http from "node:http"
import https from "node:https"
import net from "node:net"
import tls from "node:tls"
import dgram from "node:dgram"

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const verifierURL = new URL("./verify-owned-layout-comparison.mjs", import.meta.url)
const verifierPath = fileURLToPath(verifierURL), executeChild = childProcess.execFile.bind(childProcess)
const require = createRequire(import.meta.url), dispatches = { native: 0, network: 0, process: 0 }
const replacements = [], originalNative = Module._extensions[".node"]
const replace = (object, key, value) => { replacements.push([object, key, object[key]]); object[key] = value }
Module._extensions[".node"] = () => { dispatches.native++; throw Error("offline controls forbid native loading") }
const denyNetwork = () => { dispatches.network++; throw Error("offline controls forbid network dispatch") }
for (const [object, keys] of [[http, ["request", "get"]], [https, ["request", "get"]],
  [net, ["connect", "createConnection"]], [net.Socket.prototype, ["connect"]], [net.Server.prototype, ["listen"]],
  [tls, ["connect"]], [dgram, ["createSocket"]]]) for (const key of keys) replace(object, key, denyNetwork)
replace(globalThis, "fetch", denyNetwork)
for (const key of ["exec", "execSync", "execFile", "execFileSync", "spawn", "spawnSync", "fork"]) replace(childProcess, key, () => {
  dispatches.process++; throw Error("offline controls forbid subprocess dispatch")
})
syncBuiltinESMExports()

// Existing pure contracts may import node:http transitively. No method dispatch,
// entry, runner, provider, public addon loader, or installed native code is used.
const { summarizeInterval } = await import("../benchmarks/storage/backing-observer.mjs")
const { assessOwnedLayoutRunnerOutcome } = await import("../benchmarks/storage/owned-layout-outcome.mjs")
const { projectOwnedLayoutPhaseMetrics } = await import("../benchmarks/storage/owned-layout-metrics.mjs")
const { validateCompactLayoutReceipt } = await import("../benchmarks/storage/compact-layout.mjs")
const { validateSplitNamespacePresenceReceipt } = await import("../benchmarks/storage/namespace-presence.mjs")
const {
  NATIVE_DIAGNOSTICS_SCHEMA, OBJECT_STORE_LOCAL_NAMES, RUSTFS_API_MEASUREMENT, RUSTFS_LOCAL_MEASUREMENT, STORAGE_BYTE_SEMANTICS,
  STORAGE_CALL_SEMANTICS, STORAGE_INSTRUMENTED_OPERATION_NAMES, STORAGE_OPERATION_FAMILIES,
  STORAGE_OPERATION_NAMES, STORAGE_ROW_SEMANTICS, TIDB_DIAGNOSTIC_COVERAGE, validateRawPhaseDiagnostics,
} = await import("../benchmarks/storage/diagnostics.mjs")

// Missing production source gets an inert exported fallback. Behavior assertions
// fail semantically; denied-import CLI controls are explicitly skipped until it exists.
let implemented = true, api
try { api = await import(verifierURL.href) } catch (error) {
  if (error.code !== "ERR_MODULE_NOT_FOUND" || !error.message.includes("verify-owned-layout-comparison.mjs")) throw error
  implemented = false
  api = { SOURCE_PATHS: Object.freeze([]), verifyOwnedLayoutComparison: () => Object.freeze({}), main: async () => 1 }
}

const SOURCE_PATHS = Object.freeze([
  "Cargo.toml", "Cargo.lock", "bindings/mount-rs-napi/Cargo.toml", "bindings/mount-rs-napi/index.js",
  "bindings/mount-rs-napi/src/lib.rs", "bindings/mount-rs-napi/src/namespace_presence.rs", "src/diagnostics/storage.rs", "src/diagnostics/profile.rs",
  "scripts/test-tidb.sh", "scripts/test-rustfs.sh", "scripts/rustfs-combo-runner.py", "scripts/rustfs-bounded-docker.py",
  ".github/workflows/remote-drives.yml", "benchmarks/storage/errors.mjs", "benchmarks/storage/stats.mjs", "benchmarks/storage/runner.mjs",
  "benchmarks/storage/providers.mjs", "benchmarks/storage/diagnostics.mjs", "benchmarks/storage/backing-engine-transport.mjs",
  "benchmarks/storage/backing-observer.mjs", "benchmarks/storage/compact-layout.mjs", "benchmarks/storage/namespace-presence.mjs",
  "benchmarks/storage/owned-backing-pilot.mjs", "benchmarks/storage/owned-layout-arm.mjs", "benchmarks/storage/owned-layout-outcome.mjs",
  "benchmarks/storage/owned-layout-metrics.mjs", "benchmarks/storage/owned-layout-comparison.mjs", "benchmarks/storage/owned-layout-entry.mjs",
])
const secret = "PRIVATE_OFFLINE_SECRET", checkout = "d".repeat(40), providerId = "mount-rs-split-tidb-rustfs"
const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const order = ["A1", "B1", "B2", "A2"], layouts = ["legacy", "compact", "compact", "legacy"]
const metrics = ["cpu_usage_ns", "block_bytes", "block_operations", "network_rx_bytes", "network_tx_bytes"]
const observerCaps = Object.freeze({ maxContainers: 16, maxRequests: 257, maxBoundaries: 64, maxResponseBytes: 1_048_576,
  maxDeviceEntries: 128, maxNetworkInterfaces: 32, maxJournalBytes: 8_388_608, requestTimeoutMs: 2000, ownerTimeoutMs: 60_000 })
const observerRetention = "selected_projections_and_raw_version_inspect_or_first_stats_frame_sha256; consumed_trailing_bytes_counted_discarded; raw_bodies_discarded"
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
const json = (value) => Buffer.from(JSON.stringify(value))
const clone = (value) => structuredClone(value)
const canonical = (value) => value === null || typeof value !== "object" ? value : Array.isArray(value)
  ? value.map(canonical) : Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])]))
const evidenceScope = Object.freeze({
  verification: "offline_retained_record_consistency; no_runtime_or_endpoint_dispatch",
  source: "exact_28_reviewed_runtime_controller_seam_hashes; not_full_build_dependency_closure",
  build: "declared_seal_flags_and_bytes_joined; clean_checkout_and_build_not_independently_observed",
  container: "recomputed_selected_counter_intervals; raw_inspect_stats_bodies_not_retained_or_rehashed",
  capacity: "unavailable_missing_actual_owner_capacity_producer",
  owner_terminal: "unavailable_missing_actual_owner_terminal_producer",
  interpretation: "descriptive_ABBA_accounting; no_hosted_qualification_or_causal_physical_IOPS_proof",
})
const reportKeys = ["schema", "status", "runtime_scope", "hosted_qualified", "publication_complete", "originals_retained", "checked_arm_count",
  "floor_qualified", "comparable", "safe_to_continue", "native_uncertainty", "arms", "joins", "failure_codes", "evidence_scope"]
const code = (suffix) => `OWNED_LAYOUT_CHECK_${suffix}`
function frozen(value) {
  assert.equal(Object.isFrozen(value), true)
  for (const child of Object.values(value)) if (child && typeof child === "object") frozen(child)
}
function rejects(inputs, suffix) {
  const report = api.verifyOwnedLayoutComparison(inputs)
  assert.equal(report.status, "rejected", "malformed retained evidence must be rejected")
  assert.deepEqual(report.failure_codes, [code(suffix)])
  assert.equal(report.hosted_qualified, false)
  assert.equal(report.comparable, false)
  assert.equal(report.safe_to_continue, false)
  return report
}
test.after(() => {
  try {
    assert.deepEqual(dispatches, { native: 0, network: 0, process: 0 })
    assert.equal(Object.keys(require.cache).some((path) => path.endsWith(".node")), false)
  } finally {
    Module._extensions[".node"] = originalNative
    for (const [object, key, original] of replacements.reverse()) object[key] = original
    syncBuiltinESMExports()
  }
})

// Local copies of reviewed model fixtures. They are data, not live evidence.
function fixtureReceipt() {
  return { schema: "mount-rs.owned-backing-cids.v1", tidb_owner: "model-tidb", rustfs_owner: "model-rustfs", generation: "1",
    entries: roles.map((role, index) => ({ role, cid: String(index + 1).repeat(64), labels: role === "rustfs-service"
      ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "model-rustfs" }
      : { "mount-rs.tidb.run": "model-tidb" } })) }
}
const presence = () => ({
  schema: "mount-rs.split-namespace-presence.v1", namespace_absent: true,
  metadata: { provider: "tidb", key_scope: "exact_input_utf8_bytes", schema_setup: "shared_ddl_and_session_configuration",
    row_presence: { metadata: false, inodes: false, compact_guards: false, block_authority: false, blocks: false }, observed_at_ns: "1" },
  blobs: { provider: "rustfs", scope: "canonical_ascii_prefix_descendants", observation: "signed_list_page_api", requested_max_keys: 1, prefix_absent: true, observed_at_ns: "2" },
  pool_shutdown: { confirmed: true, elapsed_ns: "1" }, clock: "elapsed_monotonic_since_native_preflight_start", consistency: "separate_observations_no_reservation",
  limits: { probe_deadline_ms: 30000, pool_shutdown_deadline_ms: 15000, list_request_keys: 1,
    response_byte_cap: "unavailable", server_truncation_flag: "unavailable", deadline_semantics: "cooperative_await_with_elapsed_recheck" },
})
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

function modeledBacking(elapsed, status) {
  const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
  const allowlist = roles.map((role, index) => ({ role, cid: String(index + 1).repeat(64),
    labels: role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "model-rustfs" } : { "mount-rs.tidb.run": "model-tidb" } }))
  const boundary = (id, read) => ({ type: "boundary", id, kind: "phase", complete: true, issues: [],
    samples: allowlist.map((entry) => ({ cid: entry.cid,
      identity: { ...entry, image: `sha256:${"a".repeat(64)}`, running: true, started_at: "2026-09-27T00:00:00Z", restart_count: "0", limits: { Memory: "0", MemorySwap: "0", NanoCpus: "0", CpuQuota: "0", CpuPeriod: "100000", CpuShares: "0", PidsLimit: "0", CpusetCpus: "" }, configured_limit_semantics: "field_specific_zero_unset_and_minus_one_unlimited; not_observed_capacity" },
      inspect_body_sha256: "b".repeat(64), stats_body_sha256: "c".repeat(64), stats_frame_bytes: 512, stats_received_bytes: 512,
      inspect_window: { dispatch_ms: 0, headers_ms: 0.25, first_frame_ms: null, retired_ms: null, response_ms: 1,
        dispatch_utc: "2026-09-27T00:00:00Z", response_utc: "2026-09-27T00:00:01Z" },
      stats_window: { dispatch_ms: 1, headers_ms: 1.25, first_frame_ms: 1.5, retired_ms: 2, response_ms: 2,
        dispatch_utc: "2026-09-27T00:00:01Z", response_utc: "2026-09-27T00:00:02Z" },
      stats: { cid: entry.cid, read: new Date(Number(read) / 1_000_000).toISOString(), read_ns: read,
        cpu_usage_ns: read, cpu_user_ns: "0", cpu_kernel_ns: "0", system_cpu_usage_ns: read, online_cpus: "1",
        throttling: { periods: "0", throttled_periods: "0", throttled_ns: "0" },
        memory: { usage_bytes: "0", limit_bytes: "0", semantics: "api_gauges; not_cli_cache_adjusted" }, block_bytes: { "1:1:Read": read },
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
  const journal = [{ type: "version", api_version: "1.51", mode: "stream=true;first-frame", platform: "linux", body_sha256: "b".repeat(64) }, begin, end, interval]
  return { schema: "mount-rs.runner-backing-observer.v1", complete: true, issues: [], terminal: terminal(),
    events: ["beginPhase", "endPhase", "finalize"].map((hook) => ({ hook, status: "ok",
      ...(hook === "finalize" ? {} : { phase: "workload-4096bytes" }),
      ...(hook === "endPhase" ? { owned_operations_settled: true, native_quiescent: true, native_evidence_state: "complete", pending_count: 0 } : {}) })),
    backing_evidence: { schema: "mount-rs.backing-observer.v1", complete: true, issues: [], dropped_entries: 0, api_version: "1.51",
      caps: { ...observerCaps }, journal_bytes: journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0),
      terminal: terminal(), allowlist,
      cost: { requests: 33, response_bytes: 24_704, wall_ms: 33, cpu_user_us: 0, cpu_system_us: 0, peak_in_flight: 8,
        headers_received: 33, first_frames_received: 16, streamed_iterators_retired: 16, consumed_trailing_bytes: 0,
        in_flight: 0, owner_lifetime_ms: elapsed + 4,
        response_bytes_scope: "adapter_yielded_bytes_including_consumed_same_chunk_tail; rejected_overflow_chunks_unavailable; not_wire_bytes",
        wall_scope: "inclusive_request_windows; concurrent_windows_overlap; not_exclusive_hook_wall",
        cpu_scope: "inclusive_process_cpu_during_observer_calls; overlapping_work_not_isolated",
        pending_stages: Object.fromEntries(["headers", "body", "retirement"].map((stage) => [stage, { count: 0, age_ms: 0 }])) },
      journal, daemon_overhead: "unisolated", retention: observerRetention,
    },
  }
}

function modeledRunner({ floor = true, layout = "legacy" } = {}) {
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
      storageDiagnostics: { enabled: true, phases: [nativePhase(elapsed)] }, backingObserver: modeledBacking(elapsed, status) }],
    results: [result],
  }
}

function addMeasuredNativeCounters(result) {
  const phase = result.providers[0].storageDiagnostics.phases[0], native = phase.native, instance = native.rustfs.instances[0]
  Object.assign(native.storage.entries.find(({ name }) => name === "tidb.sql.inode_read"), {
    calls: "600", success: "600", returned_rows: "400", returned_row_observations: "600", elapsed_ns: "9007199254740993",
    latency_log2_us: ["600", ...Array(31).fill("0")],
  })
  Object.assign(instance.raw_api.entries[0], { calls: "4", success: "4", elapsed_ns: "4000", attempted_bytes: "16384", confirmed_bytes: "16384",
    latency_max_ns_end: "1000", latency_log2_us: ["4", ...Array(31).fill("0")] })
  native.measurement.rustfs_local = structuredClone(RUSTFS_LOCAL_MEASUREMENT)
  const entries = OBJECT_STORE_LOCAL_NAMES.map((name) => ({ name,
    ...zeros(["calls", "success", "error", "cancelled", "elapsed_ns", "input_bytes", "output_bytes", "latency_max_ns_start", "latency_max_ns_end"]),
    exact_phase_max_ns: "unavailable", latency_log2_us: Array(32).fill("0"),
  }))
  Object.assign(entries[0], { calls: "4", success: "4", elapsed_ns: "4000", input_bytes: "16384", output_bytes: "128",
    latency_max_ns_end: "1000", latency_log2_us: ["4", ...Array(31).fill("0")] })
  instance.local_work = { status: "observed", complete: true, schema: "mount-rs.object-store-local.v1", scope: "one_object_store_block_store_instance",
    saturated_start: false, saturated_end: false, in_flight_start: "0", in_flight_end: "0", entries }
}


const terminalFields = ["native_quiescent", "owned_operations_settled", "native_profiling_enabled", "workload_native_evidence_complete",
  "operation_deadline_failed", "cleanup_complete", "prior_native_uncertainty", "safe_to_continue_pair"]
function backingProjection(benchmark) {
  const original = benchmark.providers[0].backingObserver, backing = original.backing_evidence
  const terminal = (value) => ({ status: value.status, ...Object.fromEntries(terminalFields.map((key) => [key, value[key]])), quiescence_scope: value.quiescence_scope })
  const window = (value) => Object.fromEntries(["dispatch_ms", "headers_ms", "first_frame_ms", "retired_ms", "response_ms"].map((key) => [key, value[key]]))
  const boundaries = backing.journal.filter((entry) => entry.type === "boundary").map((boundary) => ({
    type: "boundary", id: boundary.id, kind: "phase", samples: boundary.samples.map((sample) => ({
      cid: sample.cid, identity: clone(sample.identity), stats: { ...clone(sample.stats), read: undefined },
      inspect_body_sha256: sample.inspect_body_sha256, stats_body_sha256: sample.stats_body_sha256,
      stats_frame_bytes: sample.stats_frame_bytes, stats_received_bytes: sample.stats_received_bytes,
      inspect_window: window(sample.inspect_window), stats_window: window(sample.stats_window),
    })),
  }))
  for (const boundary of boundaries) for (const sample of boundary.samples) delete sample.stats.read
  return { schema: "mount-rs.owned-layout-observer-projection.v1", original_schema: backing.schema, original_runner_schema: original.schema,
    complete: original.complete && backing.complete, runner_terminal: terminal(original.terminal), backing_terminal: terminal(backing.terminal),
    runner_issue_count: original.issues.length, backing_issue_count: backing.issues.length, original_event_count: original.events.length,
    original_journal_count: backing.journal.length, journal_bytes: backing.journal_bytes, dropped_entries: backing.dropped_entries,
    api_version: backing.api_version, version: clone(backing.journal[0]), caps: clone(backing.caps), cost: clone(backing.cost), boundaries,
    interval: summarizeInterval(backing.allowlist, ...boundaries, { workload_elapsed_ms: benchmark.results[0].summary.elapsedMs }),
    retention: "fixed_original_terminal_and_container_accounting_projections; raw_native_phase_inputs_private_for_strict_projection" }
}
function persistence(layout) {
  const compact = layout === "compact", flags = { concurrentWrites: compact, inodeUpdates: compact, compactInodeUpdates: compact }
  const observation = (generation) => compact ? { kind: "MRC5", receipt: { schema: "mount-rs.compact-layout-receipt.v1", marker: "MRC5",
    backingId: "a".repeat(32), structuralGeneration: String(generation), blockAuthorityVerified: true } }
    : { kind: "recognized_non_mrc5", exact_legacy_marker: false, fresh_preflight_required: true, constructor_flags: clone(flags) }
  return { schema: "mount-rs.owned-layout-arm.v1", available: true, layout, phase: "verified", complete: true,
    construction: { metadata_provider: "tidb", block_provider: "rustfs", chunk_size_bytes: 65536, metadata_durable: true, blocks_durable: false, flags },
    preflight: presence(), original_shutdown_confirmed: true, reopen_shutdown_confirmed_count: 2,
    layout_observations: { before_timed: observation(1), drained_original: observation(9), reopened: observation(9), removed_drained: observation(10), empty_reopened: observation(10) },
    canary: { payload_bytes: 4096, created: true, full_bytes_verified: true, eof_verified: true, remove_confirmed: true, logical_root_empty: true },
    counts: { preflight_calls: 1, initial_create_calls: 1, reopen_create_calls: 2, original_shutdown_calls: 1, reopen_shutdown_calls: 2,
      canary_write_calls: 1, canary_bytes_written: 4096, canary_sync_calls: 1, canary_write_close_calls: 1, canary_read_calls: 1,
      canary_bytes_read: 4096, canary_eof_read_calls: 1, canary_eof_bytes_read: 0, canary_read_close_calls: 1, canary_remove_calls: 1,
      initial_empty_calls: 1, final_empty_calls: 1, layout_inspection_calls: 5 },
    counter_scope: "untimed_helper_invocations_and_public_api_dispatches; not_backend_or_timed_workload_proof", failure_code: null, cleanup_codes: [] }
}
function fixture({ floor = true, tidbOwner = "model-tidb", rustfsOwner = "model-rustfs" } = {}) {
  const native = Buffer.from("inert native model bytes; never loaded\n")
  const sources = Object.fromEntries(SOURCE_PATHS.map((path) => [path, Buffer.from(`modeled seam bytes ${path}\n`)]))
  const fixtures = fixtureReceipt(), engine = { socket_path: "/var/run/docker.sock" }
  fixtures.tidb_owner = tidbOwner; fixtures.rustfs_owner = rustfsOwner
  for (const entry of fixtures.entries) entry.labels = entry.role === "rustfs-service"
    ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": rustfsOwner } : { "mount-rs.tidb.run": tidbOwner }
  const receipts = { fixtures: Buffer.from(JSON.stringify(fixtures, null, 2) + "\n"), engine: Buffer.from(' {"socket_path":"/var/run/docker.sock"}\n') }
  const id = `layout-${"a".repeat(32)}`
  const scope = { id, owner: fixtures.tidb_owner, metadataPrefix: `mount-rs-owned-layout/${fixtures.tidb_owner}/${id}`,
    blockPrefix: `mount-rs-owned-layout/${fixtures.rustfs_owner}/${id}` }
  const controller = { schema: "mount-rs.owned-layout-controller.v1", generation: "1", tidb_owner: fixtures.tidb_owner,
    rustfs_owner: fixtures.rustfs_owner, fixture_sha256: sha(receipts.fixtures), engine_capability_sha256: sha(receipts.engine),
    tidb_url: `mysql://user:${secret}@127.0.0.1:4000/storage`, rustfs_endpoint: "http://127.0.0.1:19000/", rustfs_bucket: "model-bucket", scope }
  const build = { schema: "mount-rs.owned-layout-native-build.v1", checkout_sha: checkout, source_clean: true, locked: true, release: true,
    no_js: true, build_exit_code: 0, native_sha256: sha(native), source_sha256: Object.fromEntries(SOURCE_PATHS.map((path) => [path, sha(sources[path])])) }
  receipts.controller = Buffer.from(JSON.stringify(controller, null, 2) + "\n"); receipts.build = Buffer.from(JSON.stringify(build, null, 2) + "\n")
  const benchmarks = layouts.map((layout) => {
    const benchmark = modeledRunner({ layout, floor }); addMeasuredNativeCounters(benchmark)
    const backing = benchmark.providers[0].backingObserver.backing_evidence
    backing.allowlist = clone(fixtures.entries)
    for (const boundary of backing.journal.filter((entry) => entry.type === "boundary"))
      for (const sample of boundary.samples) sample.identity.labels = clone(fixtures.entries.find((entry) => entry.cid === sample.cid).labels)
    rebuildBacking(benchmark)
    benchmark.providers[0].backingPilotIdentity = { native_used_identity: "verified", kind: "selected_native_file", native_sha256: sha(native) }
    benchmark.environment = { secret, endpoint: controller.tidb_url }
    return benchmark
  })
  const arm = (benchmark, index) => ({ role: order[index], layout: layouts[index], status: benchmark.status,
    outcome: clone(assessOwnedLayoutRunnerOutcome(benchmark)), persistence: persistence(layouts[index]), native_identity_verified: true,
    fixture_identity_stable: true, prepared: true, persistence_verified: true, comparison_safe_to_continue: true,
    backing: backingProjection(benchmark), native_metrics: clone(projectOwnedLayoutPhaseMetrics(benchmark.providers[0].storageDiagnostics.phases[0])) })
  const comparison = { schema: "mount-rs.owned-layout-comparison.v1", status: "complete", order: [...order],
    budgets_ms: { prepare: 245000, runner: 60000, persistence: 310000 }, completed_arms: 4, stopped_after: null, stop_code: null,
    native_uncertainty: false, pending_owned_call_count: 0, complete: true, comparable: true, floor_qualified: floor, safe_to_continue: true,
    arms: benchmarks.map(arm), evidence_scope: {
      workload: "original_400_lifecycle_iterations_concurrency64_payload4096_chunk65536_iops_floor1000",
      deadlines: "enclosing_cooperative_promise_ceilings; no_native_cancellation_guarantee; missed_deadline_permanently_stops_sequence",
      fixture_identity: "eight_owner_bound_allowlist_entries_and_existing_observed_container_identities; no_additional_engine_calls",
      endpoint_binding: "unobserved_here; requires_outer_entry_controller_endpoint_and_manifest_join", owner_teardown: "unobserved_here; requires_final_owner_verified_teardown",
      native_build_provenance: "selected_native_file_identity_join_only; requires_outer_entry_pinned_source_and_build_evidence", cache_state: "uncontrolled",
      interpretation: "descriptive_ABBA_accounting; not_cold_cache_physical_IOPS_or_causal_improvement_proof" },
    retention: "closed_bounded_redacted_projection_including_fixed_native_metrics; original_phase_records_private_for_independent_verification", output_cap_bytes: 33554432 }
  const originals = { schema: "mount-rs.owned-layout-originals.v1", runtime_scope: "modeled_controls", joins: {
    ...Object.fromEntries(["fixtures", "controller", "engine", "build"].map((key) => [key, { value: clone({ fixtures, controller, engine, build }[key]), sha256: sha(receipts[key]) }])),
    native: { sha256: sha(native), selection: `/private/owned/${secret}.node`, kind: "selected_native_file", native_used_identity: "unverified", profiling_enabled_before_load: true },
    scope: { sha256: sha(JSON.stringify(scope)), value: clone(scope) } },
    evidence: { schema: "mount-rs.owned-layout-private-evidence.v1", complete: true, arms: benchmarks.map((benchmark, index) => ({ role: order[index],
      cohort: { id: `${id}/${order[index]}`, layout: layouts[index], metadataKey: `${scope.metadataPrefix}/${order[index]}`,
        blockPrefix: `${scope.blockPrefix}/${order[index]}`, owner: scope.owner }, benchmark })),
      scope: "private_original_runner_input_for_independent_verification; public_native_metrics_are_separately_closed_projected_receipts" } }
  const projection = { schema: "mount-rs.owned-layout-entry.v1", runtime_scope: "modeled_controls", failure_code: null, publication: { status: "complete", reason: null }, comparison,
    handoff: { status: "verified", generation: "1", fixture_count: 8, fixture_sha256: sha(receipts.fixtures), engine_capability_sha256: sha(receipts.engine),
      scope_sha256: sha(JSON.stringify(scope)), endpoint_binding: "controller_manifest_join_verified", scope_binding: "owner_scoped_prefixes_verified",
      evidence_scope: "private_controller_created_endpoint_and_manifest_join; not_active_endpoint_probe", controller_receipt_sha256: sha(receipts.controller) },
    originals: null, identity: { kind: "selected_native_file", native_sha256: sha(native), native_used_identity: "unverified", profiling_enabled_before_load: true },
    build: { status: "verified", ...clone(build), receipt_sha256: sha(receipts.build) },
    ownership: { tidb_owner: fixtures.tidb_owner, rustfs_owner: fixtures.rustfs_owner, generation: "1", scope_sha256: sha(JSON.stringify(scope)),
      final_teardown: { status: "unverified" }, namespace_purge: "unverified" },
    qualification: { hosted: false, floor_qualified: floor, comparable: true, safe_to_continue: true, reason: "owner_teardown_and_independent_verification_required" },
    evidence_scope: { controller: "new_private_handoff_contract; producer_integration_not_established_here",
      source_and_build: "boundary_hash_and_clean_git_build_seal_observations; not_immutable_interval_proof", resource_floor: "unchanged_owned_controller_prerequisite; unobserved_here",
      execution: "pure_injected_model; not_live_or_performance_evidence" } }
  return { native, sources, receipts, fixtures, controller, engine, build, scope, benchmarks, comparison, originals, projection,
    refresh(index) { comparison.arms[index].outcome = clone(assessOwnedLayoutRunnerOutcome(benchmarks[index])); comparison.arms[index].status = benchmarks[index].status
      comparison.arms[index].backing = backingProjection(benchmarks[index]); comparison.arms[index].native_metrics = clone(projectOwnedLayoutPhaseMetrics(benchmarks[index].providers[0].storageDiagnostics.phases[0])) },
    inputs() { const bytes = json(canonical(originals)); projection.originals = { schema: originals.schema, status: "retained", sha256: sha(bytes), bytes: bytes.length, count: originals.evidence.arms.length }
      return { projection: json(projection), originals: bytes, ...receipts, native, sources } } }
}
function rebuildBacking(benchmark) {
  const backing = benchmark.providers[0].backingObserver.backing_evidence, boundaries = backing.journal.filter((entry) => entry.type === "boundary")
  backing.journal[3] = { type: "interval", ...summarizeInterval(backing.allowlist, ...boundaries, { workload_elapsed_ms: benchmark.results[0].summary.elapsedMs }),
    native_quiescent: true, owned_operations_settled: true, native_evidence_state: "complete" }
  backing.journal_bytes = backing.journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0)
}
function partialPhysical(model) {
  for (let index = 0; index < model.benchmarks.length; index++) {
    const benchmark = model.benchmarks[index], boundaries = benchmark.providers[0].backingObserver.backing_evidence.journal.filter((entry) => entry.type === "boundary")
    for (const boundary of boundaries) {
      for (const sample of boundary.samples) sample.stats.block_operations = null
      boundary.samples[0].stats.block_bytes = null
    }
    rebuildBacking(benchmark); model.refresh(index)
  }
}

// First-tranche controls concentrate on the accepted consistency gates.
test("modeled original evidence satisfies the existing pure producer contracts", () => {
  const model = fixture()
  assert.equal(model.inputs().originals.length < 33554432, true)
  for (let index = 0; index < 4; index++) {
    const benchmark = model.benchmarks[index], arm = model.comparison.arms[index]
    assert.equal(assessOwnedLayoutRunnerOutcome(benchmark).runner_safe_to_continue, true)
    assert.deepEqual(validateRawPhaseDiagnostics(benchmark.providers[0].storageDiagnostics.phases[0], "rustfs"), ["1"])
    assert.equal(arm.native_metrics.status, "observed")
    assert.equal(validateSplitNamespacePresenceReceipt(JSON.stringify(arm.persistence.preflight)).namespace_absent, true)
    if (layouts[index] === "compact") assert.equal(validateCompactLayoutReceipt(arm.persistence.layout_observations.reopened.receipt).structuralGeneration, "9")
  }
})
test("exports the exact immutable 28 seam paths without expanding build provenance", () => {
  assert.deepEqual(api.SOURCE_PATHS, SOURCE_PATHS); assert.equal(Object.isFrozen(api.SOURCE_PATHS), true)
})
test("complete modeled ABBA bytes yield a closed frozen consistency report with hosted qualification false", () => {
  const model = fixture(), inputs = model.inputs(), report = api.verifyOwnedLayoutComparison(inputs)
  assert.equal(report.status, "consistent")
  assert.deepEqual(Object.keys(report).sort(), [...reportKeys].sort())
  assert.equal(report.schema, "mount-rs.owned-layout-independent-check.v1")
  assert.equal(report.runtime_scope, "modeled_controls"); assert.equal(report.hosted_qualified, false)
  assert.equal(report.publication_complete, true); assert.equal(report.originals_retained, true); assert.equal(report.checked_arm_count, 4)
  assert.equal(report.floor_qualified, true); assert.equal(report.comparable, true); assert.equal(report.safe_to_continue, true); assert.equal(report.native_uncertainty, false)
  assert.deepEqual(report.failure_codes, []); assert.deepEqual(report.evidence_scope, evidenceScope)
  assert.deepEqual(report.arms.map(({ role, layout }) => ({ role, layout })), order.map((role, index) => ({ role, layout: layouts[index] })))
  assert.deepEqual(report.joins, { ...Object.fromEntries(["projection", "originals", "fixtures", "controller", "engine", "build", "native"].map((key) => [`${key}_sha256`, sha(inputs[key])])), source_count: 28, seam_hashes_match: true })
  frozen(report)
  assert.doesNotMatch(JSON.stringify(report), new RegExp(`${secret}|mysql://|127\\.0\\.0\\.1|socket_path|/private/owned|metadataPrefix|blockPrefix|/model/`, "u"))
})
test("sole below-floor failures remain failed while complete records remain comparable and safe", () => {
  const model = fixture({ floor: false }), report = api.verifyOwnedLayoutComparison(model.inputs())
  assert.equal(report.status, "consistent"); assert.equal(report.floor_qualified, false); assert.equal(report.comparable, true); assert.equal(report.safe_to_continue, true)
  assert.equal(report.hosted_qualified, false)
  for (const arm of report.arms) { assert.equal(arm.outcome.status, "failed"); assert.equal(arm.outcome.sole_floor_failure, true); assert.equal(arm.outcome.result_failure_count, 1); assert.equal(arm.outcome.iops, 394) }
})
test("missing daemon counters preserve null totals and independent partial totals without stopping settled work", () => {
  const model = fixture(); partialPhysical(model)
  assert.equal(model.comparison.arms.every((arm) => arm.outcome.runner_safe_to_continue), true)
  const report = api.verifyOwnedLayoutComparison(model.inputs())
  assert.equal(report.status, "consistent"); assert.equal(report.comparable, true); assert.equal(report.safe_to_continue, true)
  for (const arm of report.arms) {
    assert.deepEqual(arm.container_metrics.block_operations, { complete: false, total: null, partial_total: "0", missing_member_count: 8 })
    assert.deepEqual(arm.container_metrics.block_bytes, { complete: false, total: null, partial_total: "14000000000", missing_member_count: 1 })
    assert.deepEqual(arm.container_metrics.cpu_usage_ns, { complete: true, total: "16000000000", partial_total: "16000000000", missing_member_count: 0 })
  }
})
test("native SQL blob and local counters are independently retained; unavailable process metrics remain unavailable", () => {
  const model = fixture(), report = api.verifyOwnedLayoutComparison(model.inputs())
  assert.equal(report.status, "consistent")
  const native = report.arms[0].native_metrics
  assert.equal(native.storage.entries.find(({ name }) => name === "tidb.sql.inode_read").calls, "600")
  assert.equal(native.storage.entries.find(({ name }) => name === "tidb.sql.inode_read").elapsed_ns, "9007199254740993")
  assert.equal(native.rustfs.instances[0].raw_api.entries[0].confirmed_bytes, "16384")
  assert.equal(native.rustfs.instances[0].local_work.entries[0].output_bytes, "128")
  assert.equal(native.process.status, "unavailable"); assert.equal(native.forwarding_boxes.status, "unavailable"); assert.equal(native.core_profile.status, "unavailable")
  assert.equal(native.unavailable.physical_device_iops, true)
})
test("receipt byte hashes use the actual pretty input bytes and scope uses producer field order", () => {
  const model = fixture(), inputs = model.inputs(), sidecar = JSON.parse(inputs.originals)
  assert.notEqual(sha(inputs.fixtures), sha(json(sidecar.joins.fixtures.value)))
  assert.notEqual(sha(json(sidecar.joins.scope.value)), sidecar.joins.scope.sha256)
  assert.equal(api.verifyOwnedLayoutComparison(inputs).status, "consistent")
})
test("entry-valid leading underscore owners remain accepted by the offline handoff join", () => {
  const model = fixture({ tidbOwner: "_model-tidb", rustfsOwner: "_model-rustfs" })
  assert.equal(api.verifyOwnedLayoutComparison(model.inputs()).status, "consistent")
})
test("owners beyond the entry120-character ceiling are rejected", () => {
  rejects(fixture({ tidbOwner: "t".repeat(121), rustfsOwner: "r".repeat(121) }).inputs(), "HANDOFF_JOIN_INVALID")
})
for (const field of ["tidb_url", "rustfs_endpoint", "rustfs_bucket"]) test(`rejects entry-forbidden NUL in controller ${field}`, () => {
  const model = fixture(); model.controller[field] += "\0"
  model.receipts.controller = json(model.controller)
  model.originals.joins.controller = { value: clone(model.controller), sha256: sha(model.receipts.controller) }
  model.projection.handoff.controller_receipt_sha256 = sha(model.receipts.controller)
  rejects(model.inputs(), "HANDOFF_JOIN_INVALID")
})
for (const selection of ["/private/owned/selected.json", "/private/owned/../selected.node"])
  test(`rejects entry-forbidden native selection syntax ${selection.endsWith(".json") ? "suffix" : "normalization"}`, () => {
    const model = fixture(); model.originals.joins.native.selection = selection
    rejects(model.inputs(), "NATIVE_JOIN_INVALID")
  })
for (const key of ["fixtures", "controller", "engine", "build"]) test(`rejects byte-distinct equivalent ${key} receipt reserialization`, () => {
  const model = fixture(), inputs = model.inputs(); inputs[key] = json(JSON.parse(inputs[key]))
  rejects(inputs, "BYTE_JOIN_INVALID")
})
test("rejects private sidecar bytes changed without public byte receipt update", () => {
  const inputs = fixture().inputs(); inputs.originals = Buffer.concat([inputs.originals, Buffer.from("\n")]); rejects(inputs, "BYTE_JOIN_INVALID")
})
test("rejects equal receipt digest with a different embedded parsed value", () => {
  const model = fixture(); model.originals.joins.controller.value.rustfs_bucket = "foreign-bucket"; rejects(model.inputs(), "BYTE_JOIN_INVALID")
})
for (const [name, mutate, expected] of [
  ["native bytes", (model, inputs) => { inputs.native = Buffer.from("foreign native bytes") }, "NATIVE_JOIN_INVALID"],
  ["one seam byte buffer", (model, inputs) => { inputs.sources["Cargo.toml"] = Buffer.from("foreign seam bytes") }, "SOURCE_JOIN_INVALID"],
  ["missing seam", (model, inputs) => { delete inputs.sources["Cargo.toml"] }, "SOURCE_JOIN_INVALID"],
  ["extra seam", (model, inputs) => { inputs.sources["private-controller.py"] = Buffer.from(secret) }, "SOURCE_JOIN_INVALID"],
  ["scope ordered digest", (model) => { model.originals.joins.scope.sha256 = sha(json(canonical(model.scope))) }, "HANDOFF_JOIN_INVALID"],
  ["controller actual fixture hash", (model) => { model.originals.joins.controller.value.fixture_sha256 = "f".repeat(64); model.receipts.controller = json(model.originals.joins.controller.value); model.originals.joins.controller.sha256 = sha(model.receipts.controller); model.projection.handoff.controller_receipt_sha256 = sha(model.receipts.controller) }, "HANDOFF_JOIN_INVALID"],
]) test(`rejects changed ${name}`, () => { const model = fixture(), initial = model.inputs(); mutate(model, initial); rejects(name.includes("bytes") || name.includes("seam") ? initial : model.inputs(), expected) })

for (const [name, mutate, expected] of [
  ["ABBA roles", (model) => { model.comparison.arms[1].role = "A1" }, "COHORT_INVALID"],
  ["four pinned cohort metadata scope", (model) => { model.originals.evidence.arms[1].cohort.metadataKey = model.originals.evidence.arms[0].cohort.metadataKey }, "COHORT_INVALID"],
  ["original layout to role", (model) => { model.originals.evidence.arms[1].cohort.layout = "legacy" }, "COHORT_INVALID"],
  ["public outcome boolean", (model) => { model.comparison.arms[0].outcome.successful_operations = 1199 }, "ORIGINAL_OUTCOME_MISMATCH"],
  ["one actual raw sample", (model) => { model.benchmarks[0].results[0].rawSamples[17].payloadVerified = false }, "ORIGINAL_OUTCOME_MISMATCH"],
  ["original summary count", (model) => { model.benchmarks[0].results[0].summary.operationSuccess.read = 399 }, "ORIGINAL_OUTCOME_MISMATCH"],
  ["floor plus extra original error", (model) => { model.benchmarks[0].results[0].failures = [{ operation: "read", error: { code: secret, message: secret } }] }, "ORIGINAL_OUTCOME_MISMATCH"],
  ["hidden original pending native claim", (model) => { model.benchmarks[0].providers[0].storageDiagnostics.phases[0].native.rustfs.instances[0].raw_api.pending_claims_end = "1" }, "ORIGINAL_OUTCOME_MISMATCH"],
  ["duplicate workload phase", (model) => { const phases = model.benchmarks[0].providers[0].storageDiagnostics.phases; phases.push(clone(phases[0])) }, "ORIGINAL_OUTCOME_MISMATCH"],
  ["public SQL counter", (model) => { model.comparison.arms[0].native_metrics.storage.entries[0].calls = "399" }, "NATIVE_METRICS_MISMATCH"],
  ["unavailable process forged zero", (model) => { model.comparison.arms[0].native_metrics.process = { status: "observed", cpu_work: { user_us: "0", system_us: "0" } } }, "NATIVE_METRICS_MISMATCH"],
  ["preflight shutdown", (model) => { model.comparison.arms[0].persistence.preflight.pool_shutdown.confirmed = false }, "PERSISTENCE_INVALID"],
  ["original shutdown", (model) => { model.comparison.arms[0].persistence.original_shutdown_confirmed = false }, "PERSISTENCE_INVALID"],
  ["fresh reopen count", (model) => { model.comparison.arms[0].persistence.counts.reopen_create_calls = 1 }, "PERSISTENCE_INVALID"],
  ["canary EOF", (model) => { model.comparison.arms[0].persistence.canary.eof_verified = false }, "PERSISTENCE_INVALID"],
  ["all compact flags", (model) => { model.comparison.arms[1].persistence.construction.flags.compactInodeUpdates = false }, "PERSISTENCE_INVALID"],
  ["compact reopened generation", (model) => { model.comparison.arms[1].persistence.layout_observations.reopened.receipt.structuralGeneration = "10" }, "PERSISTENCE_INVALID"],
  ["compact identity", (model) => { model.comparison.arms[1].persistence.layout_observations.drained_original.receipt.backingId = "f".repeat(32) }, "PERSISTENCE_INVALID"],
  ["legacy exact marker claim", (model) => { model.comparison.arms[0].persistence.layout_observations.before_timed.exact_legacy_marker = true }, "PERSISTENCE_INVALID"],
  ["public backing aggregate", (model) => { model.comparison.arms[0].backing.interval.metrics.block_bytes.total = "1" }, "OBSERVER_INVALID"],
  ["public pending request stage", (model) => { model.comparison.arms[0].backing.cost.pending_stages.body.count = 1 }, "OBSERVER_INVALID"],
  ["late successful deadline", (model) => { model.comparison.native_uncertainty = true; model.comparison.pending_owned_call_count = 1 }, "CONTINUATION_INVALID"],
  ["weakened whole runner ceiling", (model) => { model.comparison.budgets_ms.runner = 60001 }, "CONTINUATION_INVALID"],
  ["floor qualification forged", (model) => { model.projection.qualification.floor_qualified = false }, "CONTINUATION_INVALID"],
  ["hosted qualification forged", (model) => { model.projection.qualification.hosted = true }, "PUBLICATION_INVALID"],
]) test(`rejects inconsistent ${name}`, () => { const model = fixture(); mutate(model); rejects(model.inputs(), expected) })

for (const [field, value] of [["image", `sha256:${"d".repeat(64)}`], ["started_at", "2026-09-27T00:00:01Z"], ["restart_count", "1"], ["limits.Memory", "1024"]])
  test(`rejects globally drifted ${field} despite locally matching safe boundary pairs`, () => {
    const model = fixture(), benchmark = model.benchmarks[1], boundaries = benchmark.providers[0].backingObserver.backing_evidence.journal.filter((entry) => entry.type === "boundary")
    for (const boundary of boundaries) { const identity = boundary.samples[0].identity
      if (field.startsWith("limits.")) identity.limits[field.split(".")[1]] = value; else identity[field] = value }
    rebuildBacking(benchmark); model.refresh(1)
    assert.equal(model.comparison.arms[1].outcome.runner_safe_to_continue, true)
    rejects(model.inputs(), "FIXTURE_IDENTITY_INVALID")
  })
test("rejects a known counter reset even when another daemon member is unavailable", () => {
  const model = fixture(); partialPhysical(model)
  const benchmark = model.benchmarks[0], boundaries = benchmark.providers[0].backingObserver.backing_evidence.journal.filter((entry) => entry.type === "boundary")
  boundaries[1].samples[1].stats.block_bytes["1:1:Read"] = "1"; rebuildBacking(benchmark); model.refresh(0)
  assert.equal(model.comparison.arms[0].outcome.runner_safe_to_continue, false)
  rejects(model.inputs(), "OBSERVER_INVALID")
})
test("rejects same-arm identity drift hidden behind missing physical counters", () => {
  const model = fixture(); partialPhysical(model)
  const benchmark = model.benchmarks[0], backing = benchmark.providers[0].backingObserver.backing_evidence
  backing.journal[2].samples[0].identity.restart_count = "1"; rebuildBacking(benchmark); model.refresh(0)
  rejects(model.inputs(), "OBSERVER_INVALID")
})
test("rejects a counter device-key drift rather than treating it as missing metric coverage", () => {
  const model = fixture(), benchmark = model.benchmarks[0], backing = benchmark.providers[0].backingObserver.backing_evidence
  backing.journal[2].samples[0].stats.block_bytes = { "2:1:Read": "3000000000" }; rebuildBacking(benchmark); model.refresh(0)
  rejects(model.inputs(), "OBSERVER_INVALID")
})
test("retained inspect and stats body digests are checked syntactically, never represented as rehashed raw bodies", () => {
  const model = fixture(), benchmark = model.benchmarks[0], backing = benchmark.providers[0].backingObserver.backing_evidence
  backing.journal[1].samples[0].inspect_body_sha256 = "e".repeat(64); rebuildBacking(benchmark); model.refresh(0)
  const report = api.verifyOwnedLayoutComparison(model.inputs())
  assert.equal(report.status, "consistent"); assert.equal(report.evidence_scope.container, evidenceScope.container)
})
test("honest stopped prefix preserves uncertainty and original status without fabricating later arms", () => {
  const model = fixture({ floor: false })
  model.originals.evidence.arms = model.originals.evidence.arms.slice(0, 1); model.originals.evidence.complete = false
  model.comparison.arms = [model.comparison.arms[0], { role: "B1", layout: "compact", status: "failed", outcome: null, persistence: null,
    native_identity_verified: false, fixture_identity_stable: false, prepared: false, persistence_verified: false,
    comparison_safe_to_continue: false, backing: null, native_metrics: null }]
  Object.assign(model.comparison, { status: "stopped", complete: false, completed_arms: 1, comparable: false, safe_to_continue: false,
    floor_qualified: false, stopped_after: "B1", stop_code: "COORDINATOR_PREPARE_TIMEOUT", native_uncertainty: true, pending_owned_call_count: 1 })
  Object.assign(model.projection.qualification, { floor_qualified: false, comparable: false, safe_to_continue: false })
  const report = api.verifyOwnedLayoutComparison(model.inputs())
  assert.equal(report.status, "incomplete"); assert.equal(report.native_uncertainty, true); assert.equal(report.checked_arm_count, 1)
  assert.equal(report.safe_to_continue, false); assert.equal(report.comparable, false); assert.equal(report.hosted_qualified, false)
  assert.deepEqual(report.failure_codes, [code("RECORD_INCOMPLETE")]); assert.equal(report.arms[0].outcome.status, "failed"); assert.equal(report.arms.length, 2)
  assert.equal(report.arms[1].outcome, null); assert.equal(report.arms[1].native_metrics, null)
})
function capturedFailedModel() {
  const model = fixture(), benchmark = model.benchmarks[0], provider = benchmark.providers[0], result = benchmark.results[0]
  benchmark.status = provider.status = result.status = "failed"
  benchmark.counts.providersFailed = benchmark.counts.sizeResultsFailed = 1
  Object.assign(result.rawSamples[0], { status: "failed", success: false, readSucceeded: false, payloadVerified: false,
    timedOut: true, timeoutCount: 1, timeoutOperations: ["read"], errors: [{ operation: "read", error: { code: "ETIMEDOUT", message: secret } }] })
  result.failures = [{ operation: "read", error: { code: "ETIMEDOUT", message: secret } }]
  provider.cleanup.pendingOperations = [{ operation: "read", path: secret }]
  provider.storageDiagnostics.phases[0].quiescent = false
  provider.storageDiagnostics.phases[0].native.rustfs.instances[0].raw_api.pending_claims_end = "1"
  for (const terminal of [provider.backingObserver.terminal, provider.backingObserver.backing_evidence.terminal])
    Object.assign(terminal, { status: "failed", native_quiescent: false, owned_operations_settled: false, operation_deadline_failed: true,
      cleanup_complete: false, prior_native_uncertainty: true, safe_to_continue_pair: false })
  const arm = model.comparison.arms[0], state = arm.persistence
  arm.status = "failed"; arm.outcome = clone(assessOwnedLayoutRunnerOutcome(benchmark))
  arm.native_metrics = clone(projectOwnedLayoutPhaseMetrics(provider.storageDiagnostics.phases[0]))
  Object.assign(arm, { backing: null, fixture_identity_stable: false, persistence_verified: false, comparison_safe_to_continue: false })
  Object.assign(state, { phase: "handed_off", complete: false, original_shutdown_confirmed: false, reopen_shutdown_confirmed_count: 0 })
  for (const key of ["drained_original", "reopened", "removed_drained", "empty_reopened"]) state.layout_observations[key] = null
  for (const key of ["full_bytes_verified", "eof_verified", "remove_confirmed", "logical_root_empty"]) state.canary[key] = false
  for (const key of ["reopen_create_calls", "original_shutdown_calls", "reopen_shutdown_calls", "canary_read_calls", "canary_bytes_read",
    "canary_eof_read_calls", "canary_read_close_calls", "canary_remove_calls", "final_empty_calls"]) state.counts[key] = 0
  state.counts.canary_eof_bytes_read = null; state.counts.layout_inspection_calls = 1
  model.originals.evidence.arms = model.originals.evidence.arms.slice(0, 1); model.originals.evidence.complete = false
  Object.assign(model.comparison, { arms: [arm], status: "stopped", complete: false, completed_arms: 0, comparable: false, safe_to_continue: false,
    floor_qualified: false, stopped_after: "A1", stop_code: "COORDINATOR_RUNNER_OUTCOME_INVALID", native_uncertainty: true, pending_owned_call_count: 0 })
  Object.assign(model.projection.qualification, { floor_qualified: false, comparable: false, safe_to_continue: false })
  return model
}
test("captured failed original with null public backing stays incomplete and preserves pending uncertainty", () => {
  const model = capturedFailedModel(), arm = model.comparison.arms[0]
  assert.equal(arm.outcome.runner_safe_to_continue, false); assert.equal(arm.outcome.pending_operation_count, 1)
  const report = api.verifyOwnedLayoutComparison(model.inputs())
  assert.equal(report.status, "incomplete"); assert.equal(report.checked_arm_count, 1); assert.equal(report.native_uncertainty, true)
  assert.equal(report.comparable, false); assert.equal(report.safe_to_continue, false); assert.equal(report.hosted_qualified, false)
  assert.deepEqual(report.arms[0].outcome, arm.outcome); assert.equal(report.arms[0].outcome.status, "failed")
  assert.equal(report.arms[0].container_metrics, null); assert.equal(report.arms[0].native_metrics.status, "unavailable")
  assert.deepEqual(report.failure_codes, [code("RECORD_INCOMPLETE")])
})
test("rejects erased source-implied stopped native uncertainty while preserving the original failed outcome", () => {
  const model = capturedFailedModel(); model.comparison.native_uncertainty = false
  const report = rejects(model.inputs(), "CONTINUATION_INVALID")
  assert.equal(report.native_uncertainty, true)
  assert.equal(report.arms[0].outcome.status, "failed"); assert.equal(report.arms[0].outcome.pending_operation_count, 1)
})
test("cap-reduced public envelope with valid complete originals stays incomplete without inventing persistence", () => {
  const model = fixture({ floor: false }), original = model.comparison
  model.projection.failure_code = "OWNED_LAYOUT_ENTRY_OUTPUT_CAP"; model.projection.publication = { status: "incomplete", reason: "OWNED_LAYOUT_ENTRY_OUTPUT_CAP" }
  model.projection.comparison = { schema: "mount-rs.owned-layout-comparison-incomplete.v1", status: original.status, complete: false,
    comparable: false, safe_to_continue: false, floor_qualified: original.floor_qualified, native_uncertainty: false, pending_owned_call_count: 0,
    completed_arms: 4, stopped_after: null, stop_code: null, required_evidence: "omitted_output_cap_or_publication_failure",
    arms: original.arms.map(({ role, layout, status, outcome }) => ({ role, layout, status, outcome })) }
  Object.assign(model.projection.qualification, { comparable: false, safe_to_continue: false })
  const report = api.verifyOwnedLayoutComparison(model.inputs())
  assert.equal(report.status, "incomplete"); assert.equal(report.originals_retained, true); assert.equal(report.checked_arm_count, 4)
  assert.equal(report.publication_complete, false); assert.equal(report.floor_qualified, false); assert.equal(report.comparable, false); assert.equal(report.safe_to_continue, false)
  assert.deepEqual(report.failure_codes, [code("RECORD_INCOMPLETE")]); assert.equal(report.arms.every((arm) => arm.outcome.status === "failed"), true)
})

for (const [name, make, expected] of [
  ["non-record input", () => [], "INPUT_SHAPE_INVALID"],
  ["non-buffer projection", (inputs) => ({ ...inputs, projection: JSON.parse(inputs.projection) }), "BUFFER_REQUIRED"],
  ["sparse source list", (inputs) => ({ ...inputs, sources: Array(28) }), "SOURCE_JOIN_INVALID"],
  ["malformed JSON", (inputs) => ({ ...inputs, engine: Buffer.from("{") }), "JSON_INVALID"],
  ["duplicate decoded JSON key", (inputs) => ({ ...inputs, engine: Buffer.from('{"socket_path":"/var/run/docker.sock","socket\\u005fpath":"/var/run/docker.sock"}') }), "JSON_INVALID"],
  ["invalid UTF8", (inputs) => ({ ...inputs, engine: Buffer.from([0x7b, 0x22, 0x61, 0x22, 0x3a, 0x22, 0xc0, 0xaf, 0x22, 0x7d]) }), "JSON_INVALID"],
  ["receipt over 16KiB", (inputs) => ({ ...inputs, engine: Buffer.alloc(16385, 32) }), "INPUT_CAP_EXCEEDED"],
  ["build over 64KiB", (inputs) => ({ ...inputs, build: Buffer.alloc(65537, 32) }), "INPUT_CAP_EXCEEDED"],
  ["projection over independent 32MiB", (inputs) => ({ ...inputs, projection: Buffer.alloc(33554433, 32) }), "INPUT_CAP_EXCEEDED"],
  ["originals over independent 32MiB", (inputs) => ({ ...inputs, originals: Buffer.alloc(33554433, 32) }), "INPUT_CAP_EXCEEDED"],
  ["addon over 128MiB", (inputs) => ({ ...inputs, native: Buffer.alloc(134217729) }), "INPUT_CAP_EXCEEDED"],
  ["source over 2MiB", (inputs) => ({ ...inputs, sources: { ...inputs.sources, "Cargo.toml": Buffer.alloc(2097153) } }), "INPUT_CAP_EXCEEDED"],
]) test(`rejects ${name} with a fixed closed code`, () => rejects(make(fixture().inputs()), expected))
test("rejects untrusted root getters without reading them", () => {
  const inputs = fixture().inputs(); let reads = 0
  Object.defineProperty(inputs, "projection", { enumerable: true, get() { reads++; throw Error(secret) } })
  rejects(inputs, "INPUT_SHAPE_INVALID"); assert.equal(reads, 0)
})
test("rejects root proxies before any proxy trap", () => {
  let traps = 0
  const input = new Proxy(fixture().inputs(), { ownKeys() { traps++; throw Error(secret) }, getOwnPropertyDescriptor() { traps++; throw Error(secret) }, getPrototypeOf() { traps++; throw Error(secret) }, get() { traps++; throw Error(secret) } })
  rejects(input, "INPUT_SHAPE_INVALID"); assert.equal(traps, 0)
})
test("rejects source getters without invoking secret callbacks", () => {
  const inputs = fixture().inputs(); let reads = 0
  Object.defineProperty(inputs.sources, "Cargo.toml", { enumerable: true, get() { reads++; throw Error(secret) } })
  rejects(inputs, "SOURCE_JOIN_INVALID"); assert.equal(reads, 0)
})
test("rejects proxied Buffer without invoking traps", () => {
  const inputs = fixture().inputs(); let traps = 0
  inputs.native = new Proxy(inputs.native, { get() { traps++; throw Error(secret) }, getPrototypeOf() { traps++; throw Error(secret) } })
  rejects(inputs, "BUFFER_REQUIRED"); assert.equal(traps, 0)
})

async function files(t, model = fixture()) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "mount-rs-offline-layout-"))); await chmod(root, 0o700)
  t.after(() => rm(root, { force: true, recursive: true }))
  const inputs = model.inputs(), paths = Object.fromEntries(["projection", "originals", "fixtures", "controller", "engine", "build", "native"].map((key) => [key, join(root, `${key}.data`)]))
  for (const key of Object.keys(paths)) await writeFile(paths[key], inputs[key], { mode: 0o600 })
  paths.checkout = join(root, "checkout"); await mkdir(paths.checkout, { mode: 0o700 })
  for (const path of SOURCE_PATHS) { const target = join(paths.checkout, path); await mkdir(dirname(target), { recursive: true, mode: 0o700 }); await writeFile(target, inputs.sources[path], { mode: 0o600 }) }
  const argv = Object.entries(paths).flatMap(([key, value]) => [`--${key}`, value])
  return { root, paths, argv }
}
async function child(t, args, { importOnly = false, sourceMutationPath = null, directPath = verifierPath } = {}) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "mount-rs-offline-guard-"))); await chmod(root, 0o700)
  t.after(() => rm(root, { force: true, recursive: true }))
  const guard = join(root, "deny.cjs")
  await writeFile(guard, `const Module=require('node:module');const cp=require('node:child_process');\n` +
    (sourceMutationPath === null ? "" : `const fsp=require('node:fs/promises');const open=fsp.open.bind(fsp),write=fsp.writeFile.bind(fsp),read=fsp.readFile.bind(fsp);let changed=false,firstClosed=false;fsp.open=async(...args)=>{if(firstClosed&&!changed&&args[0]!==${JSON.stringify(sourceMutationPath)}){changed=true;await write(${JSON.stringify(sourceMutationPath)},Buffer.concat([await read(${JSON.stringify(sourceMutationPath)}),Buffer.from('changed-after-own-read')]));}const handle=await open(...args);if(args[0]===${JSON.stringify(sourceMutationPath)}){const close=handle.close.bind(handle);handle.close=async()=>{await close();firstClosed=true;};}return handle;};\n`) +
    `const deny=()=>{process.stderr.write('FORBIDDEN_DISPATCH\\n');throw Error('FORBIDDEN_DISPATCH')};\n` +
    `Module._extensions['.node']=deny;for(const key of ['exec','execSync','execFile','execFileSync','spawn','spawnSync','fork'])cp[key]=deny;\n` +
    `for(const [name,keys] of [['node:http',['request','get']],['node:https',['request','get']],['node:net',['connect','createConnection']],['node:tls',['connect']],['node:dgram',['createSocket']]]){const mod=require(name);for(const key of keys)mod[key]=deny;}\n` +
    `require('node:net').Socket.prototype.connect=deny;require('node:net').Server.prototype.listen=deny;global.fetch=deny;Module.syncBuiltinESMExports();\n` +
    `Module.registerHooks({resolve(specifier,context,next){if(/owned-layout-entry\\.mjs|storage\\/(runner|providers)\\.mjs|mount-rs-napi|\\.node$/.test(specifier))deny();return next(specifier,context)}});\n`, { mode: 0o600 })
  const launch = importOnly ? ["--require", guard, "--input-type=module", "-e", `await import(${JSON.stringify(verifierURL.href)});console.log('INERT_IMPORT_OK')`] : ["--require", guard, directPath, ...args]
  return new Promise((resolveResult) => executeChild(process.execPath, launch, { timeout: 5000, maxBuffer: 1048576, env: { PATH: process.env.PATH, LANG: "C" } },
    (error, stdout, stderr) => resolveResult({ code: error ? error.code : 0, signal: error?.signal || null, killed: error?.killed || false, stdout, stderr })))
}
function normalChild(result, expected) {
  assert.equal(result.signal, null, "guarded CLI signal is a test failure")
  assert.equal(result.killed, false, "guarded CLI timeout is a test failure")
  assert.equal(result.code, expected)
  assert.doesNotMatch(result.stdout + result.stderr, /FORBIDDEN_DISPATCH|PRIVATE_OFFLINE_SECRET|mysql:\/\/|\/private\/owned/u)
}
const cli = { skip: !implemented && "production file is absent; real guarded CLI controls wait for implementation" }
test("normal import is inert under denied native network and subprocess dispatch", cli, async (t) => {
  const result = await child(t, [], { importOnly: true }); normalChild(result, 0); assert.match(result.stdout, /INERT_IMPORT_OK/u)
})
test("actual CLI parses exactly eight paths and succeeds only for offline consistency", cli, async (t) => {
  const fixtureFiles = await files(t), result = await child(t, fixtureFiles.argv); normalChild(result, 0)
  const report = JSON.parse(result.stdout); assert.equal(report.status, "consistent"); assert.equal(report.hosted_qualified, false)
})
test("direct executable symlink emits a closed consistency report under denied runtime dispatch", cli, async (t) => {
  const fixtureFiles = await files(t), executableAlias = join(fixtureFiles.root, "verifier-link.mjs")
  await symlink(verifierPath, executableAlias)
  const result = await child(t, fixtureFiles.argv, { directPath: executableAlias }); normalChild(result, 0)
  assert.notEqual(result.stdout.trim(), "", "direct CLI exit zero must include its consistency report")
  const report = JSON.parse(result.stdout)
  assert.deepEqual(Object.keys(report).sort(), [...reportKeys].sort())
  assert.equal(report.schema, "mount-rs.owned-layout-independent-check.v1")
  assert.equal(report.status, "consistent"); assert.equal(report.hosted_qualified, false)
})
test("actual CLI rejects missing and duplicate arguments without any runtime dispatch", cli, async (t) => {
  const fixtureFiles = await files(t)
  for (const args of [[], [...fixtureFiles.argv, "--projection", fixtureFiles.paths.projection]]) {
    const result = await child(t, args); normalChild(result, 1); assert.deepEqual(JSON.parse(result.stdout).failure_codes, [code("ARGUMENTS_INVALID")])
  }
})
test("actual CLI rechecks a source changed after its own completed read", cli, async (t) => {
  const fixtureFiles = await files(t), result = await child(t, fixtureFiles.argv, { sourceMutationPath: join(fixtureFiles.paths.checkout, "Cargo.toml") })
  normalChild(result, 1); assert.deepEqual(JSON.parse(result.stdout).failure_codes, [code("FILE_INVALID")])
})
for (const [name, mutate] of [
  ["public JSON permissions", async (paths) => chmod(paths.projection, 0o644)],
  ["nonprivate parent", async (paths) => chmod(dirname(paths.projection), 0o755)],
  ["symlink JSON input", async (paths) => { await rm(paths.originals); await symlink(paths.projection, paths.originals) }],
  ["hardlinked input bytes", async (paths) => { await rm(paths.originals); await link(paths.projection, paths.originals) }],
  ["canonical ancestor alias", async (paths, fixtureFiles) => { const alias = join(fixtureFiles.root, "parent-alias"); await symlink(fixtureFiles.root, alias); paths.originals = join(alias, "projection.data") }],
  ["native input alias", async (paths) => { await rm(paths.native); await link(paths.build, paths.native) }],
]) test(`actual CLI rejects ${name} with read-only bounded file checks`, cli, async (t) => {
  const fixtureFiles = await files(t); await mutate(fixtureFiles.paths, fixtureFiles)
  const argv = Object.entries(fixtureFiles.paths).flatMap(([key, value]) => [`--${key}`, value]), result = await child(t, argv)
  normalChild(result, 1); assert.deepEqual(JSON.parse(result.stdout).failure_codes, [code("FILE_INVALID")])
})
