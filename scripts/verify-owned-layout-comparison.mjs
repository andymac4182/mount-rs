import { Buffer } from "node:buffer"
import { createHash } from "node:crypto"
import { types, isDeepStrictEqual } from "node:util"
import { constants } from "node:fs"
import { lstat, open, realpath } from "node:fs/promises"
import { dirname, isAbsolute, join, posix, relative, resolve, sep } from "node:path"
import { assessOwnedLayoutRunnerOutcome } from "../benchmarks/storage/owned-layout-outcome.mjs"
import { projectOwnedLayoutPhaseMetrics } from "../benchmarks/storage/owned-layout-metrics.mjs"
import { projectInspect, summarizeInterval } from "../benchmarks/storage/backing-observer.mjs"
import { validateCompactLayoutReceipt } from "../benchmarks/storage/compact-layout.mjs"
import { validateSplitNamespacePresenceReceipt } from "../benchmarks/storage/namespace-presence.mjs"

// Pure imports above are inert. No entry, runner, provider or addon is imported.
export const SOURCE_PATHS = Object.freeze([
  "Cargo.toml", "Cargo.lock", "bindings/mount-rs-napi/Cargo.toml", "bindings/mount-rs-napi/index.js",
  "bindings/mount-rs-napi/src/lib.rs", "bindings/mount-rs-napi/src/namespace_presence.rs", "src/diagnostics/storage.rs", "src/diagnostics/profile.rs",
  "scripts/test-tidb.sh", "scripts/test-rustfs.sh", "scripts/rustfs-combo-runner.py", "scripts/rustfs-bounded-docker.py",
  ".github/workflows/remote-drives.yml", "benchmarks/storage/errors.mjs", "benchmarks/storage/stats.mjs", "benchmarks/storage/runner.mjs",
  "benchmarks/storage/providers.mjs", "benchmarks/storage/diagnostics.mjs", "benchmarks/storage/backing-engine-transport.mjs",
  "benchmarks/storage/backing-observer.mjs", "benchmarks/storage/compact-layout.mjs", "benchmarks/storage/namespace-presence.mjs",
  "benchmarks/storage/owned-backing-pilot.mjs", "benchmarks/storage/owned-layout-arm.mjs", "benchmarks/storage/owned-layout-outcome.mjs",
  "benchmarks/storage/owned-layout-metrics.mjs", "benchmarks/storage/owned-layout-comparison.mjs", "benchmarks/storage/owned-layout-entry.mjs",
])
const INPUTS = ["projection", "originals", "fixtures", "controller", "engine", "build", "native"]
const LIMITS = Object.freeze({ projection: 33554432, originals: 33554432, fixtures: 16384, controller: 16384, engine: 16384, build: 65536, native: 134217728, source: 2097152 })
const ORDER = ["A1", "B1", "B2", "A2"], LAYOUTS = ["legacy", "compact", "compact", "legacy"]
const ROLES = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const METRICS = ["cpu_usage_ns", "block_bytes", "block_operations", "network_rx_bytes", "network_tx_bytes"]
const CAPS = Object.freeze({ maxContainers: 16, maxRequests: 257, maxBoundaries: 64, maxResponseBytes: 1048576,
  maxDeviceEntries: 128, maxNetworkInterfaces: 32, maxJournalBytes: 8388608, requestTimeoutMs: 2000, ownerTimeoutMs: 60000 })
const CEILINGS = { prepare: 245000, runner: 60000, persistence: 310000 }
const COUNT_FIELDS = ["preflight_calls", "initial_create_calls", "reopen_create_calls", "original_shutdown_calls", "reopen_shutdown_calls",
  "canary_write_calls", "canary_bytes_written", "canary_sync_calls", "canary_write_close_calls", "canary_read_calls", "canary_bytes_read",
  "canary_eof_read_calls", "canary_eof_bytes_read", "canary_read_close_calls", "canary_remove_calls", "initial_empty_calls", "final_empty_calls", "layout_inspection_calls"]
const OBSERVER_RETENTION = "selected_projections_and_raw_version_inspect_or_first_stats_frame_sha256; consumed_trailing_bytes_counted_discarded; raw_bodies_discarded"
const COST_SCOPES = { response_bytes_scope: "adapter_yielded_bytes_including_consumed_same_chunk_tail; rejected_overflow_chunks_unavailable; not_wire_bytes",
  wall_scope: "inclusive_request_windows; concurrent_windows_overlap; not_exclusive_hook_wall", cpu_scope: "inclusive_process_cpu_during_observer_calls; overlapping_work_not_isolated" }
const COST_COUNTS = ["requests", "response_bytes", "cpu_user_us", "cpu_system_us", "peak_in_flight", "headers_received", "first_frames_received", "streamed_iterators_retired", "consumed_trailing_bytes", "in_flight"]
const ARM_CODES = new Set(["CONFIG_INVALID", "API_UNAVAILABLE", "STATE_INVALID", "PREFLIGHT_FAILED", "CREATE_FAILED", "REOPEN_FAILED",
  "REOPEN_NOT_FRESH", "LAYOUT_PROOF_FAILED", "GENERATION_CONTINUITY_FAILED", "CANARY_WRITE_FAILED", "CANARY_READ_FAILED", "CANARY_EOF_FAILED",
  "CANARY_REMOVE_FAILED", "EMPTY_PROOF_FAILED", "HANDLE_CLOSE_FAILED", "SHUTDOWN_FAILED"].map((code) => `OWNED_LAYOUT_ARM_${code}`))
const STOP_CODES = new Set(["ARM_STATE_INVALID", "CONFIG_INVALID", "FIXTURE_IDENTITY_INVALID", "LAYOUT_CONTINUITY_INVALID",
  "NATIVE_IDENTITY_INVALID", "NATIVE_METRICS_UNAVAILABLE", "PUBLICATION_INCOMPLETE", "RUNNER_OUTCOME_INVALID",
  ...["PREPARE", "RUNNER", "PERSISTENCE"].flatMap((phase) => [`${phase}_TIMEOUT`, `${phase}_FAILED`])].map((code) => `COORDINATOR_${code}`))
const ENTRY_CODES = new Set(["CONFIG_INVALID", "HANDOFF_REJECTED", "ENDPOINT_BINDING_REJECTED", "ENGINE_BINDING_REJECTED", "SCOPE_REJECTED",
  "PRIVATE_FILE_REJECTED", "JSON_REJECTED", "BUILD_IDENTITY_REJECTED", "SOURCE_IDENTITY_REJECTED", "NATIVE_SELECTION_REJECTED",
  "NATIVE_LOAD_FAILED", "NATIVE_JOIN_REJECTED", "CAPTURE_JOIN_REJECTED", "OUTPUT_CAP", "PUBLICATION_FAILED", "COMPARISON_FAILED", "TESTING_REJECTED"].map((code) => `OWNED_LAYOUT_ENTRY_${code}`))
