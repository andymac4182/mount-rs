import { constants } from "node:fs"
import { open } from "node:fs/promises"
import { isDeepStrictEqual } from "node:util"
import { STORAGE_OPERATION_NAMES } from "../benchmarks/storage/diagnostics.mjs"
import { summarizeInterval } from "../benchmarks/storage/backing-observer.mjs"
import { resolve } from "node:path"
import { pathToFileURL } from "node:url"

export const SOURCE_PATHS = Object.freeze([
  "scripts/test-rustfs.sh", "scripts/test-tidb.sh", "benchmarks/storage/runner.mjs",
  "benchmarks/storage/owned-backing-pilot.mjs", "benchmarks/storage/owned-backing-pilot.test.mjs",
  "benchmarks/storage/README.md",
])
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value)
const hash = (value) => typeof value === "string" && /^[a-f0-9]{64}$/u.test(value)
function reject(reason = "invalid_evidence") {
  throw Object.assign(new Error(`pilot_evidence_rejected: ${reason}`), { code: "pilot_evidence_rejected", reason })
}
function requireEvidence(condition, reason) { if (!condition) reject(reason) }


// Match the committed pilot projection. Counters remain decimal strings.
const PROFILE_NAMES = [
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
  "provider.inode_serialized_bytes"
]
const RAW_NAMES = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const CLAIMS = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]
const ROLES = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const decimal = (value) => typeof value === "string" && /^(0|[1-9]\d{0,19})$/u.test(value) && BigInt(value) <= 18446744073709551615n
const duration = (value) => Number.isFinite(value) && value >= 0
const counters = (value, keys) => object(value) && keys.every((key) => decimal(value[key]))
const empty = (value) => Array.isArray(value) && value.length === 0
const terminal = (value) => value?.status === "ok" && value.native_quiescent === true && value.owned_operations_settled === true &&
  value.native_profiling_enabled === true && value.workload_native_evidence_complete === true && value.cleanup_complete === true &&
  value.operation_deadline_failed === false && value.prior_native_uncertainty === false && value.safe_to_continue_pair === true
