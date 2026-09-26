import { createHash, randomBytes } from "node:crypto"
import { constants } from "node:fs"
import { open, lstat, rename, unlink } from "node:fs/promises"
import { createRequire } from "node:module"
import { dirname, isAbsolute, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { createBackingObserver } from "./backing-observer.mjs"
import { createBackingEngineTransport } from "./backing-engine-transport.mjs"
import { STORAGE_OPERATION_NAMES, validateRawPhaseDiagnostics } from "./diagnostics.mjs"

export const OUTPUT_CAP = 8_388_608
export const OUTCOME_RESERVE = 16_384
const INPUT_CAP = 16_384
const ADDON_CAP = 128 * 1024 * 1024
const directory = dirname(fileURLToPath(import.meta.url))
const repo = resolve(directory, "../..")
const capturePath = join(directory, "capture-native.cjs")
const bindingPath = join(repo, "bindings/mount-rs-napi/index.js")
const require = createRequire(import.meta.url)
const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
// Closed committed HEAD13557 core labels; protected uncommitted compact
// clone/COW rows are excluded and do not determine required coverage.
const profileNames = [
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
const phaseNames = ["create", "workload-4096bytes", "cleanup", "shutdown"]
const sources = ["scripts/test-rustfs.sh", "scripts/test-tidb.sh", "benchmarks/storage/runner.mjs", "benchmarks/storage/owned-backing-pilot.mjs", "benchmarks/storage/owned-backing-pilot.test.mjs", "benchmarks/storage/README.md"]
const fail = (code) => { throw Object.assign(new Error(code), { code }) }
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value)
function exact(value, keys, code) {
  if (!object(value) || Object.keys(value).length !== keys.length || !keys.every((key) => Object.hasOwn(value, key))) fail(code)
}
function safeName(value, code = "pilot_handoff_rejected") {
  if (typeof value !== "string" || !/^[A-Za-z0-9_.:-]{1,120}$/u.test(value)) fail(code)
  return value
}
const currentUid = () => typeof process.getuid === "function" ? process.getuid() : fail("pilot_private_file_rejected")
async function privateParent(path) {
  if (typeof path !== "string" || !isAbsolute(path) || path.includes("\0")) fail("pilot_private_file_rejected")
  const stat = await lstat(dirname(path))
  if (!stat.isDirectory() || stat.uid !== currentUid() || (stat.mode & 0o777) !== 0o700) fail("pilot_private_file_rejected")
}
async function boundedRead(path, maximum, privateFile = false) {
  let file
  try {
    if (privateFile) await privateParent(path)
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW)
    const first = await file.stat()
    if (!first.isFile() || first.size < 1 || first.size > maximum || privateFile &&
        (first.uid !== currentUid() || (first.mode & 0o777) !== 0o600 || first.nlink !== 1)) fail("pilot_private_file_rejected")
    const bytes = Buffer.alloc(first.size + 1)
    let used = 0
    while (used < bytes.length) {
      const { bytesRead } = await file.read(bytes, used, bytes.length - used, null)
      if (bytesRead === 0) break
      used += bytesRead
    }
    const last = await file.stat()
    if (used !== first.size || last.size !== first.size || last.mtimeMs !== first.mtimeMs || last.ctimeMs !== first.ctimeMs) fail("pilot_private_file_rejected")
    return bytes.subarray(0, used)
  } catch { fail("pilot_private_file_rejected") }
  finally { await file?.close() }
}
async function privateJSON(path) {
  try { return JSON.parse((await boundedRead(path, INPUT_CAP, true)).toString("utf8")) }
  catch { fail("pilot_handoff_rejected") }
}
async function privateWrite(path, bytes) {
  await privateParent(path)
  try {
    const existing = await lstat(path)
    if (!existing.isFile() || existing.uid !== currentUid() || (existing.mode & 0o777) !== 0o600 || existing.nlink !== 1) fail("pilot_private_file_rejected")
  } catch (error) { if (error.code !== "ENOENT") throw error }
  const temporary = join(dirname(path), `.mount-rs-pilot-${randomBytes(12).toString("hex")}.tmp`)
  let file
  try {
    file = await open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600)
    await file.writeFile(bytes)
    await file.close(); file = null
    await rename(temporary, path)
  } finally { await file?.close(); await unlink(temporary).catch(() => {}) }
}

