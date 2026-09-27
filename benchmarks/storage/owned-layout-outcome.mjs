import { isDeepStrictEqual } from "node:util"
import { summarizeInterval } from "./backing-observer.mjs"
import { validateCompactLayoutReceipt } from "./compact-layout.mjs"
import { validateRawPhaseDiagnostics } from "./diagnostics.mjs"

const PROVIDER = "mount-rs-split-tidb-rustfs"
const WORKLOAD = "workload-4096bytes"
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value)
const empty = (value) => Array.isArray(value) && value.length === 0
const status = (value) => ["ok", "failed", "skipped"].includes(value) ? value : "unavailable"
const count = (value) => Number.isSafeInteger(value) && value >= 0 ? value : null
const finite = (value) => Number.isFinite(value) && value >= 0 ? value : null
const boolean = (value) => typeof value === "boolean" ? value : null
const failures = (result) => result?.failures === undefined ? [] : result.failures

// Capture data once: getters cannot rewrite a status between projection and
// validation. Ignore the benchmark environment and other unused root fields.
function snapshotBenchmark(source) {
  if (!object(source)) return source
  const state = { nodes: 0, bytes: 0, seen: new WeakMap(), active: new WeakSet() }
  const clone = (value, depth = 0) => {
    if (++state.nodes > 200_000 || depth > 32) throw new Error("evidence_cap")
    if (typeof value === "string") {
      state.bytes += Buffer.byteLength(value)
      if (state.bytes > 8_388_608) throw new Error("evidence_cap")
      return value
    }
    if (value === null || value === undefined || ["boolean", "number"].includes(typeof value)) return value
    if (typeof value !== "object" || state.active.has(value)) throw new Error("invalid_evidence")
    if (state.seen.has(value)) return state.seen.get(value)
    const array = Array.isArray(value), prototype = Object.getPrototypeOf(value)
    if (!array && prototype !== Object.prototype && prototype !== null) throw new Error("invalid_evidence")
    const descriptors = Object.getOwnPropertyDescriptors(value), keys = Reflect.ownKeys(descriptors)
    if (keys.length > 200_000 - state.nodes) throw new Error("evidence_cap")
    const length = array ? descriptors.length?.value : null
    if (array && (!Number.isSafeInteger(length) || length < 0 || length > 200_000)) throw new Error("evidence_cap")
    if (array && (keys.length !== length + 1 || !Array.from({ length }, (_, index) => index).every((index) => Object.hasOwn(descriptors, String(index))))) throw new Error("invalid_evidence")
    const target = array ? Array(length) : {}
    state.seen.set(value, target)
    state.active.add(value)
    for (const key of keys) {
      const descriptor = descriptors[key]
      if (typeof key !== "string" || !Object.hasOwn(descriptor, "value")) throw new Error("invalid_evidence")
      state.bytes += Buffer.byteLength(key)
      if (state.bytes > 8_388_608) throw new Error("evidence_cap")
      if (array && key === "length") continue
      Object.defineProperty(target, key, { value: clone(descriptor.value, depth + 1), enumerable: true, writable: true, configurable: true })
    }
    state.active.delete(value)
    return target
  }
  const snapshot = {}
  for (const key of ["schemaVersion", "status", "config", "counts", "configurationFailures", "providers", "results"]) {
    const descriptor = Object.getOwnPropertyDescriptor(source, key)
    if (descriptor && !Object.hasOwn(descriptor, "value")) throw new Error("invalid_evidence")
    snapshot[key] = clone(descriptor?.value)
  }
  return snapshot
}

function freshProjection() {
  return {
    schema: "mount-rs.owned-layout-runner-outcome.v1",
    status: "unavailable", provider_status: "unavailable", result_status: "unavailable",
    observer_status: "unavailable", backing_status: "unavailable",
    floor_qualified: false, sole_floor_failure: false, runner_safe_to_continue: false,
    validated_runner_outcome: false, raw_workload_observed: false, original_layout_proof_observed: false,
    iops: null, elapsed_ms: null, iops_target: null, iops_target_met: null,
    successful_iterations: null, failed_iterations: null, successful_operations: null,
    attempted_operations: null, verified_reads: null, timeout_count: null, cleanup_failure_count: null,
    raw_sample_count: null, sample_error_count: null, result_failure_count: null,
    remaining_paths: null, path_cleanup_failure_count: null, pending_operation_count: null,
    resource_cleanup_status: "unavailable", original_safe_to_continue_pair: null,
    native_quiescent: null, owned_operations_settled: null, failure_codes: [],
  }
}

