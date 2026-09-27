import assert from "node:assert/strict"
import test from "node:test"
import {
  deltaNativeSnapshots, logPhaseSummary, NATIVE_DIAGNOSTICS_SCHEMA,
  STORAGE_BYTE_SEMANTICS, STORAGE_CALL_SEMANTICS, STORAGE_ROW_SEMANTICS,
  STORAGE_OPERATION_FAMILIES, TIDB_DIAGNOSTIC_COVERAGE,
} from "./diagnostics.mjs"

// Independent pre-extension inventory: append-only compatibility is part of the contract.
// Import only the current pure consumer; no addon, backend, filesystem helper or test harness.
const legacyNames = Object.freeze([
  "metadata.load",
  "metadata.load_if_changed",
  "metadata.snapshot",
  "metadata.publish",
  "metadata.flush",
  "blocks.put",
  "blocks.get",
  "blocks.flush",
  "blocks.verify_backing",
  "blocks.prepare_backing",
  "blocks.delete",
  "blocks.reconcile",
  "pglite.client_lock_wait",
  "sdk.metadata.compact_inode_capability",
  "sdk.metadata.compact_inode_mode_state",
  "sdk.metadata.prepare_compact_inode_mode",
  "sdk.metadata.load_compact_snapshot",
  "sdk.metadata.load_compact_inode",
  "sdk.metadata.publish_compact_inode",
  "sdk.metadata.publish_compact_structure",
  "sdk.metadata.inode_mode_state",
  "sdk.metadata.prepare_inode_mode",
  "sdk.metadata.load_inode_snapshot_if_changed",
  "sdk.metadata.load_inode_snapshot",
  "sdk.metadata.load_inode",
  "sdk.metadata.load_inode_if_changed",
  "sdk.metadata.publish_inode_if_version",
  "sdk.metadata.publish_structure_if_versions",
  "sdk.metadata.delegation_state",
  "sdk.metadata.prepare_delegated_mode",
  "sdk.metadata.checkout",
  "sdk.metadata.publish_delegated",
  "sdk.metadata.checkin",
  "sdk.metadata.recover",
  "sdk.metadata.durable",
  "sdk.metadata.publish_includes_flush_barrier",
  "sdk.metadata.load",
  "sdk.metadata.load_if_changed",
  "sdk.metadata.concurrent_mode_state",
  "sdk.metadata.preflight_new_bound_mode",
  "sdk.metadata.prepare_bound_concurrent_mode",
  "sdk.metadata.acquire_writer",
  "sdk.metadata.renew_writer",
  "sdk.metadata.release_writer",
  "sdk.metadata.publish",
  "sdk.metadata.publish_bound_if_revision",
  "sdk.metadata.migrate_mrc1_to_bound_mode",
  "sdk.metadata.preflight_mrc1_to_bound_mode",
  "sdk.metadata.preflight_trusted_unstamped_mrc1",
  "sdk.metadata.migrate_trusted_unstamped_mrc1",
  "sdk.metadata.flush",
  "sdk.blocks.durable",
  "sdk.blocks.prepare_concurrent_backing",
  "sdk.blocks.verify_concurrent_backing",
  "sdk.blocks.get_for_migration",
  "sdk.blocks.put",
  "sdk.blocks.get",
  "sdk.blocks.flush",
  "sdk.blocks.delete",
  "sdk.blocks.reconcile",
  "tidb.pool.checkout",
  "tidb.session.configure",
  "tidb.open.schema",
  "tidb.open.metadata_row",
  "tidb.tx.begin.metadata",
  "tidb.tx.begin.inode",
  "tidb.tx.begin.compact_read",
  "tidb.tx.commit",
  "tidb.tx.rollback",
  "tidb.sql.session",
  "tidb.sql.ddl",
  "tidb.sql.metadata_read",
  "tidb.sql.metadata_write",
  "tidb.sql.inode_read",
  "tidb.sql.inode_write",
  "tidb.sql.block_read",
  "tidb.sql.block_write",
  "tidb.sql.flush_probe",
])
const foundationdbNames = Object.freeze([
  "foundationdb.transaction.create",
  "foundationdb.transaction.closure_attempt",
  "foundationdb.read.get",
  "foundationdb.read.get_key",
  "foundationdb.read.get_range_page",
  "foundationdb.transaction.commit",
  "foundationdb.transaction.on_error",
])
const preCacheNames = Object.freeze([...legacyNames, ...foundationdbNames])
const cacheNames = Object.freeze([
  "blob_cache.miss.admission_wait",
  "blob_cache.miss.singleflight_wait",
  "blob_cache.ram.lookup",
  "blob_cache.disk.lookup",
  "blob_cache.peer.connection_lock_wait",
  "blob_cache.peer.connection_establish",
])
const operationNames = Object.freeze([...preCacheNames, ...cacheNames])
const duration = "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap"
const foundationdbFamilies = {
  foundationdb_transaction: {
    operations: foundationdbNames.filter((name) => name.startsWith("foundationdb.transaction.")),
    calls: "provider_closure_attempts_and_native_create_commit_on_error_invocations; distinct_units_not_logical_transactions",
    bytes: "unavailable", returned_rows: "unavailable", duration,
  },
  foundationdb_read: {
    operations: foundationdbNames.filter((name) => name.startsWith("foundationdb.read.")),
    calls: "native_client_read_method_invocations; range_page_calls_not_key_value_count",
    bytes: "known_selected_successful_returned_value_key_and_range_page_key_value_payload_bytes_only",
    returned_rows: "unavailable", duration,
  },
}
const foundationdbCoverage = {
  schema: "mount-rs-foundationdb-client-diagnostic-coverage-v1",
  status: "source_sites_instrumented",
  transaction_runner_sites: "6", point_get_sites: "2", get_key_sites: "1", range_consumer_sites: "3",
  operations: [...foundationdbNames],
  attempt_scope: "audited_provider_closure_and_native_client_dispatch_attempts; process_fixed_label_bank",
  payload_bytes_scope: "successful_get_value_get_key_key_and_range_page_key_value_return_lengths_only; no_key_contents",
  returned_rows_scope: "unavailable; storage_returned_rows_are_SQL_only",
  unavailable: [
    "client_internal_retries", "wire_rpc_count", "wire_bytes", "server_execution_time",
    "physical_device_iops", "exclusive_cpu", "transaction_lifetime",
    "native_operation_settlement_after_future_drop",
    "durability_or_rollback_after_commit_error_or_cancellation",
  ],
}
const disabledCoverage = {
  schema: foundationdbCoverage.schema, status: "unavailable",
  reason: "feature_disabled_or_unsupported_target", operations: [],
}
const rawMeasurement = {
  schema: "mount-rs.object-store-api.v1", scope: "live_registered_split_r2_block_store_instances",
  calls: "object_store_adapter_method_invocations; not_http_attempts_or_internal_retries",
  duration: "inclusive_wall_nanoseconds_at_invoked_adapter_await; excludes_argument_preparation",
  upload_bytes: "attempted=submitted_payload; confirmed=put_opts_ok_only",
  returned_bytes: "successful_body_materialization_before_integrity_validation",
  latency_max: "cumulative_per_instance; exact_phase_max_unavailable", reconcile_listing: "unavailable",
  excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "internal_client_retries"],
}
const zeros = (fields) => Object.fromEntries(fields.map((field) => [field, "0"]))
const row = (name) => ({
  name, ...zeros(["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "in_flight", "elapsed_ns"]),
  latency_log2_us: Array(32).fill("0"),
})
function snapshot({ legacy = false, preCache = false, feature = true } = {}) {
  const names = legacy ? legacyNames : preCache ? preCacheNames : operationNames
  const families = Object.fromEntries(Object.entries(STORAGE_OPERATION_FAMILIES).filter(([name]) => !name.startsWith("foundationdb_") && name !== "blob_cache"))
  return {
    schema_version: NATIVE_DIAGNOSTICS_SCHEMA, enabled: true, scope: "process",
    quiescent_snapshot_required: true, elapsed_semantics: "inclusive_wall_nanoseconds",
    measurement: {
      storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS, storage_rows: STORAGE_ROW_SEMANTICS,
      storage_operations: [...names], storage_families: { ...structuredClone(families), ...(legacy ? {} : structuredClone(foundationdbFamilies)), ...(!legacy && !preCache ? { blob_cache: structuredClone(STORAGE_OPERATION_FAMILIES.blob_cache) } : {}) },
      storage_instrumented_operations: [...legacyNames, ...(!legacy && feature ? foundationdbNames : [])],
      tidb_coverage: structuredClone(TIDB_DIAGNOSTIC_COVERAGE),
      ...(legacy ? {} : { foundationdb_coverage: structuredClone(feature ? foundationdbCoverage : disabledCoverage) }),
      storage_duration: duration,
      forwarding_boxes: "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations",
      profile: "existing_core_profile_counters",
      sqlite: "live_connection_pager_and_sql_category_counters; pager_bytes_are_page_size_estimates",
      r2: "live_store_logical_calls_and_cache_hits; not_http_attempts", r2_api: structuredClone(rawMeasurement),
      unavailable: { http_attempts: "unavailable", internal_successful_retries: "unavailable", physical_device_iops: "unavailable", tidb_pool_wait: "isolated_queue_only_wait_unavailable", native_allocation_count: "unavailable", js_allocation_count: "unavailable" },
      latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, bucket) => bucket === 0
        ? { lower_inclusive_us: "0", upper_exclusive_us: "1" }
        : bucket === 31 ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true }
          : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) }) },
    },
    backend_waits: { pglite_client_lock: "instrumented", tidb_pool: "instrumented_inclusive_checkout_including_lazy_connect_and_session_configuration" },
    http_attempts: "unavailable", physical_device_iops: "unavailable",
    storage: { in_flight: "0", forwarding_boxes: { sites: "napi_dynamic_provider_forwarding_future", calls: "0", requested_object_bytes: "0" }, entries: names.map(row) },
    profile: { entries: [] }, sqlite: { connections: [] },
    r2: { scope: "process_live_instances", instances: [], internal_successful_retries: "unavailable" },
  }
}
const find = (value, name) => value.storage.entries.find((entry) => entry.name === name)
function oneSuccess(value, name = foundationdbNames[2]) {
  Object.assign(find(value, name), { calls: "1", success: "1", elapsed_ns: "1000", latency_log2_us: ["0", "1", ...Array(30).fill("0")] })
}
function observed(before, after) {
  const value = deltaNativeSnapshots(before, after)
  assert.equal(value.complete, true, value.issues?.join("; "))
  assert.deepEqual(value.issues, [])
  return value
}
function incomplete(before, after, issue) {
  const value = deltaNativeSnapshots(before, after)
  assert.equal(value.complete, false)
  assert.ok(Array.isArray(value.issues))
  assert.match(value.issues.join("; "), issue)
  assert.equal(Object.hasOwn(value, "storage"), false, "invalid source rows cannot become complete phase deltas")
  return value
}
function summary(native) {
  let output = ""
  const original = process.stderr.write
  process.stderr.write = (chunk) => { output += String(chunk); return true }
  try { logPhaseSummary({ name: "fixture-only", elapsed_ms: 1, native }) }
  finally { process.stderr.write = original }
  const lines = output.trim().split("\n")
  assert.equal(lines.length, 1)
  return JSON.parse(lines[0].replace(/^MOUNT_RS_STORAGE_PHASE /u, ""))
}