function validateEntries(entries, tidbOwner, rustfsOwner, expectedRoles) {
  if (!Array.isArray(entries) || entries.length !== expectedRoles.length) fail("pilot_handoff_rejected")
  const cids = new Set()
  return expectedRoles.map((role) => {
    const matches = entries.filter((entry) => entry?.role === role)
    if (matches.length !== 1) fail("pilot_handoff_rejected")
    const entry = matches[0]
    exact(entry, ["cid", "role", "labels"], "pilot_handoff_rejected")
    if (typeof entry.cid !== "string" || !/^[a-f0-9]{64}$/u.test(entry.cid) || cids.has(entry.cid)) fail("pilot_handoff_rejected")
    cids.add(entry.cid)
    const labels = role === "rustfs-service"
      ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": rustfsOwner }
      : { "mount-rs.tidb.run": tidbOwner }
    exact(entry.labels, Object.keys(labels), "pilot_handoff_rejected")
    if (Object.entries(labels).some(([key, value]) => entry.labels[key] !== value)) fail("pilot_handoff_rejected")
    return { cid: entry.cid, role, labels }
  })
}
const effective = (environment, primary, secondary) => environment[primary] || environment[secondary]
export function validateHandoff(receipt, environment) {
  exact(receipt, ["schema", "tidb_owner", "rustfs_owner", "generation", "entries"], "pilot_handoff_rejected")
  if (receipt.schema !== "mount-rs.owned-backing-cids.v1" || receipt.generation !== "1" ||
      receipt.generation !== environment.MOUNT_RS_BACKING_GENERATION ||
      safeName(receipt.tidb_owner) !== environment.MOUNT_RS_BACKING_TIDB_OWNER ||
      safeName(receipt.rustfs_owner) !== environment.MOUNT_RS_BACKING_RUSTFS_OWNER) fail("pilot_handoff_rejected")
  const entries = validateEntries(receipt.entries, receipt.tidb_owner, receipt.rustfs_owner, roles)
  for (const [primary, secondary, expected] of [
    ["MOUNT_RS_TIDB_URL", "TIDB_URL", "MOUNT_RS_BACKING_EXPECT_TIDB_URL"],
    ["MOUNT_RS_R2_ENDPOINT", "R2_ENDPOINT", "MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT"],
    ["MOUNT_RS_R2_BUCKET", "R2_BUCKET", "MOUNT_RS_BACKING_EXPECT_R2_BUCKET"],
  ]) if (!environment[expected] || effective(environment, primary, secondary) !== environment[expected]) fail("pilot_endpoint_binding_rejected")
  // Owner-generated expectations must describe loopback services, never an
  // externally selected database/blob service that happens to share labels.
  try {
    const tidb = new URL(environment.MOUNT_RS_BACKING_EXPECT_TIDB_URL)
    const r2 = new URL(environment.MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT)
    if (tidb.protocol !== "mysql:" || tidb.hostname !== "127.0.0.1" || !tidb.port ||
        r2.protocol !== "http:" || r2.hostname !== "127.0.0.1" || !r2.port || r2.username || r2.password || r2.search || r2.hash || r2.pathname !== "/") fail("pilot_endpoint_binding_rejected")
  } catch { fail("pilot_endpoint_binding_rejected") }
  return { ...receipt, entries }
}

export async function preflightIdentity(environment, { mock = false } = {}) {
  const selection = environment.NAPI_RS_NATIVE_LIBRARY_PATH
  if (typeof selection !== "string" || !isAbsolute(selection) ||
      environment.NAPI_RS_FORCE_WASI || environment.NAPI_RS_WASI_FLAVOR || environment.NODE_PATH ||
      require.cache[bindingPath] || !mock && require.cache[selection] || (!mock && (environment.MOUNT_RS_PROFILE_IO !== "1" || environment.MOUNT_RS_TRACE_STORAGE === "1"))) fail("pilot_native_selection_rejected")
  if (mock ? selection !== capturePath : !selection.endsWith(".node")) fail("pilot_native_selection_rejected")
  let bytes
  try { bytes = await boundedRead(selection, mock ? 2 * 1024 * 1024 : ADDON_CAP) }
  catch { fail("pilot_native_selection_rejected") }
  const sha256 = createHash("sha256").update(bytes).digest("hex")
  const sourceHashes = {}
  for (const source of sources) sourceHashes[source] = createHash("sha256").update(await boundedRead(join(repo, source), 2 * 1024 * 1024)).digest("hex")
  return Object.freeze({ selection, sha256, mock, public: Object.freeze({ kind: mock ? "capture_js_mock" : "selected_native_file", native_sha256: mock ? null : sha256,
    mock_source_sha256: mock ? sha256 : null, real_native_proof: "unverified", source_sha256: sourceHashes, source_is_binary_build_revision: "unavailable" }) })
}