function cleanSample(sample, index) {
  return object(sample) && sample.provider === PROVIDER && sample.fileSizeBytes === 4096 &&
    sample.iteration === index + 1 && Number.isSafeInteger(sample.concurrencySlot) && sample.concurrencySlot >= 0 && sample.concurrencySlot < 64 &&
    sample.status === "ok" && sample.success === true && sample.writeSucceeded === true &&
    sample.readSucceeded === true && sample.deleteSucceeded === true && sample.payloadVerified === true &&
    sample.bytesExpected === 4096 && sample.bytesReturned === 4096 && sample.cleanupSucceeded === true &&
    sample.cleanupFailure === false && sample.timedOut === false && sample.cleanupDeferred === false &&
    sample.timeoutCount === 0 && empty(sample.timeoutOperations) && empty(sample.lateOperations) && empty(sample.errors) &&
    !Object.hasOwn(sample, "failureOperation")
}

function fixedConfig(benchmark, result) {
  const config = benchmark.config
  return benchmark.schemaVersion === "mount-rs.storage-benchmark.v1" && object(config) &&
    ["legacy", "compact"].includes(config.layout) && config.workload === "lifecycle" &&
    isDeepStrictEqual(config.sizesMiB, [1]) && isDeepStrictEqual(config.payloadSizesBytes, [4096]) &&
    config.payloadBytes === 4096 && config.iterations === 400 && config.concurrency === 64 &&
    config.chunkSizeBytes === 65536 && config.minIops === 1000 && config.requireConfigured === true &&
    config.storageDiagnosticsEnabled === true && result.provider === PROVIDER && result.layout === config.layout &&
    result.workload === "lifecycle" && result.sizeMiB === 1 && result.fileSizeBytes === 4096 &&
    result.writePayloadBytes === 4096 && result.iterationsRequested === 400 && result.concurrency === 64 && result.chunkSizeBytes === 65536
}

function cleanSummary(summary) {
  if (!object(summary) || !(summary.elapsedMs > 0) || !Number.isFinite(summary.elapsedMs) ||
      !Number.isFinite(summary.iops) || summary.iops < 0 || summary.iopsTarget !== 1000 ||
      summary.iopsTargetMet !== (summary.iops >= 1000)) return false
  return summary.successfulIterations === 400 && summary.failedIterations === 0 &&
    summary.operationsPerLifecycle === 3 && summary.successfulOperations === 1200 && summary.attemptedOperations === 1200 &&
    summary.timeoutCount === 0 && summary.cleanupFailureCount === 0 &&
    ["write", "read", "delete", "verifiedReads"].every((key) => summary.operationSuccess?.[key] === 400) &&
    Math.abs(summary.iops - 1200000 / summary.elapsedMs) <= 1e-9 * Math.max(1, summary.iops)
}

function cleanProvider(provider, benchmark, result) {
  const cleanup = provider.cleanup
  return provider.provider === PROVIDER && Array.isArray(provider.sizes) && provider.sizes.length === 1 &&
    isDeepStrictEqual(provider.sizes[0], result) && empty(benchmark.configurationFailures) && empty(provider.missingConfiguration) &&
    ["setupError", "skipReason", "layoutInspectionError", "layoutInspectionLateOperation", "revisionFailure", "oracleRevisionMismatch"].every((key) => !Object.hasOwn(provider, key)) &&
    object(cleanup) && cleanup.pathsAttempted === 0 && cleanup.remainingPaths === 0 && empty(cleanup.failures) &&
    empty(cleanup.pendingOperations) && object(cleanup.resource) && cleanup.resource.status === "ok" &&
    ["error", "lateOperation", "pendingOperations", "cleanupCompleted"].every((key) => !Object.hasOwn(cleanup.resource, key))
}

function originalStatusConsistent(benchmark, provider, result, belowFloor) {
  const expected = belowFloor ? "failed" : "ok", counts = benchmark.counts
  return benchmark.status === expected && provider.status === expected && result.status === expected &&
    object(counts) && counts.providersRequested === 1 && counts.providersFailed === (belowFloor ? 1 : 0) &&
    counts.providersSkipped === 0 && counts.configurationFailures === 0 && counts.sizeResults === 1 &&
    counts.sizeResultsFailed === (belowFloor ? 1 : 0) && counts.sizeResultsSkipped === 0
}

