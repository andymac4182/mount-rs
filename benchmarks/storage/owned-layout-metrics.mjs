import {
  STORAGE_BYTE_SEMANTICS, STORAGE_CALL_SEMANTICS, STORAGE_ROW_SEMANTICS,
  RUSTFS_API_MEASUREMENT, RUSTFS_LOCAL_MEASUREMENT,
  validateRawPhaseDiagnostics, validateLocalPhaseDiagnostics,
} from "./diagnostics.mjs"

const SCHEMA = "mount-rs.owned-layout-phase-metrics.v1"
const U64 = 18446744073709551615n
const FORWARDING = "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations"
const STORAGE_FIELDS = ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"]
const RAW_FIELDS = ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end", "exact_phase_max_ns"]
const LOCAL_FIELDS = ["calls", "success", "error", "cancelled", "elapsed_ns", "input_bytes", "output_bytes", "latency_max_ns_start", "latency_max_ns_end", "exact_phase_max_ns"]
const LOGICAL_FIELDS = ["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]
const CLAIM_FIELDS = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]
// Closed names from the native core recorder. Missing rows are never filled with zero.
const CORE_NAMES = new Set([
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
  "compact.capture.guard_clones",
  "compact.capture.file_layout_clones",
  "compact.capture.extent_clones",
  "compact.create.namespace_cow_nodes",
  "compact.create.namespace_cow_layouts",
  "compact.create.namespace_cow_extents",
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
])
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value)
const fields = (source, names) => Object.fromEntries(names.map((name) => [name, source[name]]))
const unavailable = () => ({ status: "unavailable" })
function counter(value) {
  if (typeof value !== "string" || value.length > 20 || !/^(?:0|[1-9]\d*)$/u.test(value) || BigInt(value) > U64) throw Error("counter")
  return value
}
function counters(source, names) {
  if (!object(source)) throw Error("shape")
  return Object.fromEntries(names.map((name) => [name, counter(source[name])]))
}
function frozen(value) {
  if (value !== null && typeof value === "object") { for (const item of Object.values(value)) frozen(item); Object.freeze(value) }
  return value
}
// Original runner records are private inputs. Snapshot descriptors once so getters
// cannot rewrite validated counters or throw sensitive errors into the artifact.
function snapshot(source) {
  const state = { nodes: 0, bytes: 0, active: new WeakSet(), seen: new WeakMap() }
  function copy(value, depth = 0) {
    if (++state.nodes > 200_000 || depth > 32) throw Error("cap")
    if (typeof value === "string") { state.bytes += Buffer.byteLength(value); if (state.bytes > 8_388_608) throw Error("cap"); return value }
    if (value === null || value === undefined || typeof value === "boolean" || typeof value === "number") return value
    if (typeof value !== "object" || state.active.has(value)) throw Error("shape")
    if (state.seen.has(value)) return state.seen.get(value)
    const array = Array.isArray(value), proto = Object.getPrototypeOf(value)
    if (!array && proto !== Object.prototype && proto !== null) throw Error("shape")
    const descriptors = Object.getOwnPropertyDescriptors(value), keys = Reflect.ownKeys(descriptors)
    if (keys.length > 200_000 - state.nodes) throw Error("cap")
    const length = array ? descriptors.length?.value : null
    if (array && (!Number.isSafeInteger(length) || length > 200_000 || length < 0)) throw Error("cap")
    if (array && (keys.length !== length + 1 || !Array.from({ length }, (_, index) => index).every((index) => Object.hasOwn(descriptors, String(index))))) throw Error("shape")
    const target = array ? Array(length) : {}; state.seen.set(value, target); state.active.add(value)
    for (const key of keys) {
      const descriptor = descriptors[key]
      if (typeof key !== "string" || !Object.hasOwn(descriptor, "value")) throw Error("shape")
      state.bytes += Buffer.byteLength(key); if (state.bytes > 8_388_608) throw Error("cap")
      if (array && key === "length") continue
      Object.defineProperty(target, key, { value: copy(descriptor.value, depth + 1), enumerable: true, configurable: true, writable: true })
    }
    state.active.delete(value)
    return target
  }
  return copy(source)
}
function rows(entries, names) {
  return entries.map((entry) => ({ name: entry.name, ...fields(entry, names), latency_log2_us: [...entry.latency_log2_us] }))
}
function local(instance, measurement) {
  try {
    const value = validateLocalPhaseDiagnostics(instance.local_work, measurement, "rustfs")
    return { status: "observed", ...fields(value, ["schema", "scope", "in_flight_start", "in_flight_end", "saturated_start", "saturated_end"]), entries: rows(value.entries, LOCAL_FIELDS) }
  } catch { return unavailable() }
}
function processMetrics(source) {
  try {
    return { status: "observed", cpu_work: counters(source.cpu_work, ["user_us", "system_us"]), cpu_observer: counters(source.cpu_observer, ["user_us", "system_us"]), resources_work: counters(source.resources_work, ["voluntary_context_switches", "involuntary_context_switches"]), memory_end_bytes: counters(source.memory_end_bytes, ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"]),
      native_allocation_count: "unavailable", js_allocation_count: "unavailable",
      scope: "process_work_and_snapshot_cpu; includes_background_work; memory_is_endpoint_not_delta_or_peak" }
  } catch { return { ...unavailable(), native_allocation_count: "unavailable", js_allocation_count: "unavailable" } }
}
function forwarding(native) {
  try {
    if (native.measurement.forwarding_boxes !== FORWARDING || native.storage.forwarding_boxes?.sites !== "napi_dynamic_provider_forwarding_future") throw Error("measurement")
    return { status: "observed", sites: "napi_dynamic_provider_forwarding_future", ...counters(native.storage.forwarding_boxes, ["calls", "requested_object_bytes"]), scope: FORWARDING }
  } catch { return unavailable() }
}
function core(native) {
  try {
    const entries = native.profile?.entries
    if (native.measurement.profile !== "existing_core_profile_counters" || !Array.isArray(entries) || entries.length > CORE_NAMES.size) throw Error("profile")
    const names = new Set(), result = []
    for (const entry of entries) {
      if (!object(entry) || !CORE_NAMES.has(entry.name) || names.has(entry.name)) throw Error("profile")
      names.add(entry.name); result.push({ name: entry.name, ...counters(entry, ["calls", "elapsed_ns", "units"]) })
    }
    return { status: "observed", entries: result, scope: "process_core_profile_delta; units_depend_on_fixed_event; inclusive_wall_spans_overlap; absent_events_unavailable" }
  } catch { return unavailable() }
}

// This retains measured evidence. It does not qualify a runner or derive device
// IOPS, wire amplification, exclusive CPU time, or a total allocation count.
export function projectOwnedLayoutPhaseMetrics(source) {
  try {
    const phase = snapshot(source)
    if (!object(phase) || phase.name !== "workload-4096bytes" || !Number.isFinite(phase.elapsed_ms) || !Number.isFinite(phase.benchmark_measured_elapsed_ms) || phase.benchmark_measured_elapsed_ms <= 0 || phase.elapsed_ms < phase.benchmark_measured_elapsed_ms || !Number.isFinite(phase.observer_snapshot_ms) || phase.observer_snapshot_ms < 0) throw Error("window")
    validateRawPhaseDiagnostics(phase, "rustfs")
    const native = phase.native
    if (native.rustfs.instances.length > 64) throw Error("cap")
    const result = { schema: SCHEMA, status: "observed", phase: "workload-4096bytes", quiescent: true,
      elapsed_ms: phase.elapsed_ms, benchmark_measured_elapsed_ms: phase.benchmark_measured_elapsed_ms, observer_snapshot_ms: phase.observer_snapshot_ms,
      storage: { scope: "process_fixed_label_operation_delta; audited_instrumented_operations_separately_declared; families_overlap; inclusive_wall_spans_overlap", calls: STORAGE_CALL_SEMANTICS, bytes: STORAGE_BYTE_SEMANTICS, returned_rows: STORAGE_ROW_SEMANTICS,
        operations: [...native.measurement.storage_operations], families: structuredClone(native.measurement.storage_families),
        instrumented_operations: [...native.measurement.storage_instrumented_operations], foundationdb_coverage: structuredClone(native.measurement.foundationdb_coverage),
        entries: rows(native.storage.entries, STORAGE_FIELDS) },
      rustfs: { scope: "process_live_instances", instance_ids_start: [...native.rustfs.instance_ids_start], instance_ids_end: [...native.rustfs.instance_ids_end], api_measurement: structuredClone(RUSTFS_API_MEASUREMENT), local_measurement: structuredClone(RUSTFS_LOCAL_MEASUREMENT), instances: native.rustfs.instances.map((instance) => ({
        id: instance.id, ...fields(instance, LOGICAL_FIELDS), raw_api: { ...fields(instance.raw_api, ["schema", "scope", "in_flight_start", "in_flight_end", "pending_claims_start", "pending_claims_end", "saturated_start", "saturated_end"]), claims: fields(instance.raw_api.claims, CLAIM_FIELDS), entries: rows(instance.raw_api.entries, RAW_FIELDS) }, local_work: local(instance, native.measurement.rustfs_local),
      })) },
      process: processMetrics(phase.process), forwarding_boxes: forwarding(native), core_profile: core(native),
      unavailable: { physical_device_iops: true, http_attempts_and_internal_retries: true, sql_wire_bytes: true, pool_queue_only_wait: true, exclusive_cpu_time: true, native_allocation_count: true, js_allocation_count: true },
    }
    if (Buffer.byteLength(JSON.stringify(result)) > 2_097_152) throw Error("cap")
    return frozen(result)
  } catch { return frozen({ schema: SCHEMA, status: "unavailable", reason: "PHASE_METRICS_UNAVAILABLE" }) }
}