/** Read existing cache entries only, immediately after the accepted constructor.
 * The metadata loader is deliberately not invoked to manufacture runtime proof. */
export async function observePilotBinding(identity) {
  if (identity.mock) return { native_used_identity: "unverified", kind: "capture_js_mock", native_sha256: null }
  try {
    const binding = require.cache[bindingPath], selected = require.cache[identity.selection]
    if (!binding?.loaded || !selected?.loaded || binding.exports !== selected.exports ||
        binding.exports.__napiBindingTarget !== "native" || process.env.NAPI_RS_NATIVE_LIBRARY_PATH !== identity.selection) fail("pilot_native_join_unverified")
    const digest = createHash("sha256").update(await boundedRead(identity.selection, ADDON_CAP)).digest("hex")
    if (digest !== identity.sha256) fail("pilot_native_join_unverified")
    return { native_used_identity: "verified", kind: "selected_native_file", native_sha256: digest }
  } catch { return { native_used_identity: "unverified", kind: "selected_native_file", native_sha256: null } }
}

const count = (value) => Number.isSafeInteger(value) && value >= 0 ? value : null
const duration = (value) => Number.isFinite(value) && value >= 0 ? value : null
const decimal = (value) => typeof value === "string" && /^(0|[1-9]\d{0,19})$/u.test(value) && BigInt(value) <= 18446744073709551615n ? value : null
const status = (value) => ["ok", "failed", "skipped"].includes(value) ? value : "not_started"
function firstFailure(result, provider) {
  const safeOperation = (value) => ["write", "read", "delete", "cleanup", "setup", "benchmark", "iops-target", "oracle-revision"].includes(value) ? value : "provider"
  // Sample errors are recorded before runSize appends the post-sample floor.
  const errors = [...(result?.rawSamples || []).flatMap((sample) => sample.errors || []), ...(result?.failures || [])]
  if (provider?.setupError) return { code: "provider_setup_failed", operation: "setup" }
  if (errors.length) {
    const operation = safeOperation(errors[0].operation)
    return { code: operation === "iops-target" ? "iops_target_not_met" : "provider_operation_failed", operation }
  }
  if (provider?.cleanup?.failures?.length || provider?.cleanup?.remainingPaths > 0 || object(provider?.cleanup?.resource) && provider.cleanup.resource.status !== "ok") return { code: "cleanup_failed", operation: "cleanup" }
  if (status(provider?.status) === "failed" || status(result?.status) === "failed") return { code: "provider_operation_failed", operation: "provider" }
  return null
}
function projectOutcome(benchmark) {
  const result = benchmark.results?.[0], provider = benchmark.providers?.[0], summary = result?.summary
  return { status: status(benchmark.status), provider_status: status(provider?.status), result_status: status(result?.status), first_failure: firstFailure(result, provider),
    elapsed_ms: duration(summary?.elapsedMs), iops: duration(summary?.iops), iops_target: summary?.iopsTarget === 1000 ? 1000 : null, iops_target_met: typeof summary?.iopsTargetMet === "boolean" ? summary.iopsTargetMet : null,
    successful_iterations: count(summary?.successfulIterations), failed_iterations: count(summary?.failedIterations), successful_operations: count(summary?.successfulOperations), attempted_operations: count(summary?.attemptedOperations),
    verified_reads: count(summary?.operationSuccess?.verifiedReads), timeout_count: count(summary?.timeoutCount), cleanup_failure_count: count(summary?.cleanupFailureCount),
    paths_attempted: count(provider?.cleanup?.pathsAttempted), remaining_paths: count(provider?.cleanup?.remainingPaths), path_cleanup_failures: Array.isArray(provider?.cleanup?.failures) ? provider.cleanup.failures.length : null,
    resource_cleanup_status: ["ok", "failed", "pending", "deferred"].includes(provider?.cleanup?.resource?.status) ? provider.cleanup.resource.status : "not_started",
    native_quiescent: typeof provider?.backingObserver?.terminal?.native_quiescent === "boolean" ? provider.backingObserver.terminal.native_quiescent : null,
    owned_operations_settled: typeof provider?.backingObserver?.terminal?.owned_operations_settled === "boolean" ? provider.backingObserver.terminal.owned_operations_settled : null }
}
function projectNativePhases(phases) {
  if (!Array.isArray(phases)) return { status: "unavailable", phases: [] }
  return { status: "observed", scope: "inclusive_nested_provider_driver_counters; not_sql_wire_or_physical_iops", phases: phaseNames.map((name) => {
    const phase = phases.find((item) => item.name === name)
    if (!phase) return { name, status: "not_started" }
    let valid = false
    if (name === "workload-4096bytes") { try { validateRawPhaseDiagnostics(phase); valid = true } catch {} }
    const fields = ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"]
    const storage = STORAGE_OPERATION_NAMES.map((operation) => {
      const row = phase.native?.storage?.entries?.find((entry) => entry.name === operation)
      return { name: operation, ...Object.fromEntries(fields.map((field) => [field, decimal(row?.[field])])), latency_log2_us: Array.from({ length: 32 }, (_, index) => decimal(row?.latency_log2_us?.[index])) }
    })
    const rawFields = ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end"]
    const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
    const claims = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]
    const allInstances = Array.isArray(phase.native?.r2?.instances) ? phase.native.r2.instances : null
    const rawProjection = { status: allInstances === null ? "unavailable" : allInstances.length > 16 ? "truncated_unavailable" : "observed",
      instance_count: allInstances === null ? null : allInstances.length,
      omitted_instances: allInstances === null ? null : Math.max(0, allInstances.length - 16) }
    const instances = (allInstances?.slice(0, 16) || []).map((instance) => ({ id: decimal(instance.id),
      claims: Object.fromEntries(claims.map((field) => [field, decimal(instance.raw_api?.claims?.[field])])),
      cache_hits: decimal(instance.cache_hits), conditional_conflicts: decimal(instance.conditional_conflicts),
      entries: rawNames.map((operation) => { const row = instance.raw_api?.entries?.find((entry) => entry.name === operation)
        return { name: operation, ...Object.fromEntries(rawFields.map((field) => [field, decimal(row?.[field])])), exact_phase_max_ns: "unavailable", latency_log2_us: Array.from({ length: 32 }, (_, index) => decimal(row?.latency_log2_us?.[index])) } }) }))
    const profileRows = Array.isArray(phase.native?.profile?.entries) ? phase.native.profile.entries : []
    const duplicateProfileLabels = profileNames.some((name) => profileRows.filter((entry) => entry.name === name).length > 1)
    const profile = profileNames.map((name) => {
      const matches = profileRows.filter((entry) => entry.name === name)
      const row = matches.length === 1 ? matches[0] : null
      return { name, calls: decimal(row?.calls), elapsed_ns: decimal(row?.elapsed_ns), units: decimal(row?.units) }
    })
    const profileStatus = duplicateProfileLabels ? "unavailable_duplicate_required_labels" : profile.every((row) => row.calls !== null && row.elapsed_ns !== null && row.units !== null) ? "observed" : "unavailable"
    const numericMap = (input, keys) => Object.fromEntries(keys.map((key) => [key, decimal(input?.[key])]))
    const processCounters = {
      scope: "node_process_only; work_and_snapshot_observer_deltas_separate; memory_terminal_gauges",
      cpu_work: numericMap(phase.process?.cpu_work, ["user_us", "system_us"]),
      cpu_observer: numericMap(phase.process?.cpu_observer, ["user_us", "system_us"]),
      memory_end_bytes: numericMap(phase.process?.memory_end_bytes, ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"]),
      resources_work: numericMap(phase.process?.resources_work, ["voluntary_context_switches", "involuntary_context_switches"]),
      native_allocation_count: "unavailable", js_allocation_count: "unavailable",
    }
    const componentComplete = valid && profileStatus === "observed" &&
      Object.values(processCounters.cpu_work).every((value) => value !== null) && Object.values(processCounters.cpu_observer).every((value) => value !== null) &&
      Object.values(processCounters.memory_end_bytes).every((value) => value !== null) && Object.values(processCounters.resources_work).every((value) => value !== null) &&
      rawProjection.status === "observed"
    return { name, projected_component_complete: componentComplete, status: phase.native?.complete === true ? "observed" : "incomplete", validated_workload: valid,
      elapsed_ms: duration(phase.elapsed_ms), benchmark_measured_elapsed_ms: duration(phase.benchmark_measured_elapsed_ms), observer_snapshot_ms: duration(phase.observer_snapshot_ms), quiescent: phase.quiescent === true,
      process: processCounters, core_profile: { status: profileStatus, scope: "existing_fixed_core_counters; inclusive_elapsed_and_event_specific_units", entries: profile }, storage, raw_projection: rawProjection, raw_instances: instances,
      unknown: ["metadata_payload_bytes", "sql_wire_bytes", "physical_device_iops", "internal_http_retries", "pool_queue_only_wait"] }
  }) }
}