function soleFloorFailure(result) {
  const entries = failures(result), summary = result.summary
  return Array.isArray(entries) && entries.length === 1 && entries[0]?.operation === "iops-target" &&
    entries[0].error?.code === "IOPS_TARGET_NOT_MET" && entries[0].error.actual === summary.iops && entries[0].error.target === 1000
}

function safeTerminal(terminal, originalStatus) {
  return object(terminal) && terminal.status === originalStatus && terminal.native_quiescent === true &&
    terminal.owned_operations_settled === true && terminal.native_profiling_enabled === true &&
    terminal.workload_native_evidence_complete === true && terminal.cleanup_complete === true &&
    terminal.operation_deadline_failed === false && terminal.prior_native_uncertainty === false &&
    terminal.safe_to_continue_pair === true && terminal.quiescence_scope === "runner_workload_native_diagnostics_and_owned_operation_settlement"
}

function observedNative(provider, elapsed, projection) {
  const diagnostics = provider.storageDiagnostics
  if (!object(diagnostics) || !Array.isArray(diagnostics.phases)) return false
  const workloads = diagnostics.phases.filter((phase) => typeof phase?.name === "string" && phase.name.startsWith("workload-"))
  if (workloads.length !== 1 || workloads[0].name !== WORKLOAD) return false
  const phase = workloads[0]
  try { validateRawPhaseDiagnostics(phase, "rustfs"); projection.raw_workload_observed = true }
  catch { return false }
  return diagnostics.enabled === true && phase.benchmark_measured_elapsed_ms === elapsed &&
    Number.isFinite(phase.elapsed_ms) && phase.elapsed_ms >= elapsed && Number.isFinite(phase.observer_snapshot_ms) && phase.observer_snapshot_ms >= 0
}

function observedLayout(provider, layout, projection) {
  const selection = provider.layoutSelection
  if (layout === "legacy" && selection === undefined) return true
  if (!object(selection) || selection.requested !== layout || selection.selected !== layout ||
      selection.selectionEvidence !== "createChunkedDriver-constructor-accepted") return false
  if (layout === "legacy") return [undefined, "not-observed-by-benchmark-runner"].includes(selection.persistedMarkerEvidence) && selection.persistedReceipt == null
  if (layout !== "compact" || selection.persistedMarkerEvidence !== "metadata-MRC5-and-matching-block-authority-observed") return false
  try { validateCompactLayoutReceipt(selection.persistedReceipt); projection.original_layout_proof_observed = true; return true }
  catch { return false }
}

function observedCounter(value) {
  return typeof value === "string" && value.length <= 20 && /^(?:0|[1-9]\d*)$/u.test(value) && BigInt(value) <= 18446744073709551615n
}
function observedDeviceKey(value) {
  const parts = /^(0|[1-9][0-9]{0,19}):(0|[1-9][0-9]{0,19}):(Read|Write)$/u.exec(value)
  return parts !== null && observedCounter(parts[1]) && observedCounter(parts[2])
}
function observedCounterMap(value, network) {
  if (value === null) return true
  if (!object(value)) return false
  const entries = Object.entries(value)
  return entries.length > 0 && entries.length <= (network ? 32 : 128) && entries.every(([key, counter]) =>
    (network ? /^[A-Za-z][A-Za-z0-9_.-]{0,14}$/u.test(key) && !["constructor", "prototype"].includes(key)
      : observedDeviceKey(key)) &&
    (network && counter === null || observedCounter(counter)))
}
function observedBoundaryCounters(boundary) {
  return Array.isArray(boundary.samples) && boundary.samples.length === 8 && boundary.samples.every((sample) =>
    object(sample) && object(sample.stats) && sample.cid === sample.stats.cid && observedCounter(sample.stats.read_ns) &&
    (sample.stats.cpu_usage_ns === null || observedCounter(sample.stats.cpu_usage_ns)) &&
    ["block_bytes", "block_operations", "network_rx_bytes", "network_tx_bytes"].every((key) => observedCounterMap(sample.stats[key], key.startsWith("network_"))))
}
function observedCounterPairs(before, after) {
  const ends = new Map(after.samples.map((sample) => [sample.cid, sample.stats]))
  const monotonic = (first, last) => first === null || last === null || BigInt(last) >= BigInt(first)
  return before.samples.every((sample) => {
    const first = sample.stats, last = ends.get(sample.cid)
    if (!last || BigInt(last.read_ns) <= BigInt(first.read_ns) || !monotonic(first.cpu_usage_ns, last.cpu_usage_ns)) return false
    return ["block_bytes", "block_operations", "network_rx_bytes", "network_tx_bytes"].every((key) => {
      const start = first[key], end = last[key]
      if (start === null || end === null) return true
      const keys = Object.keys(start).sort(), endKeys = Object.keys(end).sort()
      return isDeepStrictEqual(keys, endKeys) && keys.every((item) => monotonic(start[item], end[item]))
    })
  })
}