function rows(entries, names, fields, histogram = false) {
  if (!Array.isArray(entries) || entries.length !== names.length || !names.every((name) => entries.filter((row) => row?.name === name).length === 1)) return false
  return entries.every((row) => {
    if (!counters(row, fields)) return false
    if (!histogram) return true
    return Array.isArray(row.latency_log2_us) && row.latency_log2_us.length === 32 && row.latency_log2_us.every(decimal) &&
      BigInt(row.calls) === BigInt(row.success) + BigInt(row.error) + BigInt(row.cancelled) &&
      row.latency_log2_us.reduce((sum, value) => sum + BigInt(value), 0n) === BigInt(row.calls)
  })
}
function nativeCounters(workload, elapsed) {
  requireEvidence(duration(workload.elapsed_ms) && workload.elapsed_ms > 0 && duration(workload.observer_snapshot_ms) && workload.benchmark_measured_elapsed_ms === elapsed, "native_evidence")
  requireEvidence(rows(workload.core_profile.entries, PROFILE_NAMES, ["calls", "elapsed_ns", "units"]) &&
    rows(workload.storage, STORAGE_OPERATION_NAMES, ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"], true) &&
    workload.storage.every((row) => row.in_flight_start === "0" && row.in_flight_end === "0") &&
    workload.storage.some((row) => BigInt(row.calls) > 0n), "native_evidence")
  const instances = workload.raw_instances
  requireEvidence(Array.isArray(instances) && instances.length === workload.raw_projection.instance_count &&
    new Set(instances.map((instance) => instance?.id)).size === instances.length && instances.every((instance) =>
      decimal(instance.id) && counters(instance.claims, CLAIMS) && decimal(instance.cache_hits) && decimal(instance.conditional_conflicts) &&
      rows(instance.entries, RAW_NAMES, ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end"], true)), "native_evidence")
  requireEvidence(counters(workload.process?.cpu_work, ["user_us", "system_us"]) && counters(workload.process?.cpu_observer, ["user_us", "system_us"]) &&
    counters(workload.process?.memory_end_bytes, ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"]) &&
    counters(workload.process?.resources_work, ["voluntary_context_switches", "involuntary_context_switches"]), "native_evidence")
}
function backingCounters(backing, boundaries, interval, elapsed) {
  const evidence = backing.backing_evidence, allowlist = evidence.allowlist
  requireEvidence(evidence.schema === "mount-rs.backing-observer.v1" && terminal(backing.terminal) && terminal(evidence.terminal) &&
    empty(backing.issues) && empty(evidence.issues) && evidence.dropped_entries === 0 &&
    Array.isArray(backing.events) && backing.events.length === 3 && ["beginPhase", "endPhase", "finalize"].every((hook, index) =>
      backing.events[index]?.hook === hook && backing.events[index]?.status === "ok"), "resource_evidence")
  requireEvidence(Array.isArray(allowlist) && allowlist.length === ROLES.length && ROLES.every((role) => allowlist.filter((entry) => entry?.role === role).length === 1) &&
    allowlist.every((entry) => hash(entry.cid) && object(entry.labels) && Object.keys(entry.labels).length > 0) && new Set(allowlist.map((entry) => entry.cid)).size === ROLES.length, "resource_evidence")
  for (const boundary of boundaries) {
    requireEvidence(empty(boundary.issues) && Array.isArray(boundary.samples) && boundary.samples.length === allowlist.length &&
      allowlist.every((entry) => boundary.samples.filter((sample) => sample?.cid === entry.cid).length === 1), "resource_evidence")
    for (const sample of boundary.samples) {
      const entry = allowlist.find((entry) => entry.cid === sample.cid), identity = sample.identity
      requireEvidence(identity?.cid === entry.cid && identity.role === entry.role && isDeepStrictEqual(identity.labels, entry.labels) && identity.running === true &&
        typeof identity.image === "string" && /^sha256:[a-f0-9]{64}$/u.test(identity.image) && typeof identity.started_at === "string" && Number.isFinite(Date.parse(identity.started_at)) && decimal(identity.restart_count) &&
        sample.stats?.cid === entry.cid && decimal(sample.stats.read_ns) && hash(sample.inspect_body_sha256) && hash(sample.stats_body_sha256) &&
        [sample.inspect_window, sample.stats_window].every((window) => duration(window?.dispatch_ms) && duration(window?.response_ms) && window.response_ms >= window.dispatch_ms), "resource_evidence")
    }
  }
  requireEvidence(interval.schema === "mount-rs.backing-interval.v1" && interval.kind === "phase" && interval.native_quiescent === true &&
    interval.owned_operations_settled === true && interval.native_evidence_state === "complete" && interval.workload_elapsed_ms === elapsed, "resource_evidence")
  let computed
  try { computed = summarizeInterval(allowlist, boundaries[0], boundaries[1], { workload_elapsed_ms: elapsed }) } catch { reject("resource_evidence") }
  requireEvidence(computed.complete === true && ["containers", "metrics", "endpoints"].every((key) => isDeepStrictEqual(interval[key], computed[key])), "resource_evidence")
}

/** Validate the sanitized owner's record; this does not create missing evidence. */
export function verifyPilot(pilot, build) {
  const buildKeys = ["schema", "checkout_sha", "source_clean", "locked", "release", "no_js", "build_exit_code", "native_sha256", "source_sha256"]
  requireEvidence(object(build) && Object.keys(build).length === buildKeys.length && buildKeys.every((key) => Object.hasOwn(build, key)), "build_receipt")
  requireEvidence(build.schema === "mount-rs.hosted-native-build.v1" && typeof build.checkout_sha === "string" && /^[a-f0-9]{40}$/u.test(build.checkout_sha) &&
    build.source_clean === true && build.locked === true && build.release === true && build.no_js === true && build.build_exit_code === 0 && hash(build.native_sha256), "build_receipt")
  requireEvidence(object(pilot) && pilot.schema === "mount-rs.owned-backing-pilot.v1" && pilot.provider === "mount-rs-split-tidb-r2", "pilot_schema")
  requireEvidence(pilot.identity?.kind === "selected_native_file" && pilot.native_used_identity === "verified" &&
    pilot.owned_endpoint_binding === "verified" && pilot.identity.native_sha256 === build.native_sha256, "native_identity")
  requireEvidence(object(build.source_sha256) && Object.keys(build.source_sha256).length === SOURCE_PATHS.length &&
    object(pilot.identity.source_sha256) && Object.keys(pilot.identity.source_sha256).length === SOURCE_PATHS.length &&
    SOURCE_PATHS.every((path) => hash(build.source_sha256[path]) && pilot.identity.source_sha256[path] === build.source_sha256[path]), "source_identity")
  const config = pilot.config
  requireEvidence(config?.layout === "legacy" && config.workload === "lifecycle" && config.payload_bytes === 4096 &&
    config.iterations === 400 && config.concurrency === 64 && config.chunk_bytes === 65536 && config.minimum_iops === 1000, "workload_dimensions")
  const outcome = pilot.outcome
  requireEvidence(outcome?.status === "ok" && outcome.provider_status === "ok" && outcome.result_status === "ok" && outcome.first_failure === null &&
    Number.isFinite(outcome.elapsed_ms) && outcome.elapsed_ms > 0 && Number.isFinite(outcome.iops) && outcome.iops >= 1000 &&
    Math.abs(outcome.iops - 1200000 / outcome.elapsed_ms) <= 1e-9 * Math.max(1, outcome.iops) && outcome.native_quiescent === true && outcome.owned_operations_settled === true && outcome.iops_target === 1000 && outcome.iops_target_met === true &&
    outcome.successful_iterations === 400 && outcome.failed_iterations === 0 && outcome.successful_operations === 1200 && outcome.attempted_operations === 1200 &&
    outcome.verified_reads === 400 && outcome.timeout_count === 0 && outcome.cleanup_failure_count === 0 &&
    outcome.remaining_paths === 0 && outcome.path_cleanup_failures === 0 && outcome.resource_cleanup_status === "ok", "workload_or_cleanup")
  const phases = pilot.native_phases?.phases
  requireEvidence(pilot.native_phases?.status === "observed" && Array.isArray(phases), "native_evidence")
  const workloads = phases.filter((phase) => object(phase) && phase.name === "workload-4096bytes")
  requireEvidence(workloads.length === 1, "native_evidence")
  const workload = workloads[0]
  requireEvidence(workload.status === "observed" && workload.validated_workload === true && workload.projected_component_complete === true && workload.quiescent === true &&
    workload.core_profile?.status === "observed" && workload.raw_projection?.status === "observed" &&
    Number.isSafeInteger(workload.raw_projection.instance_count) && workload.raw_projection.instance_count > 0 && workload.raw_projection.instance_count <= 16 &&
    workload.raw_projection.omitted_instances === 0, "native_evidence")
  nativeCounters(workload, outcome.elapsed_ms)
  const backing = pilot.backing
  requireEvidence(backing?.schema === "mount-rs.runner-backing-observer.v1" && backing.complete === true && backing.backing_evidence?.complete === true &&
    backing.terminal?.safe_to_continue_pair === true && pilot.resource_coverage?.["workload-4096bytes"] === "captured", "resource_evidence")
  const journal = backing.backing_evidence.journal
  requireEvidence(Array.isArray(journal) && journal.every(object), "resource_evidence")
  const boundaries = journal.filter((entry) => entry.type === "boundary")
  const intervals = journal.filter((entry) => entry.type === "interval")
  requireEvidence(boundaries.length === 2 && boundaries[0].id === "workload-4096bytes:begin" && boundaries[1].id === "workload-4096bytes:end" &&
    boundaries.every((entry) => entry.complete === true) && intervals.length === 1 && intervals[0].complete === true &&
    pilot.observation_status === "observed" && pilot.live_measurement_qualified === true, "resource_evidence")
  backingCounters(backing, boundaries, intervals[0], outcome.elapsed_ms)
  return { schema: "mount-rs.hosted-backing-check.v1", status: "verified", checkout_sha: build.checkout_sha,
    native_sha256: build.native_sha256, verified_reads: outcome.verified_reads, successful_operations: outcome.successful_operations, iops: outcome.iops }
}

export async function readEvidence(path, maximum) {
  try {
    const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW)
    try {
      const before = await file.stat()
      requireEvidence(before.isFile() && before.size > 0 && before.size <= maximum, "evidence_file")
      const buffer = Buffer.alloc(before.size + 1)
      let used = 0
      while (used < buffer.length) {
        const { bytesRead } = await file.read(buffer, used, buffer.length - used, null)
        if (bytesRead === 0) break
        used += bytesRead
      }
      const after = await file.stat()
      requireEvidence(used === before.size && after.size === before.size && after.mtimeMs === before.mtimeMs && after.ctimeMs === before.ctimeMs, "evidence_file")
      return JSON.parse(buffer.subarray(0, used).toString("utf8"))
    } finally { await file.close() }
  } catch { reject("evidence_file") }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    requireEvidence(process.argv.length === 4, "arguments")
    const pilot = await readEvidence(process.argv[2], 8_388_608)
    const build = await readEvidence(process.argv[3], 16_384)
    process.stdout.write(`${JSON.stringify(verifyPilot(pilot, build))}\n`)
  } catch (error) {
    process.stderr.write(`${JSON.stringify({ schema: "mount-rs.hosted-backing-check.v1", status: "rejected", reason: error.code === "pilot_evidence_rejected" ? error.reason : "invalid_evidence" })}\n`)
    process.exitCode = 1
  }
}
