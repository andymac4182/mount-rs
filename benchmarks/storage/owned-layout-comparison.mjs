import { Buffer } from "node:buffer"
import { createRequire } from "node:module"
import { isDeepStrictEqual } from "node:util"
import { createBackingEngineTransport } from "./backing-engine-transport.mjs"
import { createBackingObserver, projectInspect, summarizeInterval } from "./backing-observer.mjs"
import { validateCompactLayoutReceipt } from "./compact-layout.mjs"
import { validateSplitNamespacePresenceReceipt } from "./namespace-presence.mjs"
import { createOwnedLayoutArm } from "./owned-layout-arm.mjs"
import { assessOwnedLayoutRunnerOutcome } from "./owned-layout-outcome.mjs"
import { projectOwnedLayoutPhaseMetrics } from "./owned-layout-metrics.mjs"
import { observePilotBinding } from "./owned-backing-pilot.mjs"

// Four original 8 MiB observer journals, with small closed outcome envelopes.
// Required receipts are never truncated to make publication appear complete.
export const OUTPUT_CAP = 33_554_432
export const WHOLE_CALL_CEILINGS_MS = Object.freeze({ prepare: 245_000, runner: 60_000, persistence: 310_000 })
const PROVIDER = "mount-rs-split-tidb-rustfs"
const ORDER = Object.freeze(["A1", "B1", "B2", "A2"])
const LAYOUTS = Object.freeze(["legacy", "compact", "compact", "legacy"])
const ROLES = Object.freeze(["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"])
const METRICS = Object.freeze(["cpu_usage_ns", "block_bytes", "block_operations", "network_rx_bytes", "network_tx_bytes"])
const CAPS = Object.freeze({ maxContainers: 16, maxRequests: 257, maxBoundaries: 64, maxResponseBytes: 1_048_576,
  maxDeviceEntries: 128, maxNetworkInterfaces: 32, maxJournalBytes: 8_388_608, requestTimeoutMs: 2000, ownerTimeoutMs: 60_000 })
const OBSERVER_RETENTION = "selected_projections_and_raw_version_inspect_or_first_stats_frame_sha256; consumed_trailing_bytes_counted_discarded; raw_bodies_discarded"
const COST_SCOPES = Object.freeze({ response_bytes_scope: "adapter_yielded_bytes_including_consumed_same_chunk_tail; rejected_overflow_chunks_unavailable; not_wire_bytes",
  wall_scope: "inclusive_request_windows; concurrent_windows_overlap; not_exclusive_hook_wall", cpu_scope: "inclusive_process_cpu_during_observer_calls; overlapping_work_not_isolated" })
const COST_COUNTS = Object.freeze(["requests", "response_bytes", "cpu_user_us", "cpu_system_us", "peak_in_flight", "headers_received", "first_frames_received", "streamed_iterators_retired", "consumed_trailing_bytes", "in_flight"])
const ARM_CODES = new Set(["CONFIG_INVALID", "API_UNAVAILABLE", "STATE_INVALID", "PREFLIGHT_FAILED", "CREATE_FAILED", "REOPEN_FAILED",
  "REOPEN_NOT_FRESH", "LAYOUT_PROOF_FAILED", "GENERATION_CONTINUITY_FAILED", "CANARY_WRITE_FAILED", "CANARY_READ_FAILED", "CANARY_EOF_FAILED",
  "CANARY_REMOVE_FAILED", "EMPTY_PROOF_FAILED", "HANDLE_CLOSE_FAILED", "SHUTDOWN_FAILED"].map((code) => `OWNED_LAYOUT_ARM_${code}`))
const COUNT_FIELDS = Object.freeze(["preflight_calls", "initial_create_calls", "reopen_create_calls", "original_shutdown_calls", "reopen_shutdown_calls",
  "canary_write_calls", "canary_bytes_written", "canary_sync_calls", "canary_write_close_calls", "canary_read_calls", "canary_bytes_read",
  "canary_eof_read_calls", "canary_eof_bytes_read", "canary_read_close_calls", "canary_remove_calls", "initial_empty_calls", "final_empty_calls", "layout_inspection_calls"])
const issued = new WeakSet()
const require = createRequire(import.meta.url)
const problem = (code) => { const error = Object.assign(new Error(code), { code }); issued.add(error); return error }
const reject = (code = "COORDINATOR_CONFIG_INVALID") => { throw problem(code) }
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value)
const count = (value) => Number.isSafeInteger(value) && value >= 0
const finite = (value) => Number.isFinite(value) && value >= 0
const decimal = (value) => typeof value === "string" && /^(?:0|[1-9][0-9]{0,19})$/u.exec(value)?.[0] === value && BigInt(value) <= 18_446_744_073_709_551_615n
const scopeText = (value, maximum) => typeof value === "string" && Buffer.byteLength(value) <= maximum &&
  /^[A-Za-z0-9_.\/-]+$/u.exec(value)?.[0] === value && !value.split("/").some((part) => ["", ".", ".."].includes(part))