function observedBacking(provider, elapsed) {
  const observer = provider.backingObserver, evidence = observer?.backing_evidence
  if (observer?.schema !== "mount-rs.runner-backing-observer.v1" || observer.complete !== true || !empty(observer.issues) ||
      evidence?.schema !== "mount-rs.backing-observer.v1" || evidence.complete !== true || !empty(evidence.issues) || evidence.dropped_entries !== 0 ||
      !safeTerminal(observer.terminal, provider.status) || !safeTerminal(evidence.terminal, provider.status)) return false
  const coverage = provider.backingResourceCoverage
  if (coverage?.[WORKLOAD] !== "captured" || !["create", "cleanup", "shutdown"].every((key) => coverage[key] === "not_selected")) return false
  const events = observer.events
  if (!Array.isArray(events) || events.length !== 3 ||
      !["beginPhase", "endPhase", "finalize"].every((hook, index) => events[index]?.hook === hook && events[index].status === "ok") ||
      events[0].phase !== WORKLOAD || events[1].phase !== WORKLOAD || events[1].owned_operations_settled !== true ||
      events[1].native_quiescent !== true || events[1].native_evidence_state !== "complete" || events[1].pending_count !== 0) return false
  const cost = evidence.cost
  if (cost?.in_flight !== 0 || !["headers", "body", "retirement"].every((stage) => cost.pending_stages?.[stage]?.count === 0 && cost.pending_stages[stage].age_ms === 0)) return false
  const journal = evidence.journal
  if (!Array.isArray(journal) || journal.length > 4 || !journal.every((entry) => object(entry) && ["version", "boundary", "interval"].includes(entry.type))) return false
  const boundaries = journal.filter((entry) => entry.type === "boundary"), intervals = journal.filter((entry) => entry.type === "interval")
  if (boundaries.length !== 2 || intervals.length !== 1 || boundaries[0].id !== `${WORKLOAD}:begin` || boundaries[1].id !== `${WORKLOAD}:end` ||
      !boundaries.every((boundary) => boundary.kind === "phase" && boundary.complete === true && empty(boundary.issues) && observedBoundaryCounters(boundary))) return false
  if (!observedCounterPairs(boundaries[0], boundaries[1])) return false
  const interval = intervals[0]
  if (interval.schema !== "mount-rs.backing-interval.v1" || interval.kind !== "phase" || typeof interval.complete !== "boolean" ||
      interval.native_quiescent !== true || interval.owned_operations_settled !== true || interval.native_evidence_state !== "complete" || interval.workload_elapsed_ms !== elapsed) return false
  try {
    const computed = summarizeInterval(evidence.allowlist, boundaries[0], boundaries[1], { workload_elapsed_ms: elapsed })
    // Missing daemon metrics affect descriptive coverage. Resets, drift, lost
    // members and invalid timestamps remain failures rather than coverage gaps.
    return interval.complete === computed.complete && computed.containers.every((container) =>
      Object.values(container.metrics).every((metric) => metric.complete === true || metric.issue === "metric_unavailable")) &&
      ["containers", "metrics", "endpoints"].every((key) => isDeepStrictEqual(interval[key], computed[key]))
  } catch { return false }
}