export function projectPilotRecord(benchmark, identity) {
  const provider = benchmark.providers?.[0]
  const backing = provider?.backingObserver
  const digest = (value) => typeof value === "string" && /^[a-f0-9]{64}$/u.test(value) ? value : null
  const publicIdentity = { kind: ["capture_js_mock", "selected_native_file"].includes(identity.kind) ? identity.kind : "unavailable",
    native_sha256: identity.kind === "selected_native_file" ? digest(identity.native_sha256) : null,
    mock_source_sha256: identity.kind === "capture_js_mock" ? digest(identity.mock_source_sha256) : null,
    real_native_proof: "unverified", source_is_binary_build_revision: "unavailable",
    source_sha256: Object.fromEntries(sources.map((source) => [source, digest(identity.source_sha256?.[source])])) }
  const joined = provider?.backingPilotIdentity?.native_used_identity === "verified" && publicIdentity.kind === "selected_native_file" &&
    publicIdentity.native_sha256 !== null && provider.backingPilotIdentity.native_sha256 === publicIdentity.native_sha256
  const outcome = projectOutcome(benchmark)
  let nativePhases
  try { nativePhases = projectNativePhases(provider?.storageDiagnostics?.phases) }
  catch { nativePhases = { status: "unavailable", phases: [], issue: "projection_unavailable" } }
  const validatedWorkload = nativePhases.phases.some((phase) => phase.name === "workload-4096bytes" && phase.validated_workload === true && phase.projected_component_complete === true)
  const coverage = Object.fromEntries(phaseNames.map((name) => [name, ["not_selected", "not_started", "captured", "incomplete"].includes(provider?.backingResourceCoverage?.[name]) ? provider.backingResourceCoverage[name] : name === "workload-4096bytes" ? "not_started" : "not_selected"]))
  const journal = Array.isArray(backing?.backing_evidence?.journal) ? backing.backing_evidence.journal : []
  const boundaries = journal.filter((entry) => entry.type === "boundary")
  const intervals = journal.filter((entry) => entry.type === "interval")
  const resourceComplete = backing?.schema === "mount-rs.runner-backing-observer.v1" && backing.complete === true && backing?.backing_evidence?.complete === true && coverage["workload-4096bytes"] === "captured" &&
    boundaries.length === 2 && boundaries[0].id === "workload-4096bytes:begin" && boundaries[1].id === "workload-4096bytes:end" && boundaries.every((entry) => entry.complete === true) &&
    intervals.length === 1 && intervals[0].complete === true
  return { schema: "mount-rs.owned-backing-pilot.v1", provider: "mount-rs-split-tidb-r2", run_id: typeof benchmark.runId === "string" && /^[A-Za-z0-9_.:-]{1,120}$/u.test(benchmark.runId) ? benchmark.runId : null,
    identity: publicIdentity, native_used_identity: joined ? "verified" : "unverified", owned_endpoint_binding: "verified", scope: "selected_workload_enclosing_container_window; native_phases_separate",
    config: { layout: "legacy", workload: "lifecycle", payload_bytes: 4096, iterations: 400, concurrency: 64, chunk_bytes: 65536, minimum_iops: 1000 },
    outcome, resource_coverage: coverage, native_phases: nativePhases,
    // This is only the original accepted session/factory projection after
    // finalization; never the full benchmark environment or raw provider error.
    backing: backing?.schema === "mount-rs.runner-backing-observer.v1" ? backing : { status: "unavailable" },
    observation_status: resourceComplete && validatedWorkload ? "observed" : "incomplete",
    live_measurement_qualified: joined && validatedWorkload && resourceComplete && backing?.terminal?.safe_to_continue_pair === true && benchmark.status === "ok" }
}