const privateText = (value) => typeof value === "string" && value.length > 0 && !value.includes("\0") && Buffer.byteLength(value) <= 4096
const hash = (value) => typeof value === "string" && /^[0-9a-f]{64}$/u.exec(value)?.[0] === value
const frozen = (value) => {
  if (value && typeof value === "object" && !Object.isFrozen(value)) { for (const child of Object.values(value)) frozen(child); Object.freeze(value) }
  return value
}
function record(source, keys, code = "COORDINATOR_CONFIG_INVALID") {
  if (!object(source)) reject(code)
  const descriptors = Object.getOwnPropertyDescriptors(source)
  if (Reflect.ownKeys(descriptors).length !== keys.length || !keys.every((key) => Object.hasOwn(descriptors, key) && Object.hasOwn(descriptors[key], "value"))) reject(code)
  return Object.fromEntries(keys.map((key) => [key, descriptors[key].value]))
}
// Snapshot data without invoking accessors, toJSON, custom iterators or prototypes.
// Bound both allocation and serialized input; the original private records stay private.
function data(source) {
  let nodes = 0, bytes = 0
  const seen = new Map(), visiting = new Set()
  function copy(value, depth) {
    if (++nodes > 500_000 || depth > 40) reject("COORDINATOR_RUNNER_OUTCOME_INVALID")
    if (value === null || value === undefined || typeof value === "boolean") return value
    if (typeof value === "number") { if (!Number.isFinite(value)) reject("COORDINATOR_RUNNER_OUTCOME_INVALID"); return value }
    if (typeof value === "string") { bytes += Buffer.byteLength(value); if (bytes > OUTPUT_CAP) reject("COORDINATOR_PUBLICATION_INCOMPLETE"); return value }
    if (typeof value !== "object" || visiting.has(value)) reject("COORDINATOR_RUNNER_OUTCOME_INVALID")
    if (seen.has(value)) return seen.get(value)
    const array = Array.isArray(value), proto = Object.getPrototypeOf(value)
    if (proto !== (array ? Array.prototype : Object.prototype) && proto !== null) reject("COORDINATOR_RUNNER_OUTCOME_INVALID")
    const descriptors = Object.getOwnPropertyDescriptors(value), result = array ? [] : Object.create(null)
    seen.set(value, result); visiting.add(value)
    const keys = Reflect.ownKeys(descriptors)
    if (array && (!count(value.length) || value.length > 100_000 || keys.length !== value.length + 1)) reject("COORDINATOR_RUNNER_OUTCOME_INVALID")
    for (const key of keys) {
      if (array && key === "length") continue
      if (typeof key !== "string" || !Object.hasOwn(descriptors[key], "value") || array && !/^(?:0|[1-9][0-9]*)$/u.test(key)) reject("COORDINATOR_RUNNER_OUTCOME_INVALID")
      bytes += Buffer.byteLength(key) + 8
      if (bytes > OUTPUT_CAP) reject("COORDINATOR_PUBLICATION_INCOMPLETE")
      Object.defineProperty(result, key, { value: copy(descriptors[key].value, depth + 1), enumerable: true, writable: true, configurable: true })
    }
    visiting.delete(value)
    return result
  }
  return copy(source, 0)
}
function fixturesFrom(source, environment) {
  const receipt = record(data(source), ["schema", "tidb_owner", "rustfs_owner", "generation", "entries"])
  if (receipt.schema !== "mount-rs.owned-backing-cids.v1" || receipt.generation !== "1" || receipt.generation !== environment.MOUNT_RS_BACKING_GENERATION ||
      receipt.tidb_owner !== environment.MOUNT_RS_BACKING_TIDB_OWNER || receipt.rustfs_owner !== environment.MOUNT_RS_BACKING_RUSTFS_OWNER ||
      ![receipt.tidb_owner, receipt.rustfs_owner].every((value) => typeof value === "string" && /^[A-Za-z0-9_.:-]{1,120}$/u.exec(value)?.[0] === value) ||
      !Array.isArray(receipt.entries) || receipt.entries.length !== 8) reject()
  const cids = new Set()
  const entries = ROLES.map((role) => {
    const matches = receipt.entries.filter((entry) => entry?.role === role)
    if (matches.length !== 1) reject()
    const entry = record(matches[0], ["role", "cid", "labels"])
    if (!hash(entry.cid) || cids.has(entry.cid)) reject()
    cids.add(entry.cid)
    const labels = role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": receipt.rustfs_owner }
      : { "mount-rs.tidb.run": receipt.tidb_owner }
    const actual = record(entry.labels, Object.keys(labels))
    if (Object.entries(labels).some(([key, value]) => actual[key] !== value)) reject()
    return { role, cid: entry.cid, labels }
  })
  return frozen({ ...receipt, entries })
}
function observation(value, layout, flags) {
  if (value === null) return null
  if (layout === "compact") {
    const source = record(value, ["kind", "receipt"], "COORDINATOR_ARM_STATE_INVALID")
    if (source.kind !== "MRC5") reject("COORDINATOR_ARM_STATE_INVALID")
    const receipt = validateCompactLayoutReceipt(source.receipt)
    if (!decimal(receipt.structuralGeneration) || /^[0-9a-f]{32}$/u.exec(receipt.backingId)?.[0] !== receipt.backingId) reject("COORDINATOR_ARM_STATE_INVALID")
    return { kind: "MRC5", receipt }
  }
  const source = record(value, ["kind", "exact_legacy_marker", "fresh_preflight_required", "constructor_flags"], "COORDINATOR_ARM_STATE_INVALID")
  if (source.kind !== "recognized_non_mrc5" || source.exact_legacy_marker !== false || source.fresh_preflight_required !== true ||
      JSON.stringify(source.constructor_flags) !== JSON.stringify(flags)) reject("COORDINATOR_ARM_STATE_INVALID")
  return { kind: "recognized_non_mrc5", exact_legacy_marker: false, fresh_preflight_required: true, constructor_flags: flags }
}
function armProjection(arm, cohort, metadataDurable, blocksDurable) {
  try {
    const source = record(data(arm.snapshot()), ["schema", "layout", "phase", "complete", "construction", "preflight", "original_shutdown_confirmed",
      "reopen_shutdown_confirmed_count", "layout_observations", "canary", "counts", "counter_scope", "failure_code", "cleanup_codes"], "COORDINATOR_ARM_STATE_INVALID")
    const flags = { concurrentWrites: cohort.layout === "compact", inodeUpdates: cohort.layout === "compact", compactInodeUpdates: cohort.layout === "compact" }
    const construction = record(source.construction, ["metadata_provider", "block_provider", "chunk_size_bytes", "metadata_durable", "blocks_durable", "flags"], "COORDINATOR_ARM_STATE_INVALID")
    const actualFlags = record(construction.flags, Object.keys(flags), "COORDINATOR_ARM_STATE_INVALID")
    if (source.schema !== "mount-rs.owned-layout-arm.v1" || source.layout !== cohort.layout || !["idle", "preparing", "prepared", "handed_off", "closing_original", "original_closed", "verifying", "verified", "failed"].includes(source.phase) ||
        typeof source.complete !== "boolean" || source.complete !== (source.phase === "verified") || construction.metadata_provider !== "tidb" || construction.block_provider !== "rustfs" ||
        construction.chunk_size_bytes !== 65536 || construction.metadata_durable !== metadataDurable || construction.blocks_durable !== blocksDurable ||
        Object.keys(flags).some((key) => actualFlags[key] !== flags[key]) || typeof source.original_shutdown_confirmed !== "boolean" || !count(source.reopen_shutdown_confirmed_count) ||
        source.failure_code !== null && !ARM_CODES.has(source.failure_code) || !Array.isArray(source.cleanup_codes) || !source.cleanup_codes.every((code) => ARM_CODES.has(code))) reject("COORDINATOR_ARM_STATE_INVALID")
    const counts = record(source.counts, COUNT_FIELDS, "COORDINATOR_ARM_STATE_INVALID")
    if (!COUNT_FIELDS.every((key) => count(counts[key]) || key === "canary_eof_bytes_read" && counts[key] === null)) reject("COORDINATOR_ARM_STATE_INVALID")
    const canary = record(source.canary, ["payload_bytes", "created", "full_bytes_verified", "eof_verified", "remove_confirmed", "logical_root_empty"], "COORDINATOR_ARM_STATE_INVALID")
    if (canary.payload_bytes !== 4096 || !Object.keys(canary).filter((key) => key !== "payload_bytes").every((key) => typeof canary[key] === "boolean")) reject("COORDINATOR_ARM_STATE_INVALID")
    const observations = record(source.layout_observations, ["before_timed", "drained_original", "reopened", "removed_drained", "empty_reopened"], "COORDINATOR_ARM_STATE_INVALID")
    const layout_observations = Object.fromEntries(Object.entries(observations).map(([key, value]) => [key, observation(value, cohort.layout, flags)]))
    const preflight = source.preflight === null ? null : validateSplitNamespacePresenceReceipt(JSON.stringify(source.preflight))
    return frozen({ schema: source.schema, available: true, layout: source.layout, phase: source.phase, complete: source.complete,
      construction: { ...construction, flags }, preflight, original_shutdown_confirmed: source.original_shutdown_confirmed,
      reopen_shutdown_confirmed_count: source.reopen_shutdown_confirmed_count, layout_observations, canary, counts,
      counter_scope: "untimed_helper_invocations_and_public_api_dispatches; not_backend_or_timed_workload_proof",
      failure_code: source.failure_code, cleanup_codes: source.cleanup_codes })
  } catch { return frozen({ schema: "mount-rs.owned-layout-arm.v1", available: false }) }
}
function continuity(next, before, exact = false) {
  if (!next || !before) return false
  if (next.kind !== "MRC5") return next.kind === "recognized_non_mrc5" && before.kind === next.kind
  return before.kind === "MRC5" && next.receipt.backingId === before.receipt.backingId &&
    (exact ? next.receipt.structuralGeneration === before.receipt.structuralGeneration : BigInt(next.receipt.structuralGeneration) >= BigInt(before.receipt.structuralGeneration))
}
function armProof(state, stage) {
  if (state.available !== true || state.failure_code !== null || state.cleanup_codes.length !== 0 || !state.preflight || !state.canary.created) return false
  const expected = { preflight_calls: 1, initial_create_calls: 1, canary_write_calls: 1, canary_bytes_written: 4096, canary_sync_calls: 1,
    canary_write_close_calls: 1, initial_empty_calls: 1, reopen_create_calls: 0, original_shutdown_calls: 0, reopen_shutdown_calls: 0,
    canary_read_calls: 0, canary_bytes_read: 0, canary_eof_read_calls: 0, canary_eof_bytes_read: null, canary_read_close_calls: 0,
    canary_remove_calls: 0, final_empty_calls: 0, layout_inspection_calls: 1 }
  if (stage !== "prepared") Object.assign(expected, { original_shutdown_calls: 1, layout_inspection_calls: 2 })
  if (stage === "verified") Object.assign(expected, { reopen_create_calls: 2, reopen_shutdown_calls: 2, canary_read_calls: 1, canary_bytes_read: 4096,
    canary_eof_read_calls: 1, canary_eof_bytes_read: 0, canary_read_close_calls: 1, canary_remove_calls: 1, final_empty_calls: 1, layout_inspection_calls: 5 })
  const observations = state.layout_observations
  return state.phase === stage && Object.entries(expected).every(([key, value]) => state.counts[key] === value) && observations.before_timed !== null &&
    state.original_shutdown_confirmed === (stage !== "prepared") && state.reopen_shutdown_confirmed_count === (stage === "verified" ? 2 : 0) &&
    (stage === "prepared" ? observations.drained_original === null : continuity(observations.drained_original, observations.before_timed)) &&
    (stage === "verified" ? continuity(observations.reopened, observations.drained_original, true) && continuity(observations.removed_drained, observations.reopened) &&
      continuity(observations.empty_reopened, observations.removed_drained, true) && state.canary.full_bytes_verified && state.canary.eof_verified && state.canary.remove_confirmed && state.canary.logical_root_empty
      : [observations.reopened, observations.removed_drained, observations.empty_reopened].every((value) => value === null) &&
        !state.canary.full_bytes_verified && !state.canary.eof_verified && !state.canary.remove_confirmed && !state.canary.logical_root_empty)
}
function identityProjection(source, entry) {
  const actual = record(source, ["cid", "role", "labels", "image", "running", "started_at", "restart_count", "limits", "configured_limit_semantics"], "COORDINATOR_FIXTURE_IDENTITY_INVALID")
  record(actual.limits, ["Memory", "MemorySwap", "NanoCpus", "CpuQuota", "CpuPeriod", "CpuShares", "PidsLimit", "CpusetCpus"], "COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const labels = record(actual.labels, Object.keys(entry.labels), "COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const projected = projectInspect({ Id: actual.cid, Image: actual.image, State: { Running: actual.running, StartedAt: actual.started_at },
    RestartCount: actual.restart_count, HostConfig: actual.limits, Config: { Labels: labels } }, entry)
  if (actual.role !== entry.role || actual.configured_limit_semantics !== projected.configured_limit_semantics ||
      Object.keys(projected.limits).some((key) => actual.limits[key] !== projected.limits[key]) ||
      Object.entries(entry.labels).some(([key, value]) => labels[key] !== value)) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  return projected
}
function terminal(source) {
  const fields = ["native_quiescent", "owned_operations_settled", "native_profiling_enabled", "workload_native_evidence_complete", "operation_deadline_failed", "cleanup_complete", "prior_native_uncertainty", "safe_to_continue_pair"]
  return { status: ["ok", "failed", "skipped"].includes(source?.status) ? source.status : "unavailable",
    ...Object.fromEntries(fields.map((key) => [key, typeof source?.[key] === "boolean" ? source[key] : null])),
    quiescence_scope: source?.quiescence_scope === "runner_workload_native_diagnostics_and_owned_operation_settlement" ? source.quiescence_scope : "unavailable" }
}
function windowProjection(source) {
  const result = {}
  for (const key of ["dispatch_ms", "headers_ms", "first_frame_ms", "retired_ms", "response_ms"]) result[key] = finite(source?.[key]) ? source[key] : null
  return result
}
function counterMap(source, network) {
  if (source === null) return null
  if (!object(source)) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const entries = Object.entries(source)
  if (entries.length > (network ? 32 : 128) || entries.length === 0) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  for (const [key, value] of entries) {
    const device = /^(0|[1-9][0-9]{0,19}):(0|[1-9][0-9]{0,19}):(Read|Write)$/u.exec(key)
    const validKey = network ? /^[A-Za-z][A-Za-z0-9_.-]{0,14}$/u.exec(key)?.[0] === key && !["constructor", "prototype"].includes(key)
      : device?.[0] === key && decimal(device[1]) && decimal(device[2])
    if (!validKey || !(network && value === null || decimal(value))) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  }
  return Object.fromEntries(entries.sort(([left], [right]) => left.localeCompare(right)))
}
function monotoneCounters(before, after) {
  const monotone = (first, last) => first === null || last === null || BigInt(last) >= BigInt(first)
  return before.samples.every((sample, index) => {
    const first = sample.stats, last = after.samples[index].stats
    if (BigInt(last.read_ns) <= BigInt(first.read_ns) || !monotone(first.cpu_usage_ns, last.cpu_usage_ns)) return false
    return METRICS.slice(1).every((metric) => {
      const start = first[metric], end = last[metric]
      if (start === null || end === null) return true
      const keys = Object.keys(start).sort()
      return isDeepStrictEqual(keys, Object.keys(end).sort()) && keys.every((key) => monotone(start[key], end[key]))
    })
  })
}
function observerProjection(source, entries, elapsed) {
  const backing = source?.backing_evidence
  const caps = record(backing?.caps, Object.keys(CAPS), "COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const cost = record(backing?.cost, [...COST_COUNTS, "wall_ms", "owner_lifetime_ms", "pending_stages", ...Object.keys(COST_SCOPES)], "COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const versions = backing?.journal?.filter((entry) => entry.type === "version")
  if (Object.entries(CAPS).some(([key, value]) => caps[key] !== value) || backing.retention !== OBSERVER_RETENTION || backing.daemon_overhead !== "unisolated" ||
      !Array.isArray(versions) || versions.length !== 1 || !/^1\.(?:4[1-9]|5[01])$/u.test(backing.api_version) || versions[0].api_version !== backing.api_version ||
      versions[0].mode !== "stream=true;first-frame" || versions[0].platform !== "linux" || !hash(versions[0].body_sha256) ||
      !count(backing.journal_bytes) || backing.journal_bytes < 1 || backing.journal_bytes > CAPS.maxJournalBytes ||
      backing.journal_bytes !== backing.journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0) ||
      !COST_COUNTS.every((key) => count(cost[key])) || !finite(cost.wall_ms) || !finite(cost.owner_lifetime_ms) ||
      cost.owner_lifetime_ms > CAPS.ownerTimeoutMs || cost.owner_lifetime_ms < elapsed || cost.wall_ms > 33 * CAPS.requestTimeoutMs ||
      cost.requests !== 33 || cost.headers_received !== 33 || cost.first_frames_received !== 16 || cost.streamed_iterators_retired !== 16 ||
      cost.peak_in_flight < 1 || cost.peak_in_flight > 8 || cost.in_flight !== 0 || cost.response_bytes < 1 || cost.response_bytes > cost.requests * CAPS.maxResponseBytes ||
      cost.consumed_trailing_bytes > cost.response_bytes || Object.entries(COST_SCOPES).some(([key, value]) => cost[key] !== value)) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const pendingStages = record(cost.pending_stages, ["headers", "body", "retirement"], "COORDINATOR_FIXTURE_IDENTITY_INVALID")
  for (const stage of Object.values(pendingStages)) {
    const state = record(stage, ["count", "age_ms"], "COORDINATOR_FIXTURE_IDENTITY_INVALID")
    if (state.count !== 0 || state.age_ms !== 0) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  }
  if (!Array.isArray(backing?.allowlist) || backing.allowlist.length !== 8 || entries.some((entry) => {
    const matches = backing.allowlist.filter((actual) => actual.cid === entry.cid)
    if (matches.length !== 1) return true
    const actual = record(matches[0], ["cid", "role", "labels"], "COORDINATOR_FIXTURE_IDENTITY_INVALID")
    const labels = record(actual.labels, Object.keys(entry.labels), "COORDINATOR_FIXTURE_IDENTITY_INVALID")
    return actual.role !== entry.role || Object.entries(entry.labels).some(([key, value]) => labels[key] !== value)
  })) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const boundaries = backing?.journal?.filter((entry) => entry.type === "boundary")
  if (!Array.isArray(boundaries) || boundaries.length !== 2 || boundaries.some((entry, index) => entry.id !== `workload-4096bytes:${index ? "end" : "begin"}` || entry.kind !== "phase" || !Array.isArray(entry.samples) || entry.samples.length !== 8)) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const projected = boundaries.map((boundary) => ({ type: "boundary", id: boundary.id, kind: "phase", samples: entries.map((entry) => {
    const matches = boundary.samples.filter((sample) => sample.cid === entry.cid)
    if (matches.length !== 1) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
    const sample = matches[0], stats = sample.stats
    if (stats?.cid !== entry.cid || !decimal(stats.read_ns) || !(stats.cpu_usage_ns === null || decimal(stats.cpu_usage_ns))) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
    return { cid: entry.cid, identity: identityProjection(sample.identity, entry), stats: { cid: entry.cid, read_ns: stats.read_ns, cpu_usage_ns: stats.cpu_usage_ns,
      ...Object.fromEntries(["cpu_user_ns", "cpu_kernel_ns", "system_cpu_usage_ns", "online_cpus"].map((key) => [key, decimal(stats[key]) ? stats[key] : null])),
      throttling: Object.fromEntries(["periods", "throttled_periods", "throttled_ns"].map((key) => [key, decimal(stats.throttling?.[key]) ? stats.throttling[key] : null])),
      memory: { usage_bytes: decimal(stats.memory?.usage_bytes) ? stats.memory.usage_bytes : null, limit_bytes: decimal(stats.memory?.limit_bytes) ? stats.memory.limit_bytes : null,
        semantics: "api_gauges; not_cli_cache_adjusted" },
      ...Object.fromEntries(METRICS.slice(1).map((key) => [key, counterMap(stats[key], key.startsWith("network_"))])) },
      inspect_body_sha256: hash(sample.inspect_body_sha256) ? sample.inspect_body_sha256 : null, stats_body_sha256: hash(sample.stats_body_sha256) ? sample.stats_body_sha256 : null,
      stats_frame_bytes: count(sample.stats_frame_bytes) ? sample.stats_frame_bytes : null, stats_received_bytes: count(sample.stats_received_bytes) ? sample.stats_received_bytes : null,
      inspect_window: windowProjection(sample.inspect_window), stats_window: windowProjection(sample.stats_window) }
  }) }))
  if (projected[0].samples.some((sample, index) => JSON.stringify(sample.identity) !== JSON.stringify(projected[1].samples[index].identity))) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  if (!monotoneCounters(projected[0], projected[1])) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  const interval = summarizeInterval(entries, projected[0], projected[1], { workload_elapsed_ms: elapsed })
  const compared = data(interval)
  const originals = backing.journal.filter((entry) => entry.type === "interval")
  // Honest missing daemon counters reduce descriptive metric coverage. Pairwise
  // checks above keep a reset or key drift from being masked by another null.
  if (originals.length !== 1 || interval.complete !== originals[0].complete ||
      !interval.containers.every((container) => Object.values(container.metrics).every((metric) => metric.complete === true || metric.issue === "metric_unavailable")) ||
      !["containers", "metrics", "endpoints"].every((key) => isDeepStrictEqual(compared[key], originals[0][key]))) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
  return frozen({ schema: "mount-rs.owned-layout-observer-projection.v1", original_schema: "mount-rs.backing-observer.v1", original_runner_schema: "mount-rs.runner-backing-observer.v1", complete: source.complete === true && backing.complete === true,
    runner_terminal: terminal(source.terminal), backing_terminal: terminal(backing.terminal), runner_issue_count: source.issues.length, backing_issue_count: backing.issues.length,
    original_event_count: source.events.length, original_journal_count: backing.journal.length, journal_bytes: backing.journal_bytes, dropped_entries: backing.dropped_entries,
    api_version: backing.api_version, version: { type: "version", api_version: versions[0].api_version, mode: "stream=true;first-frame", platform: "linux", body_sha256: versions[0].body_sha256 },
    caps, cost: { ...cost, pending_stages: { headers: { count: 0, age_ms: 0 }, body: { count: 0, age_ms: 0 }, retirement: { count: 0, age_ms: 0 } } }, boundaries: projected, interval,
    retention: "fixed_original_terminal_and_container_accounting_projections; raw_native_phase_inputs_private_for_strict_projection" })
}
function nativeJoin(source, identity) {
  try {
    const proof = record(source, ["native_used_identity", "kind", "native_sha256"], "COORDINATOR_NATIVE_IDENTITY_INVALID")
    return proof.native_used_identity === "verified" && proof.kind === "selected_native_file" && proof.native_sha256 === identity.sha256
  } catch { return false }
}
async function verifySelectedBinding(identity, binding) {
  const proof = await observePilotBinding(identity)
  return nativeJoin(proof, identity) && require.cache[identity.selection]?.exports === binding ? proof
    : { native_used_identity: "unverified", kind: "selected_native_file", native_sha256: null }
}

/** Pure, inert construction. The helper's public calls become owned only in run().
 * prepare ceiling: 30s native probe + 15s pool close + six 30s dispatches + two
 * 10s closes. persistence ceiling: nine 30s dispatches + four 10s closes.
 * These are cooperative whole-promise ceilings, not native cancellation limits.
 * A missed deadline permanently stops continuation even if work later settles. */
export function createOwnedLayoutComparison(input, dependencies = {}) {
  let settings
  try {
    const source = record(input, Object.hasOwn(input || {}, "budgets") ? ["binding", "environment", "identity", "fixtures", "engine", "scope", "budgets"] : ["binding", "environment", "identity", "fixtures", "engine", "scope"])
    const environment = data(source.environment), identity = data(source.identity), engine = record(data(source.engine), ["socketPath"]), scope = record(data(source.scope), ["id", "owner", "metadataPrefix", "blockPrefix"])
    const uri = environment.MOUNT_RS_TIDB_URL || environment.TIDB_URL
    if (!object(source.binding) || !object(environment) || !privateText(uri) || !["ENDPOINT", "BUCKET", "REGION", "ACCESS_KEY_ID", "SECRET_ACCESS_KEY"].every((key) => privateText(environment[`MOUNT_RS_RUSTFS_${key}`])) ||
        environment.MOUNT_RS_PROFILE_IO !== "1" || environment.MOUNT_RS_TRACE_STORAGE === "1" || !privateText(engine.socketPath) || !engine.socketPath.startsWith("/") ||
        identity?.mock !== false || identity.public?.kind !== "selected_native_file" || !hash(identity.sha256) || identity.public.native_sha256 !== identity.sha256 ||
        identity.selection !== environment.NAPI_RS_NATIVE_LIBRARY_PATH || !privateText(identity.selection) || !identity.selection.startsWith("/") || !identity.selection.endsWith(".node") ||
        !scopeText(scope.id, 61) || !scopeText(scope.owner, 128) || !scopeText(scope.metadataPrefix, 252) || !scopeText(scope.blockPrefix, 509)) reject()
    for (const key of ["MOUNT_RS_TIDB_DURABLE", "MOUNT_RS_RUSTFS_DURABLE"]) if (environment[key] !== undefined && !["0", "1"].includes(environment[key])) reject()
    const fixtures = fixturesFrom(source.fixtures, environment), budgets = { ...WHOLE_CALL_CEILINGS_MS }
    if (source.budgets !== undefined) {
      const provided = data(source.budgets)
      if (!object(provided) || Object.keys(provided).some((key) => !["prepareMs", "runnerMs", "persistenceMs"].includes(key))) reject()
      for (const [key, inputKey] of [["prepare", "prepareMs"], ["runner", "runnerMs"], ["persistence", "persistenceMs"]]) {
        if (!Object.hasOwn(provided, inputKey)) continue
        if (!Number.isSafeInteger(provided[inputKey]) || provided[inputKey] < 1 || provided[inputKey] > budgets[key]) reject()
        budgets[key] = provided[inputKey]
      }
    }
    const cohorts = frozen(ORDER.map((role, index) => ({ id: `${scope.id}/${role}`, layout: LAYOUTS[index], metadataKey: `${scope.metadataPrefix}/${role}`, blockPrefix: `${scope.blockPrefix}/${role}`, owner: scope.owner })))
    settings = { binding: source.binding, environment: frozen(environment), identity: frozen(identity), engine, fixtures, cohorts, budgets: frozen(budgets),
      metadata: { kind: "tidb", uri, durable: environment.MOUNT_RS_TIDB_DURABLE !== "0" }, blocks: { kind: "rustfs", endpoint: environment.MOUNT_RS_RUSTFS_ENDPOINT,
        bucket: environment.MOUNT_RS_RUSTFS_BUCKET, region: environment.MOUNT_RS_RUSTFS_REGION, accessKeyId: environment.MOUNT_RS_RUSTFS_ACCESS_KEY_ID,
        secretAccessKey: environment.MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY, durable: environment.MOUNT_RS_RUSTFS_DURABLE === "1" } }
  } catch { throw problem("COORDINATOR_CONFIG_INVALID") }
  // Original defaults are imported only after the caller's identity preflight and
  // before the first timed dispatch. Module evaluation never loads the addon.
  let deps, clock
  try {
    const keys = Reflect.ownKeys(Object.getOwnPropertyDescriptors(dependencies))
    if (keys.some((key) => !["createArm", "runBenchmark", "providerById", "createTransport", "createObserver", "verifyBinding", "clock"].includes(key))) reject()
    const provided = record(dependencies, keys)
    deps = { createArm: createOwnedLayoutArm, runBenchmark: async (...args) => (await import("./runner.mjs")).runBenchmark(...args),
      providerById: async (...args) => (await import("./providers.mjs")).providerById(...args), createTransport: createBackingEngineTransport, createObserver: createBackingObserver, verifyBinding: verifySelectedBinding, ...provided }
    for (const key of ["createArm", "runBenchmark", "providerById", "createTransport", "createObserver", "verifyBinding"]) if (typeof deps[key] !== "function") reject()
    clock = deps.clock || { now: () => performance.now(), setTimeout, clearTimeout }
    const clockKeys = Reflect.ownKeys(Object.getOwnPropertyDescriptors(clock))
    if (clockKeys.some((key) => !["now", "utc", "cpu", "setTimeout", "clearTimeout"].includes(key))) reject()
    clock = record(clock, clockKeys)
    for (const key of ["now", "setTimeout", "clearTimeout"]) if (typeof clock[key] !== "function") reject()
    if (!finite(clock.now())) reject()
  } catch { throw problem("COORDINATOR_CONFIG_INVALID") }
  const observerClock = { now: () => clock.now(), setTimeout: (...args) => clock.setTimeout(...args), clearTimeout: (...args) => clock.clearTimeout(...args),
    utc: typeof clock.utc === "function" ? () => clock.utc() : () => new Date().toISOString(), cpu: typeof clock.cpu === "function" ? () => clock.cpu() : () => process.cpuUsage() }
  const arms = [], pending = new Set(), privateEvidence = [], seenArms = new WeakSet(), seenTransports = new WeakSet(), seenObservers = new WeakSet()
  let status = "idle", stopCode = null, stoppedAfter = null, uncertainty = false, completed = 0, baseline = null, taken = false
  function snapshot() {
    const complete = status === "complete" && !stopCode && !uncertainty && completed === 4
    return frozen({ schema: "mount-rs.owned-layout-comparison.v1", status, order: ORDER, budgets_ms: settings.budgets,
      completed_arms: completed, stopped_after: stoppedAfter, stop_code: stopCode, native_uncertainty: uncertainty, pending_owned_call_count: pending.size,
      complete, comparable: complete && arms.every((arm) => arm.comparison_safe_to_continue),
      floor_qualified: arms.length === 4 && arms.every((arm) => arm.outcome?.floor_qualified === true),
      safe_to_continue: complete && arms.every((arm) => arm.comparison_safe_to_continue), arms: arms.map((arm) => ({ ...arm })),
      evidence_scope: { workload: "original_400_lifecycle_iterations_concurrency64_payload4096_chunk65536_iops_floor1000",
        deadlines: "enclosing_cooperative_promise_ceilings; no_native_cancellation_guarantee; missed_deadline_permanently_stops_sequence",
        fixture_identity: "eight_owner_bound_allowlist_entries_and_existing_observed_container_identities; no_additional_engine_calls",
        endpoint_binding: "unobserved_here; requires_outer_entry_controller_endpoint_and_manifest_join", owner_teardown: "unobserved_here; requires_final_owner_verified_teardown",
        native_build_provenance: "selected_native_file_identity_join_only; requires_outer_entry_pinned_source_and_build_evidence",
        cache_state: "uncontrolled", interpretation: "descriptive_ABBA_accounting; not_cold_cache_physical_IOPS_or_causal_improvement_proof" },
      retention: "closed_bounded_redacted_projection_including_fixed_native_metrics; original_phase_records_private_for_independent_verification", output_cap_bytes: OUTPUT_CAP })
  }
  function stop(code, role) { stopCode ||= code; stoppedAfter ||= role; status = "stopped" }
  function capture(arm, cohort) { return armProjection(arm, cohort, settings.metadata.durable, settings.blocks.durable) }
  function uncertainState(state) {
    if (state.available !== true || state.counts.initial_create_calls > 0 && !state.original_shutdown_confirmed || state.counts.reopen_create_calls > state.reopen_shutdown_confirmed_count) uncertainty = true
  }
  async function ownedCall(phase, operation) {
    const start = clock.now(), deadline = start + settings.budgets[phase], token = {}
    if (!finite(start)) reject(`COORDINATOR_${phase.toUpperCase()}_FAILED`)
    const overdue = () => { uncertainty = true; return problem(`COORDINATOR_${phase.toUpperCase()}_TIMEOUT`) }
    const guard = () => { if (status !== "running" || uncertainty || !finite(clock.now()) || clock.now() >= deadline) throw overdue() }
    pending.add(token)
    const promise = Promise.resolve().then(() => { guard(); return operation(guard) })
    promise.then(() => pending.delete(token), () => pending.delete(token))
    let timer
    const timeout = new Promise((_resolve, rejectPromise) => { timer = clock.setTimeout(() => rejectPromise(overdue()), settings.budgets[phase]) })
    try {
      const result = await Promise.race([promise, timeout])
      if (!finite(clock.now()) || clock.now() >= deadline) throw overdue()
      return result
    } catch (error) {
      if (!finite(clock.now()) || clock.now() >= deadline) throw overdue()
      throw issued.has(error) ? error : problem(`COORDINATOR_${phase.toUpperCase()}_FAILED`)
    } finally { clock.clearTimeout(timer) }
  }
  function fresh(value, seen) { if (!object(value) || seen.has(value)) reject("COORDINATOR_ARM_STATE_INVALID"); seen.add(value); return value }
  async function run() {
    if (status !== "idle") throw problem("COORDINATOR_SINGLE_USE")
    status = "running"
    for (let index = 0; index < 4; index++) {
      const cohort = settings.cohorts[index], role = ORDER[index]
      const result = { role, layout: cohort.layout, status: "not_started", outcome: null, persistence: null, native_identity_verified: false, fixture_identity_stable: false,
        prepared: false, persistence_verified: false, comparison_safe_to_continue: false, backing: null, native_metrics: null }
      arms.push(result)
      let arm, benchmark
      try {
        await ownedCall("prepare", async (guard) => {
          if (!nativeJoin(await deps.verifyBinding(settings.identity, settings.binding), settings.identity)) reject("COORDINATOR_NATIVE_IDENTITY_INVALID")
          guard()
          result.native_identity_verified = true
          arm = fresh(deps.createArm(settings.binding, { cohort, metadata: { ...settings.metadata, key: cohort.metadataKey }, blocks: { ...settings.blocks, key: cohort.blockPrefix } }), seenArms)
          guard()
          await arm.prepare()
        })
        result.persistence = capture(arm, cohort)
        if (!armProof(result.persistence, "prepared")) reject("COORDINATOR_ARM_STATE_INVALID")
        result.prepared = true
        const raw = await ownedCall("runner", async (guard) => {
          const definitions = await deps.providerById(settings.environment), original = definitions instanceof Map ? definitions.get(PROVIDER) : definitions?.[PROVIDER]
          guard()
          if (original?.id !== PROVIDER) reject("COORDINATOR_RUNNER_FAILED")
          const transport = fresh(deps.createTransport({ socketPath: settings.engine.socketPath, allowlistedCids: settings.fixtures.entries.map((entry) => entry.cid) }), seenTransports)
          const observer = fresh(deps.createObserver({ allowlist: settings.fixtures.entries, transport, clock: observerClock }), seenObservers)
          const { parseArgs } = await import("./runner.mjs")
          guard()
          const options = frozen(parseArgs(["--provider", PROVIDER, "--layout", cohort.layout, "--workload", "lifecycle", "--sizes", "1", "--iterations", "400", "--concurrency", "64",
            "--payload-bytes", "4096", "--chunk-size-bytes", "65536", "--min-iops", "1000", "--require-configured"]))
          return deps.runBenchmark(options, settings.environment, new Map([[PROVIDER, { ...original, create: () => arm.takeForRunner() }]]),
            { backingObserver: observer, backingObserverWindow: "workload", backingPilotIdentity: settings.identity, observerClock })
        })
        benchmark = frozen(data(raw))
        privateEvidence.push(frozen({ role, cohort, benchmark }))
        result.outcome = assessOwnedLayoutRunnerOutcome(benchmark)
        result.status = result.outcome.status
        const workloads = benchmark.providers?.[0]?.storageDiagnostics?.phases?.filter((phase) => typeof phase?.name === "string" && phase.name.startsWith("workload-"))
        result.native_metrics = projectOwnedLayoutPhaseMetrics(workloads?.length === 1 ? workloads[0] : null)
        result.persistence = capture(arm, cohort)
        if (benchmark.config?.layout !== cohort.layout) reject("COORDINATOR_RUNNER_OUTCOME_INVALID")
        if (!result.outcome.runner_safe_to_continue) {
          if (result.outcome.native_quiescent !== true || result.outcome.owned_operations_settled !== true || result.outcome.pending_operation_count !== 0 ||
              result.outcome.timeout_count > 0 || result.outcome.original_safe_to_continue_pair !== true) uncertainty = true
          reject("COORDINATOR_RUNNER_OUTCOME_INVALID")
        }
        if (result.native_metrics.status !== "observed") reject("COORDINATOR_NATIVE_METRICS_UNAVAILABLE")
        if (!nativeJoin(benchmark.providers[0].backingPilotIdentity, settings.identity)) reject("COORDINATOR_NATIVE_IDENTITY_INVALID")
        if (!armProof(result.persistence, "original_closed")) reject("COORDINATOR_ARM_STATE_INVALID")
        result.backing = observerProjection(benchmark.providers[0].backingObserver, settings.fixtures.entries, result.outcome.elapsed_ms)
        const observed = result.backing.boundaries[0].samples.map((sample) => sample.identity)
        if (baseline && JSON.stringify(baseline) !== JSON.stringify(observed)) reject("COORDINATOR_FIXTURE_IDENTITY_INVALID")
        baseline ||= observed
        result.fixture_identity_stable = true
        if (cohort.layout === "compact") {
          const runnerReceipt = validateCompactLayoutReceipt(benchmark.providers[0].layoutSelection.persistedReceipt)
          if (!decimal(runnerReceipt.structuralGeneration) || /^[0-9a-f]{32}$/u.exec(runnerReceipt.backingId)?.[0] !== runnerReceipt.backingId) reject("COORDINATOR_LAYOUT_CONTINUITY_INVALID")
          const timed = { kind: "MRC5", receipt: runnerReceipt }, observations = result.persistence.layout_observations
          if (!continuity(timed, observations.before_timed) || !continuity(observations.drained_original, timed)) reject("COORDINATOR_LAYOUT_CONTINUITY_INVALID")
        }
        await ownedCall("persistence", () => arm.verifyPersistence())
        result.persistence = capture(arm, cohort)
        if (!armProof(result.persistence, "verified")) reject("COORDINATOR_ARM_STATE_INVALID")
        result.persistence_verified = true
        result.comparison_safe_to_continue = true
        completed++
      } catch (error) {
        if (arm) { result.persistence = capture(arm, cohort); uncertainState(result.persistence) }
        result.status = result.outcome?.status || "failed"
        stop(issued.has(error) ? error.code : "COORDINATOR_ARM_STATE_INVALID", role)
        break
      }
    }
    if (!stopCode) status = "complete"
    if (Buffer.byteLength(JSON.stringify(snapshot())) > OUTPUT_CAP) stop("COORDINATOR_PUBLICATION_INCOMPLETE", ORDER[Math.max(0, arms.length - 1)])
    return snapshot()
  }
  function takePrivateEvidence() {
    if (!["complete", "stopped"].includes(status) || taken) throw problem("COORDINATOR_SINGLE_USE")
    taken = true
    return frozen({ schema: "mount-rs.owned-layout-private-evidence.v1", complete: snapshot().complete, arms: [...privateEvidence],
      scope: "private_original_runner_input_for_independent_verification; public_native_metrics_are_separately_closed_projected_receipts" })
  }
  const capability = { run, snapshot }
  Object.defineProperties(capability, { cohorts: { value: settings.cohorts }, takePrivateEvidence: { value: takePrivateEvidence } })
  return Object.freeze(capability)
}
