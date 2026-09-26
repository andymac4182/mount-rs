import { isDeepStrictEqual } from "node:util"

// Opt-in quiescent phase snapshots. Native counters are exact decimal strings.
const decimal = /^\d+$/u
const histogramIntervals = Array.from({ length: 32 }, (_, bucket) => bucket === 0
  ? { lower_inclusive_us: "0", upper_exclusive_us: "1" }
  : bucket === 31
    ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true }
    : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) })
function subtract(now, old = "0") {
  if (typeof now !== "string" || typeof old !== "string" || !decimal.test(now) || !decimal.test(old)) throw new Error("invalid decimal counter")
  const difference = BigInt(now) - BigInt(old)
  if (difference < 0n) throw new Error("counter reset")
  return difference.toString()
}
function entriesDelta(before, after, idKey, fields) {
  if (!Array.isArray(before) || !Array.isArray(after) || before.length !== after.length) throw new Error("counter shape changed")
  return after.map((entry, index) => {
    const old = before[index]
    if (entry[idKey] !== old[idKey]) throw new Error("counter names changed")
    const delta = { [idKey]: entry[idKey] }
    for (const field of fields) {
      if (Array.isArray(entry[field]) !== Array.isArray(old[field]) ||
          (Array.isArray(entry[field]) && entry[field].length !== old[field].length)) throw new Error("counter histogram shape changed")
      delta[field] = Array.isArray(entry[field])
        ? entry[field].map((value, bucket) => subtract(value, old[field]?.[bucket]))
        : subtract(entry[field], old[field])
    }
    return delta
  })
}
function connectionDelta(before, after) {
  const previous = new Map(before.map((entry) => [entry.connection_id, entry]))
  const current = new Map(after.map((entry) => [entry.connection_id, entry]))
  const missing = [...previous.keys()].filter((id) => !current.has(id))
  const connections = after.map((entry) => {
    const old = previous.get(entry.connection_id)
    if (entry.error || old?.error) throw new Error("SQLite counter unavailable")
    const pager = {}
    for (const label of ["cache_hits", "cache_misses", "page_writes", "cache_spills"]) pager[label] = subtract(entry.pager[label], old?.pager?.[label])
    const sql_categories = {}
    for (const label of new Set([...Object.keys(old?.sql_categories || {}), ...Object.keys(entry.sql_categories || {})])) {
      sql_categories[label] = subtract(entry.sql_categories?.[label] ?? "0", old?.sql_categories?.[label])
    }
    return { connection_id: entry.connection_id, page_size: entry.page_size, pager,
      pager_read_bytes_estimate: subtract(entry.pager_read_bytes_estimate, old?.pager_read_bytes_estimate),
      pager_write_bytes_estimate: subtract(entry.pager_write_bytes_estimate, old?.pager_write_bytes_estimate),
      sql_statements: subtract(entry.sql_statements, old?.sql_statements), sql_categories,
      opened_during_phase: !old }
  })
  return { connections, missing_connection_ids: missing, complete: missing.length === 0 }
}
function r2Delta(before, after) {
  const previous = new Map(before.map((entry) => [entry.id, entry]))
  const current = new Map(after.map((entry) => [entry.id, entry]))
  const missing = [...previous.keys()].filter((id) => !current.has(id))
  const fields = ["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]
  return { instances: after.map((entry) => {
    const old = previous.get(entry.id)
    return Object.fromEntries([["id", entry.id], ...fields.map((field) => [field, subtract(entry[field], old?.[field])])])
  }), missing_instance_ids: missing, complete: missing.length === 0, internal_successful_retries: "unavailable" }
}

export function deltaNativeSnapshots(before, after) {
  try {
    if (before.schema_version !== "mount-rs.storage-diagnostics.v1" || after.schema_version !== before.schema_version || !before.enabled || !after.enabled) throw new Error("diagnostic version or enabled state changed")
    if (!before.measurement || !isDeepStrictEqual(before.measurement, after.measurement) ||
        !isDeepStrictEqual(after.measurement.latency_histogram, { unit: "microseconds", intervals: histogramIntervals }) ||
        after.measurement.storage_calls !== "logical_provider_calls" ||
        after.measurement.storage_bytes !== "successful_payload_bytes_at_provider_boundary" ||
        after.measurement.storage_duration !== "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" ||
        after.measurement.sqlite !== "live_connection_pager_and_sql_category_counters; pager_bytes_are_page_size_estimates" ||
        after.measurement.r2 !== "live_store_logical_calls_and_cache_hits; not_http_attempts" ||
        !isDeepStrictEqual(after.measurement.unavailable, {
          http_attempts: "unavailable", internal_successful_retries: "unavailable",
          physical_device_iops: "unavailable", tidb_pool_wait: "unavailable",
          native_allocation_count: "unavailable", js_allocation_count: "unavailable",
        }) ||
        after.measurement.forwarding_boxes !== "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations") throw new Error("measurement metadata changed")
    if (!isDeepStrictEqual(before.backend_waits, after.backend_waits) ||
        before.http_attempts !== after.http_attempts || before.physical_device_iops !== after.physical_device_iops) throw new Error("availability metadata changed")
    const boxBefore = before.storage.forwarding_boxes
    const boxAfter = after.storage.forwarding_boxes
    if (boxBefore?.sites !== "napi_dynamic_provider_forwarding_future" || boxAfter?.sites !== boxBefore.sites) throw new Error("forwarding allocation site changed")
    const storage = { in_flight_start: before.storage.in_flight, in_flight_end: after.storage.in_flight,
      forwarding_boxes: { sites: boxAfter.sites, calls: subtract(boxAfter.calls, boxBefore.calls), requested_object_bytes: subtract(boxAfter.requested_object_bytes, boxBefore.requested_object_bytes) },
      entries: entriesDelta(before.storage.entries, after.storage.entries, "name", ["calls", "success", "error", "cancelled", "bytes", "elapsed_ns", "latency_log2_us"]) }
    for (const entry of storage.entries) {
      if (entry.latency_log2_us.length !== after.measurement.latency_histogram.intervals.length) throw new Error("histogram does not match declared intervals")
      if (BigInt(entry.calls) !== BigInt(entry.success) + BigInt(entry.error) + BigInt(entry.cancelled)) throw new Error("storage outcomes do not reconcile")
    }
    const profile = { entries: entriesDelta(before.profile.entries, after.profile.entries, "name", ["calls", "elapsed_ns", "units"]) }
    const sqlite = connectionDelta(before.sqlite.connections, after.sqlite.connections)
    const r2 = r2Delta(before.r2.instances, after.r2.instances)
    const issues = []
    if (typeof storage.in_flight_start !== "string" || typeof storage.in_flight_end !== "string" ||
        !decimal.test(storage.in_flight_start) || !decimal.test(storage.in_flight_end)) throw new Error("invalid in-flight gauge")
    if (BigInt(storage.in_flight_start) !== 0n || BigInt(storage.in_flight_end) !== 0n) issues.push("instrumented storage operation crossed phase boundary")
    if (!sqlite.complete) issues.push("SQLite connection closed during phase")
    if (!r2.complete) issues.push("R2 instance closed during phase")
    return { complete: issues.length === 0, issues, measurement: after.measurement,
      backend_waits: after.backend_waits, http_attempts: after.http_attempts,
      physical_device_iops: after.physical_device_iops, storage, profile, sqlite, r2 }
  } catch (error) { return { complete: false, issues: [error.message] } }
}

export function takePhaseSnapshot(nativeSnapshot, samplers = {
  now: () => performance.now(), cpu: () => process.cpuUsage(),
  resources: () => process.resourceUsage(), memory: () => process.memoryUsage(),
}) {
  const started = samplers.now()
  const cpuStart = samplers.cpu()
  const resourcesStart = samplers.resources()
  let native
  try { native = JSON.parse(nativeSnapshot()) } catch { native = null }
  const memory = samplers.memory()
  // Endpoints follow all substantial snapshot work, including memoryUsage.
  const resourcesEnd = samplers.resources()
  const cpuEnd = samplers.cpu()
  return { started, ended: samplers.now(), cpuStart, cpuEnd, resourcesStart, resourcesEnd, memory, native }
}

export function finishPhase(name, before, after, quiescent = true) {
  const delta = before.native && after.native ? deltaNativeSnapshots(before.native, after.native) : { complete: false, issues: ["native diagnostics unavailable"] }
  if (!quiescent) { delta.complete = false; delta.issues.push("native operations crossed phase boundary") }
  const cpu = { user_us: String(after.cpuStart.user - before.cpuEnd.user), system_us: String(after.cpuStart.system - before.cpuEnd.system) }
  const observerCpu = { user_us: String((before.cpuEnd.user - before.cpuStart.user) + (after.cpuEnd.user - after.cpuStart.user)), system_us: String((before.cpuEnd.system - before.cpuStart.system) + (after.cpuEnd.system - after.cpuStart.system)) }
  const resources = { voluntary_context_switches: String(after.resourcesStart.voluntaryContextSwitches - before.resourcesEnd.voluntaryContextSwitches), involuntary_context_switches: String(after.resourcesStart.involuntaryContextSwitches - before.resourcesEnd.involuntaryContextSwitches) }
  return { name, elapsed_ms: after.started - before.ended, observer_snapshot_ms: (before.ended - before.started) + (after.ended - after.started), quiescent, native: delta, process: { cpu_work: cpu, cpu_observer: observerCpu, memory_end_bytes: Object.fromEntries(Object.entries(after.memory).map(([key, value]) => [key, String(value)])), resources_work: resources, native_allocation_count: "unavailable", js_allocation_count: "unavailable" } }
}

export function logPhaseSummary(phase) {
  const entries = phase.native.storage?.entries || []
  const total = (field) => entries.reduce((sum, entry) => sum + BigInt(entry[field]), 0n).toString()
  process.stderr.write(`MOUNT_RS_STORAGE_PHASE ${JSON.stringify({ name: phase.name, complete: phase.native.complete, elapsed_ms: phase.elapsed_ms, calls: total("calls"), bytes: total("bytes"), errors: total("error"), cancelled: total("cancelled") })}\n`)
}