const CHECK_CODES = new Set(["INPUT_SHAPE_INVALID", "BUFFER_REQUIRED", "INPUT_CAP_EXCEEDED", "JSON_INVALID", "SCHEMA_INVALID", "BYTE_JOIN_INVALID",
  "SOURCE_JOIN_INVALID", "NATIVE_JOIN_INVALID", "HANDOFF_JOIN_INVALID", "COHORT_INVALID", "ORIGINAL_OUTCOME_MISMATCH", "OBSERVER_INVALID",
  "FIXTURE_IDENTITY_INVALID", "PERSISTENCE_INVALID", "NATIVE_METRICS_MISMATCH", "CONTINUATION_INVALID", "PUBLICATION_INVALID", "ARGUMENTS_INVALID", "FILE_INVALID", "RECORD_INCOMPLETE"])
const SCOPE = Object.freeze({ verification: "offline_retained_record_consistency; no_runtime_or_endpoint_dispatch",
  source: "exact_28_reviewed_runtime_controller_seam_hashes; not_full_build_dependency_closure",
  build: "declared_seal_flags_and_bytes_joined; clean_checkout_and_build_not_independently_observed",
  container: "recomputed_selected_counter_intervals; raw_inspect_stats_bodies_not_retained_or_rehashed",
  capacity: "unavailable_missing_actual_owner_capacity_producer", owner_terminal: "unavailable_missing_actual_owner_terminal_producer",
  interpretation: "descriptive_ABBA_accounting; no_hosted_qualification_or_causal_physical_IOPS_proof" })
const COMPARISON_SCOPE = Object.freeze({ workload: "original_400_lifecycle_iterations_concurrency64_payload4096_chunk65536_iops_floor1000",
  deadlines: "enclosing_cooperative_promise_ceilings; no_native_cancellation_guarantee; missed_deadline_permanently_stops_sequence",
  fixture_identity: "eight_owner_bound_allowlist_entries_and_existing_observed_container_identities; no_additional_engine_calls",
  endpoint_binding: "unobserved_here; requires_outer_entry_controller_endpoint_and_manifest_join", owner_teardown: "unobserved_here; requires_final_owner_verified_teardown",
  native_build_provenance: "selected_native_file_identity_join_only; requires_outer_entry_pinned_source_and_build_evidence", cache_state: "uncontrolled",
  interpretation: "descriptive_ABBA_accounting; not_cold_cache_physical_IOPS_or_causal_improvement_proof" })
const issued = new WeakSet(), code = (suffix) => `OWNED_LAYOUT_CHECK_${suffix}`
function reject(suffix) { const error = Object.assign(new Error(code(suffix)), { code: code(suffix) }); issued.add(error); throw error }
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value)
const count = (value) => Number.isSafeInteger(value) && value >= 0
const finite = (value) => Number.isFinite(value) && value >= 0
const hash = (value) => typeof value === "string" && /^[a-f0-9]{64}$/u.exec(value)?.[0] === value
const decimal = (value) => typeof value === "string" && /^(0|[1-9][0-9]{0,19})$/u.exec(value)?.[0] === value && BigInt(value) <= 18446744073709551615n
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
const same = isDeepStrictEqual
function requireThat(condition, suffix) { if (!condition) reject(suffix) }
function record(source, keys, suffix = "SCHEMA_INVALID") {
  requireThat(!types.isProxy(source) && object(source) && [Object.prototype, null].includes(Object.getPrototypeOf(source)), suffix)
  const descriptors = Object.getOwnPropertyDescriptors(source)
  requireThat(Reflect.ownKeys(descriptors).length === keys.length && keys.every((key) => Object.hasOwn(descriptors, key) && Object.hasOwn(descriptors[key], "value")), suffix)
  return Object.fromEntries(keys.map((key) => [key, descriptors[key].value]))
}
function freeze(value) {
  if (value && typeof value === "object" && !Object.isFrozen(value)) { for (const child of Object.values(value)) freeze(child); Object.freeze(value) }
  return value
}
function freshReport() {
  return { schema: "mount-rs.owned-layout-independent-check.v1", status: "rejected", runtime_scope: "unavailable", hosted_qualified: false,
    publication_complete: false, originals_retained: false, checked_arm_count: 0, floor_qualified: false, comparable: false, safe_to_continue: false,
    native_uncertainty: null, arms: [], joins: { ...Object.fromEntries(INPUTS.map((key) => [`${key}_sha256`, null])), source_count: 0, seam_hashes_match: false },
    failure_codes: [], evidence_scope: SCOPE }
}
const typedLength = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(Uint8Array.prototype), "byteLength").get
const typedSet = Uint8Array.prototype.set
function buffer(value, maximum) {
  requireThat(!types.isProxy(value) && Buffer.isBuffer(value) && Object.getPrototypeOf(value) === Buffer.prototype, "BUFFER_REQUIRED")
  // Known byte/method overrides are never called. Branded byte copying ignores
  // arbitrary unused Buffer properties instead of enumerating128MiB indices.
  for (const key of ["length", "byteLength", "buffer", "byteOffset", "constructor", "toJSON", Symbol.iterator]) requireThat(!Object.hasOwn(value, key), "BUFFER_REQUIRED")
  const length = typedLength.call(value)
  requireThat(length > 0 && length <= maximum, "INPUT_CAP_EXCEEDED")
  const result = Buffer.alloc(length); typedSet.call(result, value); return result
}
function parse(bytes) {
  try {
    const source = new TextDecoder("utf-8", { fatal: true }).decode(bytes), stack = []
    for (let index = 0; index < source.length; index++) {
      const char = source[index]
      if (char === "{") stack.push(new Set())
      else if (char === "[") stack.push(null)
      else if (char === "}" || char === "]") stack.pop()
      else if (char === '"') {
        const start = index++
        for (; index < source.length && source[index] !== '"'; index++) if (source[index] === "\\") index++
        let next = index + 1; while (/\s/u.test(source[next] || "") && next < source.length) next++
        if (source[next] === ":") { const keys = stack.at(-1), key = JSON.parse(source.slice(start, index + 1)); if (!(keys instanceof Set) || keys.has(key)) reject("JSON_INVALID"); keys.add(key) }
      }
      if (stack.length > 40) reject("JSON_INVALID")
    }
    const value = JSON.parse(source), queue = [[value, 0]]; let nodes = 0, textBytes = 0
    while (queue.length) {
      const [item, depth] = queue.pop(); if (++nodes > 500000 || depth > 40) reject("JSON_INVALID")
      if (typeof item === "number" && !Number.isFinite(item)) reject("JSON_INVALID")
      if (typeof item === "string") textBytes += Buffer.byteLength(item)
      if (item && typeof item === "object") for (const [key, child] of Object.entries(item)) { textBytes += Buffer.byteLength(key); queue.push([child, depth + 1]) }
      if (textBytes > LIMITS.originals) reject("JSON_INVALID")
    }
    return value
  } catch { reject("JSON_INVALID") }
}