/** Count newline and every token before publishing any candidate record. */
export function encodeBoundedJSON(value, maximum = OUTPUT_CAP) {
  const chunks = []; let bytes = 0, nodes = 0
  const append = (text) => { const size = Buffer.byteLength(text); if (bytes + size > maximum) fail("pilot_output_cap"); chunks.push(text); bytes += size }
  function encode(item, depth = 0) {
    if (++nodes > 200_000 || depth > 32) fail("pilot_output_cap")
    if (item === null || typeof item === "boolean") return append(JSON.stringify(item))
    if (typeof item === "number") { if (!Number.isFinite(item)) fail("pilot_projection_unavailable"); return append(JSON.stringify(item)) }
    if (typeof item === "string") { if (Buffer.byteLength(item) > maximum - bytes) fail("pilot_output_cap"); return append(JSON.stringify(item)) }
    if (Array.isArray(item)) { append("["); item.forEach((entry, index) => { if (index) append(","); encode(entry, depth + 1) }); return append("]") }
    if (!object(item)) fail("pilot_projection_unavailable")
    append("{"); Object.entries(item).forEach(([key, entry], index) => { if (index) append(","); encode(key, depth + 1); append(":"); encode(entry, depth + 1) }); append("}")
  }
  encode(value); append("\n")
  return Buffer.from(chunks.join(""))
}
export async function writePilotRecord(path, record) {
  // Fixed outcome envelope is built before optional components; retain its
  // first failure even when optional serialization or output exceeds caps.
  const envelope = { ...record, native_phases: { status: "omitted_output_cap" }, backing: { status: "omitted_output_cap" }, observation_status: "incomplete", live_measurement_qualified: false }
  encodeBoundedJSON(envelope, OUTCOME_RESERVE)
  let bytes, omitted = false
  try {
    encodeBoundedJSON({ native_phases: record.native_phases, backing: record.backing }, OUTPUT_CAP - OUTCOME_RESERVE)
    bytes = encodeBoundedJSON(record)
  } catch {
    omitted = true
    bytes = encodeBoundedJSON(envelope, OUTCOME_RESERVE)
  }
  await privateWrite(path, bytes)
  return { bytes: bytes.byteLength, omitted }
}