test("new 91-row fixture has reconciled exact decimal endpoints before consumer validation", () => {
  const value = snapshot()
  assert.equal(value.storage.entries.length, 91)
  assert.deepEqual(value.storage.entries.map((entry) => entry.name), operationNames)
  assert.deepEqual(value.storage.entries.slice(78, 85).map((entry) => entry.name), foundationdbNames)
  assert.deepEqual(value.storage.entries.slice(85).map((entry) => entry.name), cacheNames)
  for (const entry of value.storage.entries) {
    assert.equal(BigInt(entry.calls), BigInt(entry.success) + BigInt(entry.error) + BigInt(entry.cancelled))
    assert.equal(entry.latency_log2_us.reduce((sum, count) => sum + BigInt(count), 0n), BigInt(entry.calls))
    assert.equal(entry.returned_rows, "0"); assert.equal(entry.returned_row_observations, "0")
  }
})
test("old 78-row observation remains incomplete without seven invented zero rows", () => {
  const before = snapshot({ legacy: true }), after = snapshot({ legacy: true })
  const value = deltaNativeSnapshots(before, after)
  assert.equal(value.complete, false, "a legacy inventory cannot establish the new FoundationDB coverage")
  assert.equal(Object.hasOwn(value, "storage"), false)
  assert.equal(value.observations.after.storage.entries.length, 78)
  assert.equal(value.observations.after.storage.entries.values.some((entry) => foundationdbNames.includes(entry.name)), false)
  assert.equal(value.observations.after.measurement.foundationdb_coverage, "unavailable")
})
test("old 85-row observation remains incomplete without six invented cache rows", () => {
  const before = snapshot({ preCache: true }), after = snapshot({ preCache: true })
  Object.assign(find(after, foundationdbNames[2]), { calls: "1", success: "1", bytes: "9007199254740993", elapsed_ns: "1000", latency_log2_us: ["0", "1", ...Array(30).fill("0")] })
  const value = deltaNativeSnapshots(before, after)
  assert.equal(value.complete, false)
  assert.equal(Object.hasOwn(value, "storage"), false)
  assert.equal(value.observations.after.storage.entries.length, 85)
  assert.equal(value.observations.after.storage.entries.values.length, 85)
  assert.equal(value.observations.after.storage.entries.values.some((entry) => cacheNames.includes(entry.name)), false)
  assert.equal(value.observations.after.storage.entries.values.find((entry) => entry.name === foundationdbNames[2]).bytes, "9007199254740993")
})
test("seven FoundationDB terminal outcomes and histograms reconcile independently", () => {
  const before = snapshot(), after = snapshot()
  for (const name of foundationdbNames) Object.assign(find(after, name), {
    calls: "3", success: "1", error: "1", cancelled: "1", elapsed_ns: "3000",
    bytes: name.startsWith("foundationdb.read.") ? "7" : "0",
    latency_log2_us: ["0", "3", ...Array(30).fill("0")],
  })
  const value = observed(before, after)
  assert.deepEqual(value.storage.entries.map((entry) => entry.name), operationNames)
  for (const name of foundationdbNames) {
    const entry = find(value, name)
    assert.deepEqual([entry.calls, entry.success, entry.error, entry.cancelled], ["3", "1", "1", "1"])
    assert.equal(entry.latency_log2_us[1], "3")
    assert.equal(entry.returned_rows, "0"); assert.equal(entry.returned_row_observations, "0")
  }
  assert.deepEqual(value.measurement.foundationdb_coverage, foundationdbCoverage)
})
test("FoundationDB payload and inclusive elapsed deltas above 2^53 stay precise", () => {
  const before = snapshot(), after = snapshot(), name = foundationdbNames[2], base = 9007199254740993n
  Object.assign(find(before, name), { calls: String(base), success: String(base), bytes: String(base), elapsed_ns: String(base), latency_log2_us: [String(base), ...Array(31).fill("0")] })
  Object.assign(find(after, name), { calls: String(base + 3n), success: String(base + 1n), error: "1", cancelled: "1", bytes: String(base + 9007199254740995n), elapsed_ns: String(base + 9007199254740997n), latency_log2_us: [String(base), "3", ...Array(30).fill("0")] })
  const entry = find(observed(before, after), name)
  assert.equal(entry.calls, "3"); assert.equal(entry.success, "1")
  assert.equal(entry.bytes, "9007199254740995"); assert.equal(entry.elapsed_ns, "9007199254740997")
})
test("successful empty get payload is a known zero without inventing SQL rows", () => {
  const before = snapshot(), after = snapshot()
  oneSuccess(after)
  const value = observed(before, after), entry = find(value, foundationdbNames[2])
  assert.equal(entry.bytes, "0"); assert.equal(entry.returned_rows, "0"); assert.equal(entry.returned_row_observations, "0")
  assert.match(value.measurement.storage_bytes, /zero_does_not_establish_no_payload/u)
  assert.equal(value.measurement.storage_families.foundationdb_read.returned_rows, "unavailable")
})
test("feature-off 91-row phase keeps FDB and cache source coverage unavailable", () => {
  const value = observed(snapshot({ feature: false }), snapshot({ feature: false }))
  assert.deepEqual(value.measurement.foundationdb_coverage, disabledCoverage)
  assert.equal(value.measurement.storage_instrumented_operations.some((name) => name.startsWith("foundationdb.")), false)
  assert.equal(value.measurement.storage_instrumented_operations.length, 78)
  assert.equal(value.measurement.storage_instrumented_operations.some((name) => name.startsWith("blob_cache.")), false)
  assert.equal(value.storage.entries.length, 91)
})
test("declared cache family stays unavailable in addon summaries with FDB enabled or disabled", () => {
  for (const feature of [true, false]) {
    const value = observed(snapshot({ feature }), snapshot({ feature }))
    assert.equal(value.measurement.storage_instrumented_operations.length, feature ? 85 : 78)
    assert.equal(value.measurement.storage_instrumented_operations.some((name) => cacheNames.includes(name)), false)
    const family = summary(value).families.blob_cache
    assert.equal(family.available, false)
    assert.equal(family.instrumented, false)
    assert.deepEqual(family.instrumented_operations, [])
    for (const field of ["calls", "bytes", "error", "cancelled", "elapsed_ns", "returned_rows", "returned_row_observations"]) assert.equal(Object.hasOwn(family, field), false)
  }
})
test("feature-off family summaries do not publish fixed zero rows as observed FDB attempts", () => {
  const logged = summary(observed(snapshot({ feature: false }), snapshot({ feature: false })))
  for (const family of ["foundationdb_transaction", "foundationdb_read"]) {
    assert.equal(logged.families[family].available, false)
    assert.equal(logged.families[family].instrumented, false)
    assert.deepEqual(logged.families[family].instrumented_operations, [])
    for (const field of ["calls", "bytes", "error", "cancelled", "elapsed_ns", "returned_rows", "returned_row_observations"]) assert.equal(Object.hasOwn(logged.families[family], field), false)
  }
})
for (const [title, mutate, issue] of [
  ["outcomes", (value) => { oneSuccess(value); find(value, foundationdbNames[2]).success = "0" }, /outcomes/u],
  ["histogram", (value) => { oneSuccess(value); find(value, foundationdbNames[2]).latency_log2_us[1] = "0" }, /histogram/u],
  ["SQL-only rows", (value) => { oneSuccess(value); Object.assign(find(value, foundationdbNames[2]), { returned_rows: "1", returned_row_observations: "1" }) }, /outside SQL/u],
  ["transaction payload bytes", (value) => { oneSuccess(value, foundationdbNames[0]); find(value, foundationdbNames[0]).bytes = "1" }, /transaction payload/u],
  ["row pending", (value) => { find(value, foundationdbNames[5]).in_flight = "1" }, /pending|boundary/u],
  ["global pending", (value) => { value.storage.in_flight = "1" }, /pending|boundary/u],
]) test(`FoundationDB ${title} prevents complete phase evidence`, () => {
  const before = snapshot(), after = snapshot(); mutate(after)
  incomplete(before, after, issue)
})
test("FoundationDB counter reset prevents a valid-looking zero phase delta", () => {
  const before = snapshot(), after = snapshot(); oneSuccess(before)
  incomplete(before, after, /reset/u)
})
test("missing FoundationDB feature coverage is unavailable with closed observations", () => {
  const before = snapshot(), after = snapshot()
  delete before.measurement.foundationdb_coverage; delete after.measurement.foundationdb_coverage
  const value = incomplete(before, after, /metadata|coverage/u)
  assert.equal(value.observations.after.measurement.foundationdb_coverage, "unavailable")
})
test("a feature-on coverage claim cannot omit one audited FoundationDB operation", () => {
  const before = snapshot(), after = snapshot()
  for (const value of [before, after]) {
    value.measurement.foundationdb_coverage.operations.pop()
    value.measurement.storage_instrumented_operations.pop()
  }
  const value = incomplete(before, after, /metadata|coverage/u)
  assert.equal(value.observations.after.measurement.foundationdb_coverage, "unavailable")
})
test("arbitrary FoundationDB coverage data is rejected and never emitted as metadata", () => {
  const before = snapshot(), after = snapshot(), sentinel = "PRIVATE_FDB_KEY_ENDPOINT_OR_ERROR"
  for (const value of [before, after]) value.measurement.foundationdb_coverage.private_key = sentinel
  const value = incomplete(before, after, /metadata|coverage/u)
  assert.equal(value.observations.after.measurement.foundationdb_coverage, "unavailable")
  assert.equal(JSON.stringify(value).includes(sentinel), false)
})