/** Assess the original runner evidence; arm persistence and identity remain separate gates. */
export function assessOwnedLayoutRunnerOutcome(benchmark) {
  const projection = freshProjection(), issues = new Set()
  try {
    benchmark = snapshotBenchmark(benchmark)
    projection.status = status(benchmark?.status)
    const provider = Array.isArray(benchmark?.providers) && benchmark.providers.length === 1 ? benchmark.providers[0] : null
    const result = Array.isArray(benchmark?.results) && benchmark.results.length === 1 ? benchmark.results[0] : null
    projection.provider_status = status(provider?.status)
    projection.result_status = status(result?.status)
    projection.observer_status = status(provider?.backingObserver?.terminal?.status)
    projection.backing_status = status(provider?.backingObserver?.backing_evidence?.terminal?.status)
    if (!object(benchmark) || !object(provider) || !object(result)) throw new Error("invalid_shape")
    const summary = result.summary, samples = result.rawSamples, cleanup = provider.cleanup, terminal = provider.backingObserver?.terminal
    projection.iops = finite(summary?.iops)
    projection.elapsed_ms = finite(summary?.elapsedMs)
    projection.iops_target = count(summary?.iopsTarget)
    projection.iops_target_met = boolean(summary?.iopsTargetMet)
    for (const [target, value] of Object.entries({
      successful_iterations: summary?.successfulIterations, failed_iterations: summary?.failedIterations,
      successful_operations: summary?.successfulOperations, attempted_operations: summary?.attemptedOperations,
      verified_reads: summary?.operationSuccess?.verifiedReads, timeout_count: summary?.timeoutCount,
      cleanup_failure_count: summary?.cleanupFailureCount, remaining_paths: cleanup?.remainingPaths,
    })) projection[target] = count(value)
    projection.raw_sample_count = Array.isArray(samples) ? samples.length : null
    const sampleIndices = Array.isArray(samples) && samples.length <= 400 ? Array.from({ length: samples.length }, (_, index) => index) : null
    projection.sample_error_count = sampleIndices && sampleIndices.every((index) => Object.hasOwn(samples, index) && Array.isArray(samples[index]?.errors))
      ? count(sampleIndices.reduce((sum, index) => sum + samples[index].errors.length, 0)) : null
    projection.result_failure_count = Array.isArray(failures(result)) ? failures(result).length : null
    projection.path_cleanup_failure_count = Array.isArray(cleanup?.failures) ? cleanup.failures.length : null
    projection.pending_operation_count = Array.isArray(cleanup?.pendingOperations) ? cleanup.pendingOperations.length : null
    projection.resource_cleanup_status = ["ok", "failed", "pending", "deferred"].includes(cleanup?.resource?.status) ? cleanup.resource.status : "unavailable"
    projection.original_safe_to_continue_pair = boolean(terminal?.safe_to_continue_pair)
    projection.native_quiescent = boolean(terminal?.native_quiescent)
    projection.owned_operations_settled = boolean(terminal?.owned_operations_settled)
    if (!fixedConfig(benchmark, result)) issues.add("workload_contract_invalid")
    if (!cleanSummary(summary)) issues.add("logical_counts_invalid")
    if (!sampleIndices || sampleIndices.length !== 400 || !sampleIndices.every((index) => Object.hasOwn(samples, index) && cleanSample(samples[index], index))) issues.add("sample_evidence_invalid")
    if (!cleanProvider(provider, benchmark, result)) issues.add("provider_or_cleanup_invalid")
    const belowFloor = summary?.iopsTargetMet === false
    if (!originalStatusConsistent(benchmark, provider, result, belowFloor)) issues.add("original_status_invalid")
    if (belowFloor ? !soleFloorFailure(result) : !empty(failures(result))) issues.add("original_failure_set_invalid")
    const logicalComplete = issues.size === 0
    projection.floor_qualified = logicalComplete && !belowFloor
    projection.sole_floor_failure = logicalComplete && belowFloor
    if (!observedLayout(provider, benchmark.config?.layout, projection)) issues.add("original_layout_evidence_invalid")
    if (!observedNative(provider, summary?.elapsedMs, projection)) issues.add("rustfs_workload_evidence_invalid")
    if (!observedBacking(provider, summary?.elapsedMs)) issues.add("original_observer_evidence_invalid")
    projection.validated_runner_outcome = issues.size === 0
    projection.runner_safe_to_continue = projection.validated_runner_outcome
    if (projection.sole_floor_failure) projection.failure_codes.push("iops_target_not_met")
  } catch {
    projection.floor_qualified = false
    projection.sole_floor_failure = false
    projection.validated_runner_outcome = false
    projection.runner_safe_to_continue = false
    issues.add("runner_shape_invalid")
  }
  projection.failure_codes.push(...issues)
  Object.freeze(projection.failure_codes)
  return Object.freeze(projection)
}