async function readCID(path) {
  const cid = (await boundedRead(path, 65, true)).toString("utf8")
  if (!/^[a-f0-9]{64}\n?$/u.test(cid)) fail("pilot_handoff_rejected")
  return cid.trim()
}
export async function writeFixtureReceipt(kind, environment) {
  const root = environment.MOUNT_RS_BACKING_CID_DIR
  if (typeof root !== "string" || !isAbsolute(root)) fail("pilot_handoff_rejected")
  if (kind === "rustfs") {
    const owner = safeName(environment.MOUNT_RS_BACKING_RUSTFS_OWNER)
    const entries = [{ cid: await readCID(join(root, "rustfs-service-1.cid")), role: "rustfs-service", labels: { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": owner } }]
    await privateWrite(environment.MOUNT_RS_BACKING_RUSTFS_RECEIPT, encodeBoundedJSON({ schema: "mount-rs.owned-rustfs-cid.v1", rustfs_owner: owner, generation: "1", entries }, INPUT_CAP))
  } else if (kind === "tidb") {
    const rustfs = await privateJSON(environment.MOUNT_RS_BACKING_RUSTFS_RECEIPT)
    exact(rustfs, ["schema", "rustfs_owner", "generation", "entries"], "pilot_handoff_rejected")
    const owner = safeName(environment.MOUNT_RS_BACKING_TIDB_OWNER)
    if (rustfs.schema !== "mount-rs.owned-rustfs-cid.v1" || rustfs.generation !== "1" || rustfs.rustfs_owner !== environment.MOUNT_RS_BACKING_RUSTFS_OWNER) fail("pilot_handoff_rejected")
    const entries = []
    for (const role of roles.slice(0, 7)) entries.push({ cid: await readCID(join(root, `${role}-1.cid`)), role, labels: { "mount-rs.tidb.run": owner } })
    entries.push(...validateEntries(rustfs.entries, owner, rustfs.rustfs_owner, ["rustfs-service"]))
    const receipt = validateHandoff({ schema: "mount-rs.owned-backing-cids.v1", tidb_owner: owner, rustfs_owner: rustfs.rustfs_owner, generation: "1", entries }, environment)
    await privateWrite(environment.MOUNT_RS_BACKING_CID_RECEIPT, encodeBoundedJSON(receipt, INPUT_CAP))
  } else fail("pilot_handoff_rejected")
}

export async function runOwnedPilot(environment = process.env, testing = {}) {
  if (Object.keys(testing).some((key) => !["mock", "clock", "requestImpl", "providers"].includes(key)) ||
      Object.keys(testing).length > 0 && testing.mock !== true) fail("pilot_native_selection_rejected")
  const receipt = validateHandoff(await privateJSON(environment.MOUNT_RS_BACKING_CID_RECEIPT), environment)
  const capability = await privateJSON(environment.MOUNT_RS_BACKING_ENGINE_CAPABILITY)
  exact(capability, ["socket_path"], "pilot_handoff_rejected")
  const identity = await preflightIdentity(environment, { mock: testing.mock === true })
  const { parseArgs, runBenchmark } = await import("./runner.mjs")
  const options = parseArgs(["--providers", "mount-rs-split-tidb-r2", "--layout", "legacy", "--sizes", "1", "--payload-bytes", "4096", "--iterations", "400", "--concurrency", "64", "--min-iops", "1000", "--require-configured", "--network-context", "rustfs-owned-pilot"])
  // Source/selection and endpoint validation precede construction; construction
  // starts the existing owner deadline immediately before this one benchmark.
  const transport = createBackingEngineTransport({ socketPath: capability.socket_path, allowlistedCids: receipt.entries.map((entry) => entry.cid), ...(testing.requestImpl ? { requestImpl: testing.requestImpl } : {}) })
  const observer = createBackingObserver({ allowlist: receipt.entries, transport, ...(testing.clock ? { clock: testing.clock } : {}) })
  const benchmark = await runBenchmark(options, environment, testing.providers, { backingObserver: observer, backingObserverWindow: "workload", backingPilotIdentity: identity, observerClock: testing.clock })
  const record = projectPilotRecord(benchmark, identity.public)
  record.owner = { tidb: receipt.tidb_owner, rustfs: receipt.rustfs_owner, generation: receipt.generation }
  const artifact = await writePilotRecord(environment.MOUNT_RS_BACKING_PILOT_OUTPUT, record)
  return { status: benchmark.status, artifact, observation_status: artifact.omitted ? "incomplete" : record.observation_status, live_measurement_qualified: !artifact.omitted && record.live_measurement_qualified }
}

export async function main(argv = process.argv.slice(2), environment = process.env) {
  try {
    const action = argv[0]
    if (action === "fixture-rustfs" || action === "fixture-tidb") await writeFixtureReceipt(action.slice(8), environment)
    else if (action === "run") {
      const outcome = await runOwnedPilot(environment)
      process.stdout.write(`OWNED_BACKING_PILOT ${JSON.stringify(outcome)}\n`)
      return outcome.status === "ok" ? 0 : 1
    } else fail("pilot_handoff_rejected")
    return 0
  } catch (error) {
    const code = ["pilot_handoff_rejected", "pilot_endpoint_binding_rejected", "pilot_native_selection_rejected", "pilot_private_file_rejected", "pilot_output_cap"].includes(error.code) ? error.code : "pilot_projection_unavailable"
    process.stderr.write(`OWNED_BACKING_PILOT_FAILURE ${code}\n`)
    return 1
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  // Finish library evaluation before the runner imports observePilotBinding.
  main().then((code) => { process.exitCode = code })
}