// Fixed selected-field reconstruction follows the committed comparison producer.
function identityProjection(source, entry) {
  const actual = record(source, ["cid", "role", "labels", "image", "running", "started_at", "restart_count", "limits", "configured_limit_semantics"], "OBSERVER_INVALID")
  record(actual.limits, ["Memory", "MemorySwap", "NanoCpus", "CpuQuota", "CpuPeriod", "CpuShares", "PidsLimit", "CpusetCpus"], "OBSERVER_INVALID")
  const labels = record(actual.labels, Object.keys(entry.labels), "OBSERVER_INVALID")
  const projected = projectInspect({ Id: actual.cid, Image: actual.image, State: { Running: actual.running, StartedAt: actual.started_at },
    RestartCount: actual.restart_count, HostConfig: actual.limits, Config: { Labels: labels } }, entry)
  if (actual.role !== entry.role || actual.configured_limit_semantics !== projected.configured_limit_semantics ||
      Object.keys(projected.limits).some((key) => actual.limits[key] !== projected.limits[key]) ||
      Object.entries(entry.labels).some(([key, value]) => labels[key] !== value)) reject("OBSERVER_INVALID")
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
  if (!object(source)) reject("OBSERVER_INVALID")
  const entries = Object.entries(source)
  if (entries.length > (network ? 32 : 128) || entries.length === 0) reject("OBSERVER_INVALID")
  for (const [key, value] of entries) {
    const device = /^(0|[1-9][0-9]{0,19}):(0|[1-9][0-9]{0,19}):(Read|Write)$/u.exec(key)
    const validKey = network ? /^[A-Za-z][A-Za-z0-9_.-]{0,14}$/u.exec(key)?.[0] === key && !["constructor", "prototype"].includes(key)
      : device?.[0] === key && decimal(device[1]) && decimal(device[2])
    if (!validKey || !(network && value === null || decimal(value))) reject("OBSERVER_INVALID")
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
  const caps = record(backing?.caps, Object.keys(CAPS), "OBSERVER_INVALID")
  const cost = record(backing?.cost, [...COST_COUNTS, "wall_ms", "owner_lifetime_ms", "pending_stages", ...Object.keys(COST_SCOPES)], "OBSERVER_INVALID")
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
      cost.consumed_trailing_bytes > cost.response_bytes || Object.entries(COST_SCOPES).some(([key, value]) => cost[key] !== value)) reject("OBSERVER_INVALID")
  const pendingStages = record(cost.pending_stages, ["headers", "body", "retirement"], "OBSERVER_INVALID")
  for (const stage of Object.values(pendingStages)) {
    const state = record(stage, ["count", "age_ms"], "OBSERVER_INVALID")
    if (state.count !== 0 || state.age_ms !== 0) reject("OBSERVER_INVALID")
  }
  if (!Array.isArray(backing?.allowlist) || backing.allowlist.length !== 8 || entries.some((entry) => {
    const matches = backing.allowlist.filter((actual) => actual.cid === entry.cid)
    if (matches.length !== 1) return true
    const actual = record(matches[0], ["cid", "role", "labels"], "OBSERVER_INVALID")
    const labels = record(actual.labels, Object.keys(entry.labels), "OBSERVER_INVALID")
    return actual.role !== entry.role || Object.entries(entry.labels).some(([key, value]) => labels[key] !== value)
  })) reject("OBSERVER_INVALID")
  const boundaries = backing?.journal?.filter((entry) => entry.type === "boundary")
  if (!Array.isArray(boundaries) || boundaries.length !== 2 || boundaries.some((entry, index) => entry.id !== `workload-4096bytes:${index ? "end" : "begin"}` || entry.kind !== "phase" || !Array.isArray(entry.samples) || entry.samples.length !== 8)) reject("OBSERVER_INVALID")
  const projected = boundaries.map((boundary) => ({ type: "boundary", id: boundary.id, kind: "phase", samples: entries.map((entry) => {
    const matches = boundary.samples.filter((sample) => sample.cid === entry.cid)
    if (matches.length !== 1) reject("OBSERVER_INVALID")
    const sample = matches[0], stats = sample.stats
    if (stats?.cid !== entry.cid || !decimal(stats.read_ns) || !(stats.cpu_usage_ns === null || decimal(stats.cpu_usage_ns))) reject("OBSERVER_INVALID")
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
  if (projected[0].samples.some((sample, index) => JSON.stringify(sample.identity) !== JSON.stringify(projected[1].samples[index].identity))) reject("OBSERVER_INVALID")
  if (!monotoneCounters(projected[0], projected[1])) reject("OBSERVER_INVALID")
  const interval = summarizeInterval(entries, projected[0], projected[1], { workload_elapsed_ms: elapsed })
  const compared = interval
  const originals = backing.journal.filter((entry) => entry.type === "interval")
  // Honest missing daemon counters reduce descriptive metric coverage. Pairwise
  // checks above keep a reset or key drift from being masked by another null.
  if (originals.length !== 1 || interval.complete !== originals[0].complete ||
      !interval.containers.every((container) => Object.values(container.metrics).every((metric) => metric.complete === true || metric.issue === "metric_unavailable")) ||
      !["containers", "metrics", "endpoints"].every((key) => isDeepStrictEqual(compared[key], originals[0][key]))) reject("OBSERVER_INVALID")
  return freeze({ schema: "mount-rs.owned-layout-observer-projection.v1", original_schema: "mount-rs.backing-observer.v1", original_runner_schema: "mount-rs.runner-backing-observer.v1", complete: source.complete === true && backing.complete === true,
    runner_terminal: terminal(source.terminal), backing_terminal: terminal(backing.terminal), runner_issue_count: source.issues.length, backing_issue_count: backing.issues.length,
    original_event_count: source.events.length, original_journal_count: backing.journal.length, journal_bytes: backing.journal_bytes, dropped_entries: backing.dropped_entries,
    api_version: backing.api_version, version: { type: "version", api_version: versions[0].api_version, mode: "stream=true;first-frame", platform: "linux", body_sha256: versions[0].body_sha256 },
    caps, cost: { ...cost, pending_stages: { headers: { count: 0, age_ms: 0 }, body: { count: 0, age_ms: 0 }, retirement: { count: 0, age_ms: 0 } } }, boundaries: projected, interval,
    retention: "fixed_original_terminal_and_container_accounting_projections; raw_native_phase_inputs_private_for_strict_projection" })
}
function handoff(fixtures, controller, engine, originals, projection, digests) {
  record(fixtures, ["schema", "tidb_owner", "rustfs_owner", "generation", "entries"], "HANDOFF_JOIN_INVALID")
  record(controller, ["schema", "generation", "tidb_owner", "rustfs_owner", "fixture_sha256", "engine_capability_sha256", "tidb_url", "rustfs_endpoint", "rustfs_bucket", "scope"], "HANDOFF_JOIN_INVALID")
  record(engine, ["socket_path"], "HANDOFF_JOIN_INVALID")
  const text = (value) => typeof value === "string" && value.length > 0 && !value.includes("\0") && Buffer.byteLength(value) <= 4096
  const owner = (value) => typeof value === "string" && /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,119}$/u.exec(value)?.[0] === value && ![".", ".."].includes(value)
  requireThat(fixtures.schema === "mount-rs.owned-backing-cids.v1" && controller.schema === "mount-rs.owned-layout-controller.v1" && fixtures.generation === "1" && controller.generation === "1" &&
    owner(fixtures.tidb_owner) && owner(fixtures.rustfs_owner) && fixtures.tidb_owner === controller.tidb_owner && fixtures.rustfs_owner === controller.rustfs_owner &&
    controller.fixture_sha256 === digests.fixtures_sha256 && controller.engine_capability_sha256 === digests.engine_sha256 && engine.socket_path === "/var/run/docker.sock", "HANDOFF_JOIN_INVALID")
  requireThat(Array.isArray(fixtures.entries) && fixtures.entries.length === 8, "HANDOFF_JOIN_INVALID")
  const seen = new Set(), entries = ROLES.map((role) => {
    const matches = fixtures.entries.filter((entry) => entry?.role === role); requireThat(matches.length === 1, "HANDOFF_JOIN_INVALID")
    const entry = record(matches[0], ["cid", "role", "labels"], "HANDOFF_JOIN_INVALID")
    const labels = role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": fixtures.rustfs_owner } : { "mount-rs.tidb.run": fixtures.tidb_owner }
    requireThat(hash(entry.cid) && !seen.has(entry.cid) && same(record(entry.labels, Object.keys(labels), "HANDOFF_JOIN_INVALID"), labels), "HANDOFF_JOIN_INVALID")
    seen.add(entry.cid); return { role, cid: entry.cid, labels }
  })
  try {
    const tidb = new URL(controller.tidb_url), blob = new URL(controller.rustfs_endpoint), port = (url) => /^[1-9][0-9]{0,4}$/u.test(url.port) && Number(url.port) <= 65535
    requireThat(text(controller.tidb_url) && text(controller.rustfs_endpoint) && text(controller.rustfs_bucket) &&
      tidb.protocol === "mysql:" && tidb.hostname === "127.0.0.1" && port(tidb) && !tidb.search && !tidb.hash && blob.protocol === "http:" && blob.hostname === "127.0.0.1" &&
      port(blob) && !blob.username && !blob.password && !blob.search && !blob.hash && blob.pathname === "/", "HANDOFF_JOIN_INVALID")
  } catch { reject("HANDOFF_JOIN_INVALID") }
  const scope = record(controller.scope, ["id", "owner", "metadataPrefix", "blockPrefix"], "HANDOFF_JOIN_INVALID")
  requireThat(/^layout-[a-f0-9]{32}$/u.exec(scope.id)?.[0] === scope.id && scope.owner === fixtures.tidb_owner && scope.metadataPrefix === `mount-rs-owned-layout/${fixtures.tidb_owner}/${scope.id}` &&
    scope.blockPrefix === `mount-rs-owned-layout/${fixtures.rustfs_owner}/${scope.id}`, "HANDOFF_JOIN_INVALID")
  const scopeHash = sha(Buffer.from(JSON.stringify(scope)))
  requireThat(same(originals.joins.scope, { sha256: scopeHash, value: scope }), "HANDOFF_JOIN_INVALID")
  requireThat(same(projection.handoff, { status: "verified", generation: "1", fixture_count: 8, fixture_sha256: digests.fixtures_sha256,
    engine_capability_sha256: digests.engine_sha256, scope_sha256: scopeHash, endpoint_binding: "controller_manifest_join_verified",
    scope_binding: "owner_scoped_prefixes_verified", evidence_scope: "private_controller_created_endpoint_and_manifest_join; not_active_endpoint_probe", controller_receipt_sha256: digests.controller_sha256 }), "HANDOFF_JOIN_INVALID")
  requireThat(same(projection.ownership, { tidb_owner: fixtures.tidb_owner, rustfs_owner: fixtures.rustfs_owner, generation: "1", scope_sha256: scopeHash,
    final_teardown: { status: "unverified" }, namespace_purge: "unverified" }), "HANDOFF_JOIN_INVALID")
  return { entries, scope }
}
function observation(source, layout, flags) {
  if (source === null) return null
  if (layout === "legacy") {
    requireThat(same(source, { kind: "recognized_non_mrc5", exact_legacy_marker: false, fresh_preflight_required: true, constructor_flags: flags }), "PERSISTENCE_INVALID")
    return source
  }
  record(source, ["kind", "receipt"], "PERSISTENCE_INVALID")
  requireThat(source.kind === "MRC5", "PERSISTENCE_INVALID")
  try { const value = validateCompactLayoutReceipt(source.receipt); requireThat(/^[0-9a-f]{32}$/u.exec(value.backingId)?.[0] === value.backingId && decimal(value.structuralGeneration), "PERSISTENCE_INVALID") }
  catch { reject("PERSISTENCE_INVALID") }
  return source
}
function continuity(first, last, exact = false) {
  if (!first || !last || first.kind !== last.kind) return false
  return first.kind === "recognized_non_mrc5" || first.receipt.backingId === last.receipt.backingId &&
    (exact ? first.receipt.structuralGeneration === last.receipt.structuralGeneration : BigInt(last.receipt.structuralGeneration) >= BigInt(first.receipt.structuralGeneration))
}
function persistence(source, layout, benchmark, completed) {
  if (source === null) { requireThat(!completed, "PERSISTENCE_INVALID"); return null }
  if (source?.available === false) { record(source, ["schema", "available"], "PERSISTENCE_INVALID"); requireThat(source.schema === "mount-rs.owned-layout-arm.v1" && !completed, "PERSISTENCE_INVALID"); return null }
  record(source, ["schema", "available", "layout", "phase", "complete", "construction", "preflight", "original_shutdown_confirmed", "reopen_shutdown_confirmed_count", "layout_observations", "canary", "counts", "counter_scope", "failure_code", "cleanup_codes"], "PERSISTENCE_INVALID")
  const flags = { concurrentWrites: layout === "compact", inodeUpdates: layout === "compact", compactInodeUpdates: layout === "compact" }
  const construction = record(source.construction, ["metadata_provider", "block_provider", "chunk_size_bytes", "metadata_durable", "blocks_durable", "flags"], "PERSISTENCE_INVALID")
  requireThat(source.schema === "mount-rs.owned-layout-arm.v1" && source.available === true && source.layout === layout &&
    ["idle", "preparing", "prepared", "handed_off", "closing_original", "original_closed", "verifying", "verified", "failed"].includes(source.phase) && source.complete === (source.phase === "verified") &&
    construction.metadata_provider === "tidb" && construction.block_provider === "rustfs" && construction.chunk_size_bytes === 65536 &&
    typeof construction.metadata_durable === "boolean" && typeof construction.blocks_durable === "boolean" && same(construction.flags, flags) &&
    typeof source.original_shutdown_confirmed === "boolean" && count(source.reopen_shutdown_confirmed_count) &&
    (source.failure_code === null || ARM_CODES.has(source.failure_code)) && Array.isArray(source.cleanup_codes) && source.cleanup_codes.every((value) => ARM_CODES.has(value)) &&
    source.counter_scope === "untimed_helper_invocations_and_public_api_dispatches; not_backend_or_timed_workload_proof", "PERSISTENCE_INVALID")
  const counters = record(source.counts, COUNT_FIELDS, "PERSISTENCE_INVALID"), canary = record(source.canary, ["payload_bytes", "created", "full_bytes_verified", "eof_verified", "remove_confirmed", "logical_root_empty"], "PERSISTENCE_INVALID")
  requireThat(COUNT_FIELDS.every((field) => count(counters[field]) || field === "canary_eof_bytes_read" && counters[field] === null) && canary.payload_bytes === 4096 && Object.entries(canary).every(([key, value]) => key === "payload_bytes" || typeof value === "boolean"), "PERSISTENCE_INVALID")
  const observations = record(source.layout_observations, ["before_timed", "drained_original", "reopened", "removed_drained", "empty_reopened"], "PERSISTENCE_INVALID")
  Object.values(observations).forEach((value) => observation(value, layout, flags))
  try { if (source.preflight !== null) validateSplitNamespacePresenceReceipt(JSON.stringify(source.preflight)) } catch { reject("PERSISTENCE_INVALID") }
  if (completed) {
    const expected = { preflight_calls: 1, initial_create_calls: 1, reopen_create_calls: 2, original_shutdown_calls: 1, reopen_shutdown_calls: 2,
      canary_write_calls: 1, canary_bytes_written: 4096, canary_sync_calls: 1, canary_write_close_calls: 1, canary_read_calls: 1, canary_bytes_read: 4096,
      canary_eof_read_calls: 1, canary_eof_bytes_read: 0, canary_read_close_calls: 1, canary_remove_calls: 1, initial_empty_calls: 1, final_empty_calls: 1, layout_inspection_calls: 5 }
    requireThat(source.phase === "verified" && source.complete && source.failure_code === null && source.cleanup_codes.length === 0 && source.preflight !== null && source.original_shutdown_confirmed === true &&
      source.reopen_shutdown_confirmed_count === 2 && same(counters, expected) && Object.entries(canary).every(([key, value]) => key === "payload_bytes" || value === true) &&
      continuity(observations.before_timed, observations.drained_original) && continuity(observations.drained_original, observations.reopened, true) &&
      continuity(observations.reopened, observations.removed_drained) && continuity(observations.removed_drained, observations.empty_reopened, true), "PERSISTENCE_INVALID")
    if (layout === "compact") {
      const timed = { kind: "MRC5", receipt: benchmark?.providers?.[0]?.layoutSelection?.persistedReceipt }
      observation(timed, layout, flags)
      requireThat(continuity(observations.before_timed, timed) && continuity(timed, observations.drained_original), "PERSISTENCE_INVALID")
    }
  }
  return [construction.metadata_durable, construction.blocks_durable]
}
/** Check retained records only. No live endpoint, addon or build is executed. */
export function verifyOwnedLayoutComparison(inputs) {
  const report = freshReport()
  try {
    const input = record(inputs, [...INPUTS, "sources"], "INPUT_SHAPE_INVALID")
    const sources = record(input.sources, SOURCE_PATHS, "SOURCE_JOIN_INVALID")
    const bytes = Object.fromEntries(INPUTS.map((key) => [key, buffer(input[key], LIMITS[key])]))
    const seamBytes = Object.fromEntries(SOURCE_PATHS.map((path) => [path, buffer(sources[path], LIMITS.source)]))
    report.joins = { ...Object.fromEntries(INPUTS.map((key) => [`${key}_sha256`, sha(bytes[key])])), source_count: 28, seam_hashes_match: false }
    const parsed = Object.fromEntries(INPUTS.filter((key) => key !== "native").map((key) => [key, parse(bytes[key])]))
    const { projection, originals, fixtures, controller, engine, build } = parsed
    record(projection, ["schema", "runtime_scope", "failure_code", "publication", "comparison", "handoff", "originals", "identity", "build", "ownership", "qualification", "evidence_scope"])
    record(originals, ["schema", "runtime_scope", "joins", "evidence"])
    record(originals.joins, ["fixtures", "controller", "engine", "build", "native", "scope"])
    record(originals.evidence, ["schema", "complete", "arms", "scope"])
    requireThat(projection.schema === "mount-rs.owned-layout-entry.v1" && originals.schema === "mount-rs.owned-layout-originals.v1" &&
      ["production_entry_attempt", "modeled_controls"].includes(projection.runtime_scope) && projection.runtime_scope === originals.runtime_scope &&
      originals.evidence.schema === "mount-rs.owned-layout-private-evidence.v1" && typeof originals.evidence.complete === "boolean" &&
      originals.evidence.scope === "private_original_runner_input_for_independent_verification; public_native_metrics_are_separately_closed_projected_receipts" &&
      Array.isArray(originals.evidence.arms) && originals.evidence.arms.length <= 4, "SCHEMA_INVALID")
    report.runtime_scope = projection.runtime_scope
    for (const key of ["fixtures", "controller", "engine", "build"]) {
      const retained = record(originals.joins[key], ["value", "sha256"], "BYTE_JOIN_INVALID")
      requireThat(retained.sha256 === report.joins[`${key}_sha256`] && same(retained.value, parsed[key]), "BYTE_JOIN_INVALID")
    }
    requireThat(same(projection.originals, { schema: originals.schema, status: "retained", sha256: report.joins.originals_sha256, bytes: bytes.originals.length, count: originals.evidence.arms.length }), "BYTE_JOIN_INVALID")
    report.originals_retained = true
    record(build, ["schema", "checkout_sha", "source_clean", "locked", "release", "no_js", "build_exit_code", "native_sha256", "source_sha256"], "SOURCE_JOIN_INVALID")
    requireThat(build.schema === "mount-rs.owned-layout-native-build.v1" && /^[0-9a-f]{40}$/u.exec(build.checkout_sha)?.[0] === build.checkout_sha && build.source_clean === true &&
      build.locked === true && build.release === true && build.no_js === true && build.build_exit_code === 0 &&
      same(projection.build, { status: "verified", ...build, receipt_sha256: report.joins.build_sha256 }), "SOURCE_JOIN_INVALID")
    const sourceHashes = record(build.source_sha256, SOURCE_PATHS, "SOURCE_JOIN_INVALID")
    requireThat(SOURCE_PATHS.every((path) => sourceHashes[path] === sha(seamBytes[path])), "SOURCE_JOIN_INVALID")
    report.joins.seam_hashes_match = true
    const native = record(originals.joins.native, ["sha256", "selection", "kind", "native_used_identity", "profiling_enabled_before_load"], "NATIVE_JOIN_INVALID")
    requireThat(native.sha256 === report.joins.native_sha256 && build.native_sha256 === report.joins.native_sha256 && native.kind === "selected_native_file" &&
      typeof native.selection === "string" && posix.isAbsolute(native.selection) && posix.resolve(native.selection) === native.selection && native.selection.endsWith(".node") && !native.selection.includes("\0") && Buffer.byteLength(native.selection) <= 4096 &&
      ["verified", "unverified"].includes(native.native_used_identity) && native.profiling_enabled_before_load === true &&
      same(projection.identity, { kind: native.kind, native_sha256: native.sha256, native_used_identity: native.native_used_identity, profiling_enabled_before_load: true }) &&
      (projection.runtime_scope === "modeled_controls" ? native.native_used_identity === "unverified" : native.native_used_identity === "verified"), "NATIVE_JOIN_INVALID")
    const settings = handoff(fixtures, controller, engine, originals, projection, report.joins)
    const comparison = projection.comparison, reduced = comparison?.schema === "mount-rs.owned-layout-comparison-incomplete.v1"
    if (reduced) record(comparison, ["schema", "status", "complete", "comparable", "safe_to_continue", "floor_qualified", "native_uncertainty", "pending_owned_call_count",
      "completed_arms", "stopped_after", "stop_code", "required_evidence", "arms"])
    else record(comparison, ["schema", "status", "order", "budgets_ms", "completed_arms", "stopped_after", "stop_code", "native_uncertainty", "pending_owned_call_count",
      "complete", "comparable", "floor_qualified", "safe_to_continue", "arms", "evidence_scope", "retention", "output_cap_bytes"])
    requireThat(["complete", "stopped"].includes(comparison.status) && (reduced || comparison.schema === "mount-rs.owned-layout-comparison.v1") &&
      Array.isArray(comparison.arms) && comparison.arms.length > 0 && comparison.arms.length <= 4 && typeof comparison.native_uncertainty === "boolean" && count(comparison.pending_owned_call_count), "SCHEMA_INVALID")
    report.native_uncertainty = comparison.native_uncertainty
    requireThat(reduced || same(comparison.order, ORDER), "COHORT_INVALID")
    const captured = originals.evidence.arms, rawArms = []
    for (let index = 0; index < comparison.arms.length; index++) {
      const arm = comparison.arms[index]
      record(arm, reduced ? ["role", "layout", "status", "outcome"] : ["role", "layout", "status", "outcome", "persistence", "native_identity_verified", "fixture_identity_stable", "prepared",
        "persistence_verified", "comparison_safe_to_continue", "backing", "native_metrics"], "COHORT_INVALID")
      requireThat(arm.role === ORDER[index] && arm.layout === LAYOUTS[index], "COHORT_INVALID")
      const original = captured[index]
      if (original) {
        record(original, ["role", "cohort", "benchmark"], "COHORT_INVALID")
        const role = ORDER[index], scope = settings.scope
        requireThat(original.role === role && same(original.cohort, { id: `${scope.id}/${role}`, layout: LAYOUTS[index], metadataKey: `${scope.metadataPrefix}/${role}`,
          blockPrefix: `${scope.blockPrefix}/${role}`, owner: scope.owner }) && original.benchmark?.config?.layout === LAYOUTS[index], "COHORT_INVALID")
        rawArms.push(original.benchmark)
      } else requireThat(comparison.status === "stopped" && index === comparison.arms.length - 1 && arm.outcome === null && arm.status === "failed", "COHORT_INVALID")
    }
    requireThat(captured.length === rawArms.length && captured.length <= comparison.arms.length && comparison.arms.length - captured.length <= 1 &&
      (comparison.status !== "complete" || captured.length === 4 && comparison.arms.length === 4 && originals.evidence.complete === true), "COHORT_INVALID")
    const reassessed = rawArms.map((benchmark) => assessOwnedLayoutRunnerOutcome(benchmark))
    let uncertaintyRequired = comparison.pending_owned_call_count > 0 || STOP_CODES.has(comparison.stop_code) && comparison.stop_code.endsWith("_TIMEOUT") ||
      reassessed.some((outcome) => outcome.runner_safe_to_continue !== true && (outcome.native_quiescent !== true || outcome.owned_operations_settled !== true ||
        outcome.pending_operation_count !== 0 || outcome.timeout_count > 0 || outcome.original_safe_to_continue_pair !== true))
    if (uncertaintyRequired) report.native_uncertainty = true
    for (let index = 0; index < comparison.arms.length; index++) {
      const arm = comparison.arms[index], outcome = reassessed[index] || null
      report.arms.push({ role: ORDER[index], layout: LAYOUTS[index], status: outcome?.status || "failed", outcome, native_metrics: null, container_metrics: null })
      requireThat(same(arm.outcome, outcome) && arm.status === (outcome?.status || "failed"), "ORIGINAL_OUTCOME_MISMATCH")
    }
    report.checked_arm_count = reassessed.length
    report.floor_qualified = comparison.arms.length === 4 && reassessed.length === 4 && reassessed.every((outcome) => outcome.floor_qualified)
    let identityBaseline = null, durabilityBaseline = null
    const completedClaims = comparison.arms.map((arm) => !reduced && arm.comparison_safe_to_continue === true)
    for (let index = 0; index < comparison.arms.length; index++) {
      const arm = comparison.arms[index], original = rawArms[index]
      if (reduced || arm.backing === null) { requireThat(!completedClaims[index], "OBSERVER_INVALID"); continue }
      requireThat(original !== undefined, "OBSERVER_INVALID")
      let backing
      try {
        const source = original.providers[0].backingObserver
        requireThat(source.complete === true && source.backing_evidence?.complete === true && Array.isArray(source.issues) && source.issues.length === 0 &&
          source.backing_evidence.issues.length === 0 && source.backing_evidence.dropped_entries === 0 && source.events.length === 3 && source.backing_evidence.journal.length === 4, "OBSERVER_INVALID")
        const boundaries = source.backing_evidence.journal.filter((entry) => entry.type === "boundary")
        requireThat(boundaries.length === 2 && boundaries.every((boundary) => boundary.complete === true && Array.isArray(boundary.issues) && boundary.issues.length === 0 &&
          boundary.samples.every((sample) => hash(sample.inspect_body_sha256) && hash(sample.stats_body_sha256) && count(sample.stats_frame_bytes) && sample.stats_frame_bytes > 0 &&
            sample.stats_frame_bytes <= CAPS.maxResponseBytes && count(sample.stats_received_bytes) && sample.stats_received_bytes >= sample.stats_frame_bytes && sample.stats_received_bytes <= CAPS.maxResponseBytes)), "OBSERVER_INVALID")
        backing = observerProjection(source, settings.entries, reassessed[index].elapsed_ms)
        requireThat(same(arm.backing, backing), "OBSERVER_INVALID")
      } catch { reject("OBSERVER_INVALID") }
      const identities = backing.boundaries[0].samples.map((sample) => sample.identity)
      requireThat(identityBaseline === null || same(identities, identityBaseline), "FIXTURE_IDENTITY_INVALID")
      identityBaseline ||= identities
      report.arms[index].container_metrics = Object.fromEntries(METRICS.map((metric) => { const value = backing.interval.metrics[metric]
        return [metric, { complete: value.complete, total: value.total, partial_total: value.partial_total, missing_member_count: value.missing_members.length }] }))
    }
    if (!reduced) for (let index = 0; index < comparison.arms.length; index++) {
      const arm = comparison.arms[index], durable = persistence(arm.persistence, LAYOUTS[index], rawArms[index], completedClaims[index])
      const state = arm.persistence
      if (state !== null && (state.available !== true || state.counts.initial_create_calls > 0 && !state.original_shutdown_confirmed ||
          state.counts.reopen_create_calls > state.reopen_shutdown_confirmed_count)) {
        uncertaintyRequired = true; report.native_uncertainty = true
      }
      requireThat(durable === null || durabilityBaseline === null || same(durable, durabilityBaseline), "PERSISTENCE_INVALID")
      durabilityBaseline ||= durable
    }
    for (let index = 0; index < rawArms.length; index++) {
      const phases = rawArms[index].providers?.[0]?.storageDiagnostics?.phases?.filter((phase) => typeof phase?.name === "string" && phase.name.startsWith("workload-"))
      const nativeMetrics = projectOwnedLayoutPhaseMetrics(phases?.length === 1 ? phases[0] : null)
      requireThat(reduced || same(comparison.arms[index].native_metrics, nativeMetrics), "NATIVE_METRICS_MISMATCH")
      report.arms[index].native_metrics = nativeMetrics
    }
    // Original supported floor statuses are independent from enclosing safety.
    requireThat(!uncertaintyRequired || comparison.native_uncertainty === true, "CONTINUATION_INVALID")
    requireThat(count(comparison.completed_arms) && comparison.completed_arms <= 4 && typeof comparison.complete === "boolean" && typeof comparison.comparable === "boolean" &&
      typeof comparison.safe_to_continue === "boolean" && comparison.floor_qualified === report.floor_qualified, "CONTINUATION_INVALID")
    if (!reduced) {
      const budgets = record(comparison.budgets_ms, Object.keys(CEILINGS), "CONTINUATION_INVALID")
      requireThat(Object.entries(budgets).every(([phase, value]) => Number.isSafeInteger(value) && value > 0 && value <= CEILINGS[phase]) &&
        same(comparison.evidence_scope, COMPARISON_SCOPE) && comparison.output_cap_bytes === 33554432 && comparison.retention === "closed_bounded_redacted_projection_including_fixed_native_metrics; original_phase_records_private_for_independent_verification", "CONTINUATION_INVALID")
      for (let index = 0; index < comparison.arms.length; index++) {
        const arm = comparison.arms[index]
        requireThat(["native_identity_verified", "fixture_identity_stable", "prepared", "persistence_verified", "comparison_safe_to_continue"].every((key) => typeof arm[key] === "boolean"), "CONTINUATION_INVALID")
        if (completedClaims[index]) requireThat(reassessed[index]?.runner_safe_to_continue === true && report.arms[index].native_metrics?.status === "observed" && arm.prepared === true &&
          arm.native_identity_verified === true && arm.fixture_identity_stable === true && arm.persistence_verified === true && arm.backing !== null &&
          same(rawArms[index]?.providers?.[0]?.backingPilotIdentity, { native_used_identity: "verified", kind: "selected_native_file", native_sha256: native.sha256 }), "CONTINUATION_INVALID")
        else requireThat(index === comparison.arms.length - 1 && comparison.status === "stopped", "CONTINUATION_INVALID")
      }
      const countCompleted = completedClaims.filter(Boolean).length
      requireThat(comparison.completed_arms === countCompleted && completedClaims.every((value, index) => value === (index < countCompleted)), "CONTINUATION_INVALID")
    }
    const complete = !reduced && comparison.status === "complete" && comparison.arms.length === 4 && comparison.completed_arms === 4 &&
      comparison.native_uncertainty === false && comparison.pending_owned_call_count === 0 && comparison.stopped_after === null && comparison.stop_code === null
    if (comparison.status === "complete") requireThat(reduced || complete, "CONTINUATION_INVALID")
    else requireThat(comparison.complete === false && comparison.comparable === false && comparison.safe_to_continue === false &&
      comparison.stopped_after === ORDER[comparison.arms.length - 1] && STOP_CODES.has(comparison.stop_code) && originals.evidence.complete === false &&
      (!comparison.stop_code.endsWith("_TIMEOUT") || comparison.native_uncertainty === true), "CONTINUATION_INVALID")
    requireThat(comparison.complete === complete && comparison.comparable === complete && comparison.safe_to_continue === complete &&
      (comparison.pending_owned_call_count === 0 || comparison.native_uncertainty === true) && (!reduced || comparison.required_evidence === "omitted_output_cap_or_publication_failure"), "CONTINUATION_INVALID")
    const publication = record(projection.publication, ["status", "reason"], "PUBLICATION_INVALID"), qualification = record(projection.qualification, ["hosted", "floor_qualified", "comparable", "safe_to_continue", "reason"], "PUBLICATION_INVALID")
    requireThat(["complete", "incomplete"].includes(publication.status) && (projection.failure_code === null || ENTRY_CODES.has(projection.failure_code)) &&
      (publication.reason === null || ENTRY_CODES.has(publication.reason)) && qualification.hosted === false && qualification.reason === "owner_teardown_and_independent_verification_required", "PUBLICATION_INVALID")
    report.publication_complete = publication.status === "complete"
    requireThat(qualification.floor_qualified === report.floor_qualified && qualification.comparable === (report.publication_complete && complete) && qualification.safe_to_continue === (report.publication_complete && complete), "CONTINUATION_INVALID")
    requireThat(report.publication_complete ? projection.failure_code === null && publication.reason === null && !reduced : projection.failure_code !== null && publication.reason !== null, "PUBLICATION_INVALID")
    requireThat(same(projection.evidence_scope, { controller: "new_private_handoff_contract; producer_integration_not_established_here", source_and_build: "boundary_hash_and_clean_git_build_seal_observations; not_immutable_interval_proof",
      resource_floor: "unchanged_owned_controller_prerequisite; unobserved_here", execution: projection.runtime_scope === "modeled_controls" ? "pure_injected_model; not_live_or_performance_evidence" : "production_entry_attempt; qualification_requires_separate_verifier" }), "PUBLICATION_INVALID")
    report.comparable = report.safe_to_continue = report.publication_complete && complete
    report.status = report.comparable ? "consistent" : "incomplete"
    report.failure_codes = report.comparable ? [] : [code("RECORD_INCOMPLETE")]
  } catch (error) {
    report.status = "rejected"; report.comparable = report.safe_to_continue = false
    report.failure_codes = [issued.has(error) && CHECK_CODES.has(error.code.slice("OWNED_LAYOUT_CHECK_".length)) ? error.code : code("SCHEMA_INVALID")]
  }
  return freeze(report)
}
const STAMP_FIELDS = ["dev", "ino", "size", "mtimeNs", "ctimeNs"]
const stamp = (stat) => Object.fromEntries(STAMP_FIELDS.map((key) => [key, stat[key]]))
function fileCondition(condition) { requireThat(condition, "FILE_INVALID") }
async function privateParent(path) {
  const parent = await lstat(dirname(path), { bigint: true })
  fileCondition(parent.isDirectory() && parent.uid === BigInt(process.getuid()) && (parent.mode & 0o777n) === 0o700n)
}
async function readStable(path, maximum, privateJSON) {
  const before = await lstat(path, { bigint: true })
  fileCondition(before.isFile() && before.size > 0n && before.size <= BigInt(maximum))
  if (privateJSON) { fileCondition(before.uid === BigInt(process.getuid()) && before.nlink === 1n && (before.mode & 0o777n) === 0o600n); await privateParent(path) }
  const canonical = await realpath(path), handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  let bytes
  try {
    const opened = await handle.stat({ bigint: true })
    fileCondition(opened.isFile() && same(stamp(opened), stamp(before)))
    const buffer = Buffer.alloc(Number(before.size) + 1); let used = 0
    while (used < buffer.length) {
      const result = await handle.read(buffer, used, buffer.length - used, null)
      if (result.bytesRead === 0) break
      used += result.bytesRead
    }
    fileCondition(used === Number(before.size) && same(stamp(await handle.stat({ bigint: true })), stamp(before)))
    bytes = buffer.subarray(0, used)
  } finally { await handle.close() }
  const observation = { path, canonical, initial: stamp(before), maximum, privateJSON }
  await recheck(observation)
  return { bytes, observation }
}
async function recheck(observation) {
  const current = await lstat(observation.path, { bigint: true })
  fileCondition(current.isFile() && same(stamp(current), observation.initial) && await realpath(observation.path) === observation.canonical)
  if (observation.privateJSON) { fileCondition(current.uid === BigInt(process.getuid()) && current.nlink === 1n && (current.mode & 0o777n) === 0o600n); await privateParent(observation.path) }
}
async function sourcePath(root, source) {
  let candidate = root
  const parts = source.split("/")
  for (let index = 0; index < parts.length; index++) {
    candidate = join(candidate, parts[index]); const stat = await lstat(candidate, { bigint: true })
    fileCondition(!stat.isSymbolicLink() && (index === parts.length - 1 ? stat.isFile() : stat.isDirectory()))
  }
  const canonical = await realpath(candidate), rel = relative(root, canonical)
  fileCondition(rel === source.split("/").join(sep) && rel !== "" && rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel))
  return candidate
}
function argumentsRecord(argv) {
  requireThat(!types.isProxy(argv) && Array.isArray(argv) && Object.getPrototypeOf(argv) === Array.prototype, "ARGUMENTS_INVALID")
  const descriptors = Object.getOwnPropertyDescriptors(argv)
  requireThat(descriptors.length?.value === 16 && Reflect.ownKeys(descriptors).length === 17, "ARGUMENTS_INVALID")
  const names = [...INPUTS, "checkout"], paths = {}
  for (let index = 0; index < 16; index += 2) {
    const flag = descriptors[index], value = descriptors[index + 1]
    requireThat(flag && value && Object.hasOwn(flag, "value") && Object.hasOwn(value, "value") && typeof flag.value === "string" && typeof value.value === "string", "ARGUMENTS_INVALID")
    const key = flag.value.slice(2)
    requireThat(flag.value === `--${key}` && names.includes(key) && !Object.hasOwn(paths, key) && value.value.length > 0 && Buffer.byteLength(value.value) <= 4096 && !value.value.includes("\0"), "ARGUMENTS_INVALID")
    paths[key] = resolve(value.value)
  }
  return paths
}
async function readInputs(paths) {
  fileCondition(["linux", "darwin"].includes(process.platform) && typeof process.getuid === "function" && Number.isInteger(constants.O_NOFOLLOW) && Number.isInteger(constants.O_NONBLOCK))
  const root = await realpath(paths.checkout), rootStat = await lstat(root, { bigint: true })
  fileCondition(rootStat.isDirectory())
  const initialRoot = stamp(rootStat), inputs = {}, observed = [], canonicals = new Set(), identities = new Set()
  for (const key of INPUTS) {
    const read = await readStable(paths[key], LIMITS[key], key !== "native"), identity = `${read.observation.initial.dev}:${read.observation.initial.ino}`
    fileCondition(!canonicals.has(read.observation.canonical) && !identities.has(identity))
    canonicals.add(read.observation.canonical); identities.add(identity)
    inputs[key] = read.bytes; observed.push(read.observation)
  }
  inputs.sources = {}
  for (const source of SOURCE_PATHS) {
    const path = await sourcePath(root, source), read = await readStable(path, LIMITS.source, false)
    fileCondition(read.observation.canonical === path)
    inputs.sources[source] = read.bytes; observed.push(read.observation)
  }
  // This final read-set check is a boundary observation, not interval immutability.
  for (const observation of observed) await recheck(observation)
  fileCondition(await realpath(paths.checkout) === root && same(stamp(await lstat(root, { bigint: true })), initialRoot))
  for (const source of SOURCE_PATHS) await sourcePath(root, source)
  return inputs
}
export async function main(argv = process.argv.slice(2)) {
  let report
  try { const paths = argumentsRecord(argv); let inputs
    try { inputs = await readInputs(paths) } catch { reject("FILE_INVALID") }
    report = verifyOwnedLayoutComparison(inputs)
  } catch (error) {
    report = freshReport(); report.failure_codes = [issued.has(error) ? error.code : code("ARGUMENTS_INVALID")]; report = freeze(report)
  }
  process.stdout.write(`${JSON.stringify(report)}\n`)
  return report.status === "consistent" ? 0 : 1
}
if (import.meta.main) main().then((status) => { process.exitCode = status })
