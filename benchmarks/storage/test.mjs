import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { readFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"

import {
  validateArtifact,
  W26_IOPS_MINIMUM,
  W26_IOPS_PROFILE,
} from "../../scripts/verify-w26-ozone-iops-artifact.mjs"
import { validateEvidencePacket } from "../../scripts/verify-w26-ozone-evidence-packet.mjs"
import { validateContract } from "../../scripts/verify-w26-ozone-rollout-contract.mjs"

import {
  BenchmarkTimeoutError,
  errorRecord,
  isTimeout,
  usageError,
  withTimeout,
} from "./errors.mjs"
import { foundationDbMetadataOptions, providerById, providerSummary } from "./providers.mjs"
import { cleanupOwnedPaths, helpText, parseArgs, runBenchmark, runSample, runSteadySample } from "./runner.mjs"
import { computeStats, percentile, round, roundStats } from "./stats.mjs"
import { deltaNativeSnapshots, takePhaseSnapshot, finishPhase, logPhaseSummary, STORAGE_OPERATION_NAMES, STORAGE_CALL_SEMANTICS, STORAGE_BYTE_SEMANTICS, STORAGE_ROW_SEMANTICS } from "./diagnostics.mjs"

const storageFamilyMeasurement = {
  napi_provider: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("metadata.") || name.startsWith("blocks.")), calls: "napi_dynamic_provider_method_invocations", bytes: "known_successful_block_put_input_and_get_or_migration_payload_bytes; metadata_bytes_unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  sdk_provider: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("sdk.")), calls: "direct_sdk_provider_method_invocations_including_synchronous_methods", bytes: "known_successful_block_put_input_and_get_or_migration_payload_bytes; metadata_bytes_unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  pglite_client_lock: { operations: ["pglite.client_lock_wait"], calls: "client_lock_acquisition_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_client_lock_await_nanoseconds" },
  tidb_pool_checkout: { operations: ["tidb.pool.checkout"], calls: "pool_checkout_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_checkout_nanoseconds_including_lazy_connect_and_session_configuration; queue_only_wait_unavailable" },
  tidb_session: { operations: ["tidb.session.configure"], calls: "session_configuration_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  tidb_open: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("tidb.open.")), calls: "open_schema_and_metadata_initialization_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  tidb_transaction: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("tidb.tx.")), calls: "transaction_lifecycle_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  tidb_sql: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("tidb.sql.")), calls: "categorized_sql_adapter_invocations; not_internal_requests", bytes: "known_selected_successful_payload_bytes_only; other_sql_bytes_unavailable", returned_rows: STORAGE_ROW_SEMANTICS, duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
}
const storageInstrumentedOperations = [...STORAGE_OPERATION_NAMES]
const tidbCoverageMeasurement = {
  schema: "mount-rs-tidb-client-diagnostic-coverage-v1", status: "source_sites_instrumented",
  pool_checkout_sites: "34", session_configure_sites: "1", schema_initialize_sites: "1", metadata_open_sites: "1",
  transaction_begin_sites: "3", transaction_commit_sites: "1", transaction_rollback_sites: "5", sql_statement_sites: "56",
  operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("tidb.")),
  sql_returned_rows_scope: "successful SELECT Option/Vec results only; exec_iter affected_rows excluded",
  sql_payload_bytes_scope: "successful block INSERT submitted bytes and block-body SELECT returned bytes only; other SQL payload bytes unavailable",
  pool_checkout_scope: "pool.get_conn await includes possible lazy connection and session setup; queue-only wait unavailable",
  unavailable: ["pool_queue_only_wait", "separate_driver_connect_handshake", "server_sql_execution_time", "transaction_lifetime_and_implicit_drop_rollback", "affected_rows", "other_sql_payload_bytes", "sql_wire_bytes_and_client_internal_retries", "tikv_and_physical_device_io"],
}

const rawApiNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const rawApiMeasurement = {
  schema: "mount-rs.object-store-api.v1", scope: "live_registered_split_r2_block_store_instances",
  calls: "object_store_adapter_method_invocations; not_http_attempts_or_internal_retries",
  duration: "inclusive_wall_nanoseconds_at_invoked_adapter_await; excludes_argument_preparation",
  upload_bytes: "attempted=submitted_payload; confirmed=put_opts_ok_only",
  returned_bytes: "successful_body_materialization_before_integrity_validation",
  latency_max: "cumulative_per_instance; exact_phase_max_unavailable", reconcile_listing: "unavailable",
  excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "internal_client_retries"],
}

const localWorkNames = ["sha256.digest", "block_id.encode", "copy.upload_payload", "copy.cache_insert", "copy.return_vec", "cache.lock_acquire", "put.follower_wait"]
const localWorkMeasurement = {
  schema: "mount-rs.object-store-local.v1", scope: "live_registered_split_r2_block_store_instances",
  calls: "fixed_local_adapter_work_invocations; not_backend_requests_or_allocations",
  duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap",
  input_bytes: "entered_digest_encoding_and_copy_input; waits_zero",
  output_bytes: "completed_digest_32_id_65_and_actual_copy_bytes; waits_zero",
  cache_lock_scope: "mutex_acquisition_including_wait; excludes_lock_hold_and_lru_work",
  latency_max: "cumulative_per_instance; exact_phase_max_unavailable", adapter_compression: "not_used",
  excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "client_internal_work", "cache_key_and_lru_work", "upload_claim_setup"],
}

function localWorkSnapshot(calls) {
  const instance = rawApiInstance(calls)
  instance.local_work = {
    schema: "mount-rs.object-store-local.v1", scope: "one_object_store_block_store_instance", saturated: false, in_flight: "0",
    entries: localWorkNames.map((name, index) => {
      const count = index === 0 ? calls : 0
      return { name, calls: String(count), success: String(count), error: "0", cancelled: "0", elapsed_ns: String(count * 1000), input_bytes: String(count * 4096), output_bytes: String(count * 32), latency_max_ns: count ? "1000" : "0", latency_log2_us: ["0", String(count), ...Array(30).fill("0")] }
    }),
  }
  const snapshot = diagnosticSnapshot(calls, "7", [instance])
  snapshot.measurement.r2_local = structuredClone(localWorkMeasurement)
  return snapshot
}

async function testObjectStoreLocalPhaseDiagnostics() {
  const before = localWorkSnapshot(2), after = localWorkSnapshot(5)
  const delta = deltaNativeSnapshots(before, after)
  assert.equal(delta.complete, true)
  const local = delta.r2.instances[0].local_work
  assert.equal(local?.status, "observed", "optional local evidence must survive the phase delta")
  assert.equal(local.complete, true)
  assert.deepEqual(local.entries.map((row) => row.name), localWorkNames)
  assert.equal(local.entries[0].calls, "3")
  assert.equal(local.entries[0].input_bytes, "12288")
  assert.equal(local.entries[0].output_bytes, "96")
  assert.equal(local.entries[0].latency_max_ns_start, "1000")
  assert.equal(local.entries[0].latency_max_ns_end, "1000")
  assert.equal(local.entries[0].exact_phase_max_ns, "unavailable")
  assert.deepEqual(local.entries[0].latency_log2_us, ["0", "3", ...Array(30).fill("0")])
  assert.deepEqual(delta.measurement.r2_local, localWorkMeasurement)
  const rawOnly = deltaNativeSnapshots(diagnosticSnapshot(2, "7", [rawApiInstance(2)]), diagnosticSnapshot(5, "7", [rawApiInstance(5)]))
  assert.deepEqual(delta.r2.instances[0].raw_api, rawOnly.r2.instances[0].raw_api)
  assert.deepEqual(delta.measurement.r2_api, rawOnly.measurement.r2_api)
  assert.equal(rawOnly.r2.instances[0].local_work.status, "unavailable")
  assert.equal(rawOnly.r2.instances[0].local_work.complete, false)
  const lines = [], originalWrite = process.stderr.write
  process.stderr.write = (value) => { lines.push(String(value)); return true }
  try {
    logPhaseSummary({ name: "workload-local-control", elapsed_ms: 1, native: delta })
    logPhaseSummary({ name: "workload-legacy-control", elapsed_ms: 1, native: rawOnly })
    for (const metadata of [undefined, { schema: "EXCLUDED_LOCAL_SECRET" }]) {
      const altered = structuredClone(delta)
      altered.measurement.r2_local = metadata
      logPhaseSummary({ name: "workload-local-metadata-control", elapsed_ms: 1, native: altered })
    }
  } finally { process.stderr.write = originalWrite }
  const summaries = lines.map((line) => JSON.parse(line.slice("MOUNT_RS_STORAGE_PHASE ".length)))
  assert.equal(summaries[0].local_work?.status, "observed", "the existing bounded phase log must expose local bottleneck totals")
  assert.equal(summaries[0].local_work.observed_instances, 1)
  assert.equal(summaries[0].local_work.unavailable_instances, 0)
  assert.deepEqual(summaries[0].local_work.entries.map((row) => row.name), localWorkNames)
  assert.equal(summaries[0].local_work.entries[0].input_bytes, "12288")
  assert.equal(summaries[1].local_work.status, "unavailable")
  assert.equal(summaries[1].local_work.entries, undefined)
  for (const summary of summaries.slice(2)) {
    assert.equal(summary.complete, true)
    assert.equal(summary.local_work.status, "unavailable", "valid-looking local rows require their exact local measurement provenance")
    assert.equal(summary.local_work.entries, undefined)
    assert.equal(JSON.stringify(summary).includes("EXCLUDED_LOCAL_SECRET"), false)
  }

  const malformed = [
    (snapshot) => { snapshot.r2.instances[0].local_work.saturated = true },
    (snapshot) => { snapshot.r2.instances[0].local_work.in_flight = "1" },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries.pop() },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[1].name = localWorkNames[0] },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[0].name = "EXCLUDED_LOCAL_SECRET" },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[0].calls = "1" },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[0].input_bytes = "01" },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[0].output_bytes = "0" },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[5].input_bytes = "1" },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[0].latency_log2_us[1] = "0" },
    (snapshot) => { snapshot.r2.instances[0].local_work.entries[0].latency_max_ns = "99999" },
    (snapshot) => { snapshot.measurement.r2_local.calls = "private-secret-metadata" },
    (snapshot) => { delete snapshot.measurement.r2_local },
    (snapshot) => { snapshot.r2.instances[0].local_work = null },
  ]
  for (const mutate of malformed) {
    const altered = structuredClone(after)
    mutate(altered)
    const result = deltaNativeSnapshots(before, altered)
    assert.equal(result.complete, true, "optional local failure must preserve the existing raw qualification")
    assert.equal(result.r2.instances[0].local_work.complete, false)
    assert.doesNotMatch(JSON.stringify(result), /private-secret-metadata/u)
    assert.deepEqual(result.r2.instances[0].raw_api, delta.r2.instances[0].raw_api)
  }
  for (const endpoint of [before, after]) endpoint.r2.instances[0].local_work = null
  const disabled = deltaNativeSnapshots(before, after)
  assert.equal(disabled.complete, true)
  assert.equal(disabled.r2.instances[0].local_work.status, "unavailable")
  assert.equal(disabled.r2.instances[0].local_work.complete, false)
  assert.equal(disabled.r2.instances[0].local_work.entries, undefined, "disabled evidence cannot be fabricated as zero work")

  const hugeBefore = localWorkSnapshot(2), hugeAfter = localWorkSnapshot(5)
  hugeBefore.r2.instances[0].local_work.entries[0].input_bytes = "9007199254740993"
  hugeAfter.r2.instances[0].local_work.entries[0].input_bytes = "9007199254745090"
  assert.equal(deltaNativeSnapshots(hugeBefore, hugeAfter).r2.instances[0].local_work.entries[0].input_bytes, "4097")
  const advanced = localWorkSnapshot(5)
  advanced.r2.instances[0].local_work.entries[0].latency_max_ns = "1500"
  const maximum = deltaNativeSnapshots(localWorkSnapshot(2), advanced).r2.instances[0].local_work.entries[0]
  assert.equal(maximum.latency_max_ns_end, "1500")
  assert.equal(maximum.exact_phase_max_ns, "unavailable")
  const idle = deltaNativeSnapshots(localWorkSnapshot(2), localWorkSnapshot(2)).r2.instances[0].local_work.entries[0]
  assert.equal(idle.calls, "0")
  assert.equal(idle.latency_max_ns_end, "1000")
  const openedBefore = localWorkSnapshot(0)
  openedBefore.r2.instances = []
  const opened = deltaNativeSnapshots(openedBefore, localWorkSnapshot(5)).r2.instances[0]
  assert.equal(opened.opened_during_phase, true)
  assert.equal(opened.local_work.entries[0].calls, "5", "a genuinely new instance has an explicit zero local baseline")
  const badBaseline = localWorkSnapshot(2)
  badBaseline.r2.instances[0].local_work.saturated = true
  const baselineFailure = deltaNativeSnapshots(badBaseline, localWorkSnapshot(5))
  assert.equal(baselineFailure.complete, true)
  assert.equal(baselineFailure.r2.instances[0].local_work.complete, false)
  const mixed = (calls) => {
    const snapshot = localWorkSnapshot(calls)
    snapshot.r2.instances[0].local_work.entries = localWorkNames.map((name, index) => ({
      name, calls: String(calls), success: String(index === 6 && calls === 5 ? 3 : calls), error: index === 6 && calls === 5 ? "1" : "0", cancelled: index === 6 && calls === 5 ? "1" : "0",
      elapsed_ns: String(calls * 1000), input_bytes: String(calls * (index === 1 ? 32 : index < 5 ? 4096 : 0)), output_bytes: String(calls * (index === 0 ? 32 : index === 1 ? 65 : index < 5 ? 4096 : 0)),
      latency_max_ns: "1000", latency_log2_us: ["0", String(calls), ...Array(30).fill("0")],
    }))
    return snapshot
  }
  const allRows = deltaNativeSnapshots(mixed(2), mixed(5)).r2.instances[0].local_work
  assert.equal(allRows.complete, true)
  assert.equal(allRows.entries[1].input_bytes, "96")
  assert.equal(allRows.entries[1].output_bytes, "195")
  for (const row of allRows.entries.slice(2, 5)) assert.deepEqual([row.input_bytes, row.output_bytes], ["12288", "12288"])
  for (const row of allRows.entries.slice(5)) assert.deepEqual([row.input_bytes, row.output_bytes], ["0", "0"])
  assert.deepEqual([allRows.entries[6].success, allRows.entries[6].error, allRows.entries[6].cancelled], ["1", "1", "1"])
  for (const reset of ["input_bytes", "latency_max_ns"]) {
    const endpoint = localWorkSnapshot(5)
    endpoint.r2.instances[0].local_work.entries[0][reset] = reset === "input_bytes" ? "1" : "999"
    const result = deltaNativeSnapshots(localWorkSnapshot(2), endpoint)
    assert.equal(result.complete, true)
    assert.equal(result.r2.instances[0].local_work.status, "invalid", `${reset} reset at otherwise valid endpoints must be rejected independently`)
    assert.equal(result.r2.instances[0].local_work.complete, false)
  }
}

function rawApiInstance(calls, id = "19") {
  return {
    id, puts: String(calls), gets: "0", deletes: "0", reconciles: "0", successes: String(calls), errors: "0",
    duration_ms_total: "0", duration_ms_max: "0", bytes_read: "0", bytes_written: String(calls * 4096),
    conditional_conflicts: "0", id_collision_exhausted: "0", retry_exhausted: "0", cache_hits: "0",
    raw_api: {
      schema: "mount-rs.object-store-api.v1", scope: "one_object_store_block_store_instance",
      saturated: false, in_flight: "0", pending_claims: "0",
      claims: { leader_claims: String(calls), leader_success: String(calls), leader_error: "0", leader_cancelled: "0", follower_claims: "0", follower_success: "0", follower_error: "0", follower_cancelled: "0" },
      entries: rawApiNames.map((name, index) => {
        const count = index === 0 ? calls : 0
        return { name, calls: String(count), success: String(count), error: "0", cancelled: "0", elapsed_ns: String(count * 1000), attempted_bytes: String(count * 4096), confirmed_bytes: String(count * 4096), returned_bytes: "0", latency_max_ns: count ? "1000" : "0", latency_log2_us: ["0", String(count), ...Array(30).fill("0")] }
      }),
    },
  }
}

function diagnosticSnapshot(calls, connectionId = "7", instances = []) {
  return {
    schema_version: "mount-rs.storage-diagnostics.v3", enabled: true, scope: "process", quiescent_snapshot_required: true, elapsed_semantics: "inclusive_wall_nanoseconds",
    measurement: { storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS, storage_rows: STORAGE_ROW_SEMANTICS, storage_operations: [...STORAGE_OPERATION_NAMES], storage_families: structuredClone(storageFamilyMeasurement), storage_instrumented_operations: [...storageInstrumentedOperations], tidb_coverage: structuredClone(tidbCoverageMeasurement),
      storage_duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap",
      forwarding_boxes: "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations",
      profile: "existing_core_profile_counters",
      sqlite: "live_connection_pager_and_sql_category_counters; pager_bytes_are_page_size_estimates",
      r2: "live_store_logical_calls_and_cache_hits; not_http_attempts",
      r2_api: structuredClone(rawApiMeasurement),
      unavailable: { http_attempts: "unavailable", internal_successful_retries: "unavailable", physical_device_iops: "unavailable", tidb_pool_wait: "isolated_queue_only_wait_unavailable", native_allocation_count: "unavailable", js_allocation_count: "unavailable" },
      latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, bucket) => bucket === 0
        ? { lower_inclusive_us: "0", upper_exclusive_us: "1" }
        : bucket === 31
          ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true }
          : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) }) } },
    backend_waits: { pglite_client_lock: "instrumented", tidb_pool: "instrumented_inclusive_checkout_including_lazy_connect_and_session_configuration" },
    http_attempts: "unavailable", physical_device_iops: "unavailable",
    storage: { in_flight: "0", forwarding_boxes: { sites: "napi_dynamic_provider_forwarding_future", calls: String(calls), requested_object_bytes: String(calls * 80) }, entries: STORAGE_OPERATION_NAMES.map((name) => { const count = name === "blocks.put" ? calls : 0; return { name, calls: String(count), success: String(count), error: "0", cancelled: "0", bytes: String(count * 4096), returned_rows: "0", returned_row_observations: "0", in_flight: "0", elapsed_ns: String(count * 1000), latency_log2_us: [String(count), ...Array(31).fill("0")] } }) },
    profile: { entries: [{ name: "filesystem.gate_wait", calls: String(calls), elapsed_ns: String(calls * 50), units: "0" }] },
    sqlite: { connections: [{ connection_id: connectionId, pager: { cache_hits: String(calls), cache_misses: "0", page_writes: String(calls), cache_spills: "0" }, page_size: "4096", pager_read_bytes_estimate: "0", pager_write_bytes_estimate: String(calls * 4096), sql_statements: String(calls), sql_categories: { SELECT: String(calls) } }] },
    r2: { scope: "process_live_instances", instances, internal_successful_retries: "unavailable" },
  }
}

async function testStoragePhaseDiagnostics() {
  const snapshot = diagnosticSnapshot
  const delta = deltaNativeSnapshots(snapshot(2), snapshot(5))
  assert.equal(delta.complete, true)
  assert.equal(delta.storage?.entries?.[5]?.bytes, "12288")
  assert.equal(delta.storage.forwarding_boxes.calls, "3")
  assert.equal(delta.storage.forwarding_boxes.requested_object_bytes, "240")
  assert.equal(delta.measurement.storage_duration, "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap")
  assert.equal(delta.measurement.profile, "existing_core_profile_counters")
  assert.equal(delta.backend_waits.tidb_pool, "instrumented_inclusive_checkout_including_lazy_connect_and_session_configuration")
  assert.equal(delta.measurement.unavailable.tidb_pool_wait, "isolated_queue_only_wait_unavailable")
  assert.equal(delta.profile.entries[0].elapsed_ns, "150")
  assert.equal(delta.sqlite.connections[0].pager.page_writes, "3")
  assert.equal(delta.sqlite.connections[0].sql_categories.SELECT, "3")
  assert.equal(deltaNativeSnapshots(snapshot(5), snapshot(2)).complete, false)
  assert.equal(deltaNativeSnapshots(snapshot(2), { ...snapshot(5), sqlite: { connections: [] } }).complete, false)
  assert.equal(deltaNativeSnapshots(snapshot(2), snapshot(5, "8")).complete, false)
  const unsafe = snapshot(5)
  unsafe.storage.entries[5].calls = Number.MAX_SAFE_INTEGER + 1
  assert.equal(deltaNativeSnapshots(snapshot(2), unsafe).complete, false)
  const changedHistogram = snapshot(5)
  changedHistogram.storage.entries[5].latency_log2_us.push("0")
  assert.equal(deltaNativeSnapshots(snapshot(2), changedHistogram).complete, false)
  const inflight = snapshot(5)
  inflight.storage.in_flight = "1"
  assert.equal(deltaNativeSnapshots(snapshot(2), inflight).complete, false)
  const missingMetadata = snapshot(5)
  delete missingMetadata.measurement
  assert.equal(deltaNativeSnapshots(snapshot(2), missingMetadata).complete, false)
  const resetBoxes = snapshot(5)
  resetBoxes.storage.forwarding_boxes.calls = "1"
  assert.equal(deltaNativeSnapshots(snapshot(2), resetBoxes).complete, false)
}

async function testStorageDriverFieldDeltas() {
  const before = diagnosticSnapshot(2)
  const after = diagnosticSnapshot(5)
  const sqlIndex = STORAGE_OPERATION_NAMES.indexOf("tidb.sql.metadata_read")
  Object.assign(before.storage.entries[sqlIndex], {
    in_flight: "0", calls: "2", success: "2", elapsed_ns: "2000", latency_log2_us: ["2", ...Array(31).fill("0")], returned_rows: "9007199254740993", returned_row_observations: "1",
  })
  Object.assign(after.storage.entries[sqlIndex], {
    in_flight: "0", calls: "5", success: "5", elapsed_ns: "5000", latency_log2_us: ["5", ...Array(31).fill("0")], returned_rows: "9007199254740996", returned_row_observations: "3",
  })
  const delta = deltaNativeSnapshots(before, after)
  assert.equal(delta.complete, true)
  assert.equal(delta.storage.entries[sqlIndex].returned_rows, "3", "driver rows must retain exact integer deltas")
  assert.equal(delta.storage.entries[sqlIndex].returned_row_observations, "2")
  assert.equal(delta.storage.entries[sqlIndex].in_flight_start, "0")
  assert.equal(delta.storage.entries[sqlIndex].in_flight_end, "0")
  const pending = structuredClone(after)
  pending.storage.entries[sqlIndex].in_flight = "1"
  assert.equal(deltaNativeSnapshots(before, pending).complete, false, "per-operation pending work cannot disappear behind a zero global gauge")
  const reset = structuredClone(after)
  reset.storage.entries[sqlIndex].returned_rows = "1"
  assert.equal(deltaNativeSnapshots(before, reset).complete, false)
  const noObservation = structuredClone(after)
  noObservation.storage.entries[sqlIndex].returned_row_observations = "1"
  assert.equal(deltaNativeSnapshots(before, noObservation).complete, false, "valid endpoints cannot produce rows without a known phase observation")
  const excessObservations = structuredClone(after)
  excessObservations.storage.entries[sqlIndex].returned_row_observations = "5"
  assert.equal(deltaNativeSnapshots(before, excessObservations).complete, false, "phase observations cannot exceed successful phase calls")
  const nonSql = diagnosticSnapshot(5)
  Object.assign(nonSql.storage.entries[5], { returned_rows: "1", returned_row_observations: "1" })
  assert.equal(deltaNativeSnapshots(diagnosticSnapshot(2), nonSql).complete, false, "block put success cannot claim an observed SQL result row")
}

async function testStorageFamilyMetadata() {
  const before = diagnosticSnapshot(2)
  const after = diagnosticSnapshot(5)
  const sql = after.storage.entries.find((entry) => entry.name === "tidb.sql.flush_probe")
  Object.assign(sql, { calls: "1", success: "1", elapsed_ns: "43", returned_rows: "0", returned_row_observations: "1", latency_log2_us: ["1", ...Array(31).fill("0")] })
  const delta = deltaNativeSnapshots(before, after)
  assert.equal(delta.complete, true)
  assert.deepEqual(delta.measurement.storage_families, storageFamilyMeasurement, "closed families must retain distinct byte, row and duration meanings")
  assert.deepEqual(delta.measurement.storage_instrumented_operations, storageInstrumentedOperations, "coverage must retain the producer's audited operation list")
  assert.deepEqual(delta.measurement.tidb_coverage, tidbCoverageMeasurement, "static coverage is distinct from dynamic operation counters")
  assert.equal(delta.storage.entries.length, 78)
  assert.equal(delta.storage.entries.at(-1).returned_row_observations, "1", "the last declared row must reach phase evidence")
  for (const field of ["storage_families", "storage_instrumented_operations", "tidb_coverage"]) {
    const missingBefore = structuredClone(before)
    const missingAfter = structuredClone(after)
    delete missingBefore.measurement[field]
    delete missingAfter.measurement[field]
    assert.equal(deltaNativeSnapshots(missingBefore, missingAfter).complete, false, `missing ${field} must be incomplete`)
  }
  const invalidBefore = structuredClone(before)
  const invalidAfter = structuredClone(after)
  invalidBefore.measurement.storage_families.tidb_pool_checkout.duration = "queue_wait_only"
  invalidAfter.measurement.storage_families.tidb_pool_checkout.duration = "queue_wait_only"
  const invalid = deltaNativeSnapshots(invalidBefore, invalidAfter)
  assert.equal(invalid.complete, false)
  assert.equal(invalid.observations.after.measurement.storage_families, "unavailable")
  assert.deepEqual(invalid.observations.after.measurement.storage_operations, STORAGE_OPERATION_NAMES)
  assert.equal(invalid.observations.after.measurement.storage_rows, STORAGE_ROW_SEMANTICS)
  const malformedCoverageBefore = structuredClone(before)
  const malformedCoverageAfter = structuredClone(after)
  for (const endpoint of [malformedCoverageBefore, malformedCoverageAfter]) {
    endpoint.measurement.tidb_coverage.operations.push("secret-payload-category")
    endpoint.measurement.tidb_coverage.status = "live_server_performance_proven"
  }
  const malformedCoverage = deltaNativeSnapshots(malformedCoverageBefore, malformedCoverageAfter)
  assert.equal(malformedCoverage.complete, false)
  assert.equal(malformedCoverage.observations.after.measurement.tidb_coverage, "unavailable")
  assert.equal(JSON.stringify(malformedCoverage).includes("secret-payload-category"), false, "invalid coverage must not retain arbitrary labels")
  const alteredSitesBefore = structuredClone(before)
  const alteredSitesAfter = structuredClone(after)
  alteredSitesBefore.measurement.tidb_coverage.pool_checkout_sites = "35"
  alteredSitesAfter.measurement.tidb_coverage.pool_checkout_sites = "35"
  assert.equal(deltaNativeSnapshots(alteredSitesBefore, alteredSitesAfter).complete, false, "equal endpoints cannot silently alter static coverage")
  const lines = []
  const originalWrite = process.stderr.write
  process.stderr.write = (value) => { lines.push(String(value)); return true }
  try { logPhaseSummary({ name: "workload-family-control", elapsed_ms: 1, native: delta }) }
  finally { process.stderr.write = originalWrite }
  const summary = JSON.parse(lines[0].slice("MOUNT_RS_STORAGE_PHASE ".length))
  assert.equal(summary.families.tidb_sql.elapsed_ns, "43")
  assert.equal(summary.families.tidb_sql.returned_rows, "0")
  assert.equal(summary.families.tidb_sql.returned_row_observations, "1", "known zero SQL rows differ from unknown rows")
  assert.equal(summary.families.tidb_pool_checkout.instrumented, true)
}

async function testRawObjectStorePhaseDiagnostics() {
  const snapshot = (calls) => diagnosticSnapshot(calls, "7", [rawApiInstance(calls)])
  const delta = deltaNativeSnapshots(snapshot(2), snapshot(5))
  assert.equal(delta.complete, true)
  assert.equal(delta.schema_version, "mount-rs.storage-diagnostics.v3")
  assert.equal(delta.r2.scope, "process_live_instances")
  assert.deepEqual(delta.r2.instance_ids_start, ["19"])
  assert.deepEqual(delta.r2.instance_ids_end, ["19"])
  assert.equal(delta.r2.instances[0].opened_during_phase, false)
  const raw = delta.r2.instances[0].raw_api
  assert.equal(raw.schema, "mount-rs.object-store-api.v1")
  assert.equal(raw.scope, "one_object_store_block_store_instance")
  assert.equal(raw.in_flight_start, "0")
  assert.equal(raw.pending_claims_end, "0")
  assert.equal(raw.claims.leader_claims, "3")
  assert.equal(raw.entries[0].calls, "3")
  assert.equal(raw.entries[0].confirmed_bytes, "12288")
  assert.equal(raw.entries[0].latency_max_ns_start, "1000")
  assert.equal(raw.entries[0].latency_max_ns_end, "1000")
  assert.equal(raw.entries[0].exact_phase_max_ns, "unavailable")
  assert.deepEqual(raw.entries[0].latency_log2_us, ["0", "3", ...Array(30).fill("0")])
  const advanced = snapshot(5)
  advanced.r2.instances[0].raw_api.entries[0].latency_max_ns = "1500"
  const maximum = deltaNativeSnapshots(snapshot(2), advanced).r2.instances[0].raw_api.entries[0]
  assert.equal(maximum.latency_max_ns_end, "1500", "a cumulative maximum must never be subtracted")
  assert.equal(maximum.exact_phase_max_ns, "unavailable")
  const unchanged = deltaNativeSnapshots(snapshot(2), snapshot(2)).r2.instances[0].raw_api.entries[0]
  assert.equal(unchanged.calls, "0")
  assert.equal(unchanged.latency_max_ns_end, "1000", "an idle phase retains its lifetime maximum")

  const semanticBefore = diagnosticSnapshot(0, "7", [rawApiInstance(0)])
  const uncertain = diagnosticSnapshot(2, "7", [rawApiInstance(2)])
  const uncertainRaw = uncertain.r2.instances[0].raw_api
  Object.assign(uncertainRaw.entries[0], { success: "0", error: "1", cancelled: "1", confirmed_bytes: "0" })
  Object.assign(uncertainRaw.claims, { leader_success: "0", leader_error: "1", leader_cancelled: "1" })
  assert.equal(deltaNativeSnapshots(semanticBefore, uncertain).complete, true, "errors and cancellation retain attempted bytes without confirming writes")
  const integrityFailure = diagnosticSnapshot(1, "7", [rawApiInstance(1)])
  const integrityRaw = integrityFailure.r2.instances[0].raw_api
  Object.assign(integrityRaw.claims, { leader_success: "0", leader_error: "1" })
  Object.assign(integrityRaw.entries[0], { success: "0", error: "1", confirmed_bytes: "0" })
  for (const index of [3, 4]) Object.assign(integrityRaw.entries[index], { calls: "1", success: "1", elapsed_ns: "1000", latency_max_ns: "1000", latency_log2_us: ["0", "1", ...Array(30).fill("0")] })
  integrityRaw.entries[4].returned_bytes = "4096"
  const integrityDelta = deltaNativeSnapshots(semanticBefore, integrityFailure)
  assert.equal(integrityDelta.complete, true, "successful raw body bytes remain observable after claim integrity failure")
  assert.equal(integrityDelta.r2.instances[0].raw_api.entries[4].returned_bytes, "4096")
  const followers = diagnosticSnapshot(5, "7", [rawApiInstance(2)])
  Object.assign(followers.r2.instances[0].raw_api.claims, { leader_success: "1", leader_error: "1", follower_claims: "3", follower_success: "2", follower_error: "1" })
  Object.assign(followers.r2.instances[0].raw_api.entries[0], { success: "1", error: "1", confirmed_bytes: "4096" })
  const followerDelta = deltaNativeSnapshots(semanticBefore, followers)
  assert.equal(followerDelta.complete, true, "followers need not invoke an additional raw API method")
  assert.equal(followerDelta.r2.instances[0].raw_api.entries[0].calls, "2")
  assert.equal(followerDelta.r2.instances[0].raw_api.claims.follower_claims, "3")

  const badCases = [
    ["missing raw", (value) => { delete value.r2.instances[0].raw_api }],
    ["null raw", (value) => { value.r2.instances[0].raw_api = null }],
    ["number counter", (value) => { value.r2.instances[0].raw_api.entries[0].calls = 5 }],
    ["noncanonical decimal", (value) => { value.r2.instances[0].raw_api.entries[0].calls = "05" }],
    ["u64 overflow", (value) => { value.r2.instances[0].raw_api.entries[0].elapsed_ns = "18446744073709551616" }],
    ["saturated endpoint", (value) => { value.r2.instances[0].raw_api.saturated = true }],
    ["saturated counter", (value) => { value.r2.instances[0].raw_api.entries[0].elapsed_ns = "18446744073709551615" }],
    ["missing saturation", (value) => { delete value.r2.instances[0].raw_api.saturated }],
    ["counter reset", (value) => { value.r2.instances[0].raw_api.entries[0].attempted_bytes = "1" }],
    ["maximum reset", (value) => { value.r2.instances[0].raw_api.entries[0].latency_max_ns = "999" }],
    ["duplicate instance", (value) => { value.r2.instances.push(structuredClone(value.r2.instances[0])) }],
    ["number identity", (value) => { value.r2.instances[0].id = 19 }],
    ["dropped instance", (value) => { value.r2.instances = [] }],
    ["missing registry", (value) => { delete value.r2.instances }],
    ["registry unavailable", (value) => { value.r2.available = false }],
    ["row order", (value) => { value.r2.instances[0].raw_api.entries.reverse() }],
    ["row name", (value) => { value.r2.instances[0].raw_api.entries[0].name = "put_opts.secret-key" }],
    ["row missing", (value) => { value.r2.instances[0].raw_api.entries.pop() }],
    ["row outcomes", (value) => { value.r2.instances[0].raw_api.entries[0].success = "4" }],
    ["histogram shape", (value) => { value.r2.instances[0].raw_api.entries[0].latency_log2_us.pop() }],
    ["histogram total", (value) => { value.r2.instances[0].raw_api.entries[0].latency_log2_us[1] = "4" }],
    ["claim outcomes", (value) => { value.r2.instances[0].raw_api.claims.leader_success = "4" }],
    ["claim missing", (value) => { delete value.r2.instances[0].raw_api.claims.follower_cancelled }],
    ["raw in flight", (value) => { value.r2.instances[0].raw_api.in_flight = "1" }],
    ["pending claims", (value) => { value.r2.instances[0].raw_api.pending_claims = "1" }],
    ["byte applicability", (value) => { value.r2.instances[0].raw_api.entries[1].returned_bytes = "1" }],
    ["unconfirmed success bytes", (value) => { value.r2.instances[0].raw_api.entries[0].confirmed_bytes = "999999" }],
    ["metadata change", (value) => { value.measurement.r2_api.reconcile_listing = "instrumented" }],
    ["native scope change", (value) => { value.scope = "private-backend" }],
    ["schema v1", (value) => { value.schema_version = "mount-rs.storage-diagnostics.v1" }],
  ]
  for (const [label, mutate] of badCases) {
    const after = snapshot(5)
    mutate(after)
    const incomplete = deltaNativeSnapshots(snapshot(2), after)
    assert.equal(incomplete.complete, false, label)
    assert.ok(incomplete.observations?.before && incomplete.observations?.after, `${label}: preserve sanitized endpoint observations`)
  }
  const malformedBaseline = snapshot(2)
  malformedBaseline.r2.instances[0].raw_api.claims.follower_claims = "1"
  assert.equal(deltaNativeSnapshots(malformedBaseline, snapshot(5)).complete, false, "validate the baseline itself")
  const malicious = snapshot(5)
  malicious.r2.instances[0].id = "https://user:secret@example.test/key"
  malicious.r2.instances[0].raw_api.entries[0].name = "private-key"
  malicious.r2.instances[0].raw_api.entries[0].elapsed_ns = "private-counter"
  malicious.measurement.private_url = "https://secret@example.test"
  malicious.r2.error = "private-error"
  const evidence = JSON.stringify(deltaNativeSnapshots(snapshot(2), malicious))
  for (const secret of ["secret", "private-key", "private-counter", "private-error", "private_url", "https:"]) assert.equal(evidence.includes(secret), false, secret)
  const privateBefore = snapshot(2)
  const privateAfter = snapshot(5)
  for (const value of [privateBefore, privateAfter]) {
    value.storage.entries[5].name = "private-storage-label"
    value.profile.entries[0].name = "private-profile-label"
    value.sqlite.connections[0].sql_categories = { "private-sql-label": "1" }
  }
  privateAfter.storage.in_flight = "1"
  const partial = JSON.stringify(deltaNativeSnapshots(privateBefore, privateAfter))
  for (const secret of ["private-storage-label", "private-profile-label", "private-sql-label"]) assert.equal(partial.includes(secret), false, secret)
  const oversized = snapshot(5)
  oversized.r2.instances = Array.from({ length: 100 }, (_, index) => rawApiInstance(5, String(index + 1)))
  oversized.r2.instances[0].raw_api.entries[0].calls = "invalid"
  assert.equal(deltaNativeSnapshots(snapshot(2), oversized).observations.truncated, true)

  const samplers = { now: () => 0, cpu: () => ({ user: 0, system: 0 }), resources: () => ({ voluntaryContextSwitches: 0, involuntaryContextSwitches: 0 }), memory: () => ({ rss: 0 }) }
  const endpoint = (value) => takePhaseSnapshot(() => JSON.stringify(value), samplers)
  const openedBefore = diagnosticSnapshot(2)
  assert.equal(finishPhase("create", endpoint(openedBefore), endpoint(snapshot(5))).native.complete, true)
  const workload = finishPhase("workload-4096bytes", endpoint(openedBefore), endpoint(snapshot(5)))
  assert.equal(workload.native.complete, false, "a measured workload requires stable instances")
  assert.equal(workload.native.r2.instances[0].opened_during_phase, true)
  assert.ok(workload.native.observations)
  const shutdown = finishPhase("shutdown", endpoint(snapshot(2)), endpoint(diagnosticSnapshot(5)))
  assert.equal(shutdown.native.complete, false)
  assert.deepEqual(shutdown.native.r2.missing_instance_ids, ["19"])
}

async function testObserverEndpointsExcludeSnapshotWork() {
  const order = []
  let tick = 0
  const samplers = {
    now: () => { order.push("wall"); return ++tick },
    cpu: () => { order.push("cpu"); return { user: ++tick, system: ++tick } },
    resources: () => { order.push("resources"); return { voluntaryContextSwitches: ++tick, involuntaryContextSwitches: ++tick } },
    memory: () => { order.push("memory"); return { rss: ++tick } },
  }
  const snapshot = () => { order.push("native"); return "{}" }
  const before = takePhaseSnapshot(snapshot, samplers)
  assert.deepEqual(order, ["wall", "cpu", "resources", "native", "memory", "resources", "cpu", "wall"])
  order.length = 0
  const after = takePhaseSnapshot(snapshot, samplers)
  const phase = finishPhase("observer-control", before, after)
  assert.equal(phase.elapsed_ms, after.started - before.ended)
  assert.equal(phase.process.cpu_work.user_us, String(after.cpuStart.user - before.cpuEnd.user))
  assert.equal(phase.observer_snapshot_ms, (before.ended - before.started) + (after.ended - after.started))
}

async function testOzoneMetricsReachAllNodeProcesses() {
  const workflow = await readFile(fileURLToPath(new URL("../../.github/workflows/ci.yml", import.meta.url)), "utf8")
  const foundation = await readFile(fileURLToPath(new URL("../../scripts/test-foundationdb.sh", import.meta.url)), "utf8")
  assert.equal((workflow.match(/MOUNT_RS_PROFILE_IO: '1'/gu) || []).length, 3)
  assert.equal((foundation.match(/--env MOUNT_RS_PROFILE_IO \\/gu) || []).length, 2)
}

async function testPendingProviderInvalidatesFollowingPhaseAttribution() {
  const definitions = providerById({})
  definitions.get("mount-rs-memory").create = async () => ({
    filesystem: { writeFile: () => new Promise(() => {}), unlink: async () => {} },
    cleanup: async () => {},
  })
  definitions.get("mount-rs-sqlite").create = async () => ({
    filesystem: { writeFile: async () => {}, readFile: async () => Buffer.from([1]), unlink: async () => {} },
    cleanup: async () => {},
  })
  const options = parseArgs(["--providers", "mount-rs-memory,mount-rs-sqlite", "--sizes", "1", "--payload-bytes", "1", "--iterations", "1", "--concurrency", "1", "--timeout-ms", "5", "--cleanup-timeout-ms", "5"])
  const originalWrite = process.stderr.write
  const summaries = []
  process.stderr.write = (text) => { summaries.push(String(text)); return true }
  let result
  try { result = await runBenchmark(options, { MOUNT_RS_PROFILE_IO: "1" }, definitions) }
  finally { process.stderr.write = originalWrite }
  assert.equal(summaries.filter((line) => line.startsWith("MOUNT_RS_STORAGE_PHASE ")).length, 8)
  assert.equal(result.providers[0].cleanup.resource.status, "deferred")
  assert.equal(result.providers[1].cleanup.resource.status, "ok")
  const secondPhases = result.providers[1].storageDiagnostics.phases
  assert.ok(secondPhases.length > 0)
  assert.ok(secondPhases.every((phase) => !phase.native.complete && phase.native.issues.includes("prior provider cleanup or native operation incomplete")))
}

async function testStats() {
  assert.deepEqual(computeStats([]), { median: 0, p95: 0, p99: 0 })
  assert.deepEqual(computeStats([4, 1, 3, 2]), { median: 2.5, p95: 4, p99: 4 })

  const values = Array.from({ length: 20 }, (_, index) => index + 1)
  assert.deepEqual(computeStats(values), { median: 10.5, p95: 19, p99: 19 })
  assert.equal(percentile([10, 20, 30], 0), 10)
  assert.equal(percentile([10, 20, 30], 95), 30)
  assert.equal(round(1.236), 1.24)
  assert.deepEqual(roundStats({ median: 1.236, p95: 2.345, p99: 3.456 }), {
    median: 1.24,
    p95: 2.35,
    p99: 3.46,
  })

  assert.throws(() => computeStats([1, Number.NaN]), /finite/)
  assert.throws(() => percentile([1], 101), /between 0 and 100/)
}

async function testErrors() {
  assert.equal(await withTimeout(Promise.resolve("ok"), 50, "ready"), "ok")
  await assert.rejects(
    withTimeout(Promise.reject(Object.assign(new Error("boom"), { code: "EIO" })), 50, "read"),
    (error) => {
      assert.equal(error.code, "EIO")
      return true
    },
  )

  const timeout = withTimeout(new Promise(() => {}), 5, "download")
  await assert.rejects(timeout, (error) => {
    assert.equal(error instanceof BenchmarkTimeoutError, true)
    assert.equal(error.code, "BENCHMARK_TIMEOUT")
    assert.equal(error.operation, "download")
    assert.equal(error.timeoutMs, 5)
    assert.equal(isTimeout(error), true)
    return true
  })

  const nativeLike = Object.assign(new Error("missing"), {
    code: "ENOENT",
    errno: -2,
    syscall: "open",
    path: "/owned-by-test",
  })
  assert.deepEqual(errorRecord(nativeLike), {
    name: "Error",
    message: "missing",
    code: "ENOENT",
    errno: -2,
    syscall: "open",
    path: "/owned-by-test",
  })
  assert.equal(errorRecord("plain").message, "plain")
  const secretError = Object.assign(
    new Error("request failed for https://user:pass@example.test/object?token=secret"),
    { path: "https://example.test/object?secret_access_key=secret" },
  )
  const redacted = JSON.stringify(errorRecord(secretError))
  assert.equal(redacted.includes("pass@example"), false)
  assert.equal(redacted.includes("=secret"), false)
  assert.equal(usageError("bad").code, "BENCHMARK_USAGE")
}

async function testDeferredWriteCleanup() {
  let writeSettled = false
  let pathPresent = false
  let unlinkCalls = 0
  const filesystem = {
    writeFile() {
      return new Promise((resolve) => {
        setTimeout(() => {
          writeSettled = true
          pathPresent = true
          resolve()
        }, 25)
      })
    },
    unlink() {
      unlinkCalls += 1
      if (!pathPresent) {
        return Promise.reject(Object.assign(new Error("missing"), { code: "ENOENT" }))
      }
      pathPresent = false
      return Promise.resolve()
    },
  }
  const ownedPaths = new Set()
  const pendingOperations = new Map()
  const sample = await runSample({
    definition: { id: "injected-deferred-write" },
    filesystem,
    payload: Buffer.from("payload"),
    path: "/deferred-write",
    iteration: 1,
    options: { timeoutMs: 5, cleanupTimeoutMs: 10 },
    ownedPaths,
    pendingOperations,
  })
  assert.equal(sample.status, "failed")
  assert.equal(sample.cleanupSucceeded, false)
  assert.equal(sample.cleanupDeferred, true)
  assert.equal(sample.lateOperations[0].operation, "write")
  assert.equal(sample.lateOperations[0].status, "pending")
  assert.equal(unlinkCalls, 0)
  assert.equal(ownedPaths.has("/deferred-write"), true)

  const cleanup = await cleanupOwnedPaths(filesystem, ownedPaths, pendingOperations, 100)
  assert.equal(writeSettled, true)
  assert.equal(unlinkCalls, 1)
  assert.equal(cleanup.remaining, 0)
  assert.deepEqual(cleanup.failures, [])
}

async function testCli() {
  assert.equal(parseArgs(["--layout", "inode", "--workload", "steady-overwrite"]).layout, "inode")
  assert.equal(parseArgs(["--layout", "compact"]).layout, "compact")
  assert.match(helpText(), /--layout legacy\|inode\|compact/u)
  assert.equal(parseArgs(["--workload", "steady-overwrite"]).workload, "steady-overwrite")
  assert.throws(() => parseArgs(["--layout", "unknown"]), /layout/)
  assert.throws(() => parseArgs(["--workload", "unknown"]), /workload/)
  assert.equal(parseArgs(["--workload", "steady-overwrite", "--payload-bytes", "1", "--iterations", "255"]).iterations, 255)
  assert.throws(() => parseArgs(["--workload", "steady-overwrite", "--payload-bytes", "1", "--iterations", "256"]), /generation capacity/)
  assert.equal(parseArgs(["--workload", "lifecycle", "--payload-bytes", "1", "--iterations", "256"]).iterations, 256)

  assert.deepEqual(parseArgs(["--smoke"]).sizes, [1])
  assert.deepEqual(parseArgs(["--sizes", "1,4MiB,10MB,16", "--iterations", "3", "--concurrency", "2"]), {
    help: false,
    mode: "full",
    layout: "legacy",
    workload: "lifecycle",
    sizes: [1, 4, 10, 16],
    iterations: 3,
    concurrency: 2,
    timeoutMs: 30_000,
    cleanupTimeoutMs: 10_000,
    chunkSizeBytes: 65_536,
    providers: null,
    output: undefined,
    payloadSeed: "mount-rs-storage-benchmark",
    networkContext: "not-provided",
    payloadBytes: undefined,
    minIops: undefined,
    requireConfigured: false,
  })
  assert.deepEqual(parseArgs(["--smoke", "--providers", "mount-rs-memory,mountx-memory"]).providers, [
    "mount-rs-memory",
    "mountx-memory",
  ])
  assert.equal(parseArgs(["--payload-bytes", "4096", "--min-iops", "1000"]).payloadBytes, 4096)
  assert.equal(parseArgs(["--payload-bytes", "4096", "--min-iops", "1000"]).minIops, 1000)
  assert.equal(parseArgs(["--require-configured"]).requireConfigured, true)
  assert.throws(() => parseArgs(["--iterations", "0"]), /positive integer/)
  assert.throws(() => parseArgs(["--unknown"]), /unknown argument/)
}

async function testCompactRejectsNonSplitBeforeProviderIo() {
  await assert.rejects(
    runBenchmark(
      {
        ...parseArgs([]),
        layout: "compact",
        providers: ["mount-rs-memory"],
      },
      {},
    ),
    (error) => {
      assert.equal(error.code, "BENCHMARK_USAGE")
      assert.match(error.message, /compact layout requires mount-rs split storage providers/u)
      return true
    },
  )
}

async function testSplitFactoryLayoutOptions() {
  const capturePath = fileURLToPath(new URL("./capture-native.cjs", import.meta.url))
  const previousNativePath = process.env.NAPI_RS_NATIVE_LIBRARY_PATH
  process.env.NAPI_RS_NATIVE_LIBRARY_PATH = capturePath
  try {
    const environment = {
      MOUNT_RS_PGLITE_DATABASE_URL: "postgres://pglite.example.test/storage",
      MOUNT_RS_TIDB_URL: "mysql://tidb.example.test/storage",
      MOUNT_RS_NAPI_FOUNDATIONDB: "1",
      MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE: "/owned/fdb.cluster",
      MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER: "1",
      MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX: "owned-authority",
      MOUNT_RS_R2_ENDPOINT: "https://r2.example.test",
      MOUNT_RS_R2_BUCKET: "bucket",
      MOUNT_RS_R2_ACCESS_KEY_ID: "access-key",
      MOUNT_RS_R2_SECRET_ACCESS_KEY: "secret-key",
    }
    const definitions = providerById(environment)
    const splitProviders = [
      "mount-rs-split-sqlite",
      "mount-rs-split-pglite",
      "mount-rs-split-pglite-r2",
      "mount-rs-split-sqlite-r2",
      "mount-rs-split-tidb-r2",
      "mount-rs-split-foundationdb-r2",
    ]
    const require = createRequire(import.meta.url)
    const capture = require(capturePath)

    for (const layout of ["legacy", "inode", "compact"]) {
      for (const provider of splitProviders) {
        const before = capture.calls.length
        const opened = await definitions.get(provider).create({
          layout,
          runId: `${layout}-${provider}`,
          environment,
          chunkSizeBytes: 65_536,
        })
        assert.equal(capture.calls.length, before + 1, `${provider} must construct one NAPI driver`)
        const options = capture.calls.at(-1)
        const selected = Object.fromEntries(
          ["concurrentWrites", "inodeUpdates", "compactInodeUpdates"]
            .filter((key) => key in options)
            .map((key) => [key, options[key]]),
        )
        assert.deepEqual(
          selected,
          layout === "legacy"
            ? {}
            : layout === "inode"
              ? { concurrentWrites: true, inodeUpdates: true }
              : {
                  concurrentWrites: true,
                  inodeUpdates: true,
                  compactInodeUpdates: true,
                },
          `${provider} ${layout} layout options`,
        )
        await opened.cleanup()
      }
    }
  } finally {
    if (previousNativePath === undefined) delete process.env.NAPI_RS_NATIVE_LIBRARY_PATH
    else process.env.NAPI_RS_NATIVE_LIBRARY_PATH = previousNativePath
  }
}

async function testFoundationDbAvailabilityFollowsSelectedLayout() {
  const capturePath = fileURLToPath(new URL("./capture-native.cjs", import.meta.url))
  const require = createRequire(import.meta.url)
  const capture = require(capturePath)
  const environment = {
    MOUNT_RS_NAPI_FOUNDATIONDB: "1",
    MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE: "/owned/fdb.cluster",
    MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER: "1",
    MOUNT_RS_R2_ENDPOINT: "https://r2.example.test",
    MOUNT_RS_R2_BUCKET: "bucket",
    MOUNT_RS_R2_ACCESS_KEY_ID: "access-key",
    MOUNT_RS_R2_SECRET_ACCESS_KEY: "secret-key",
  }
  const definition = providerById(environment).get("mount-rs-split-foundationdb-r2")

  for (const layout of ["legacy", "inode", "compact"]) {
    const availability = definition.availability(environment, { layout })
    const requiredEnvVars =
      typeof definition.requiredEnvVars === "function"
        ? definition.requiredEnvVars({ layout })
        : definition.requiredEnvVars
    assert.equal(availability.configured, layout !== "legacy", `${layout} availability`)
    assert.equal(
      availability.missing.includes("MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX"),
      layout === "legacy",
      `${layout} availability prefix requirement`,
    )
    assert.equal(
      requiredEnvVars.includes("MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX"),
      layout === "legacy",
      `${layout} artifact prefix requirement`,
    )
  }

  const withPrefix = {
    ...environment,
    MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX: "owned-authority",
  }
  const configuredDefinition = providerById(withPrefix).get("mount-rs-split-foundationdb-r2")
  for (const layout of ["legacy", "inode", "compact"]) {
    assert.equal(
      configuredDefinition.availability(withPrefix, { layout }).configured,
      true,
      `${layout} availability with prefix`,
    )
  }

  const legacyCalls = capture.calls.length
  const legacy = await runBenchmark(
    parseArgs([
      "--layout",
      "legacy",
      "--providers",
      "mount-rs-split-foundationdb-r2",
      "--sizes",
      "1",
      "--payload-bytes",
      "1",
      "--iterations",
      "1",
      "--require-configured",
    ]),
    environment,
  )
  assert.equal(capture.calls.length, legacyCalls, "legacy missing prefix must reject before NAPI I/O")
  assert.equal(legacy.status, "failed")
  assert.equal(legacy.counts.providersSkipped, 1)
  assert.equal(legacy.counts.configurationFailures, 1)
  assert.ok(
    legacy.configurationFailures[0].missingConfiguration.includes(
      "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX",
    ),
  )

  for (const layout of ["inode", "compact"]) {
    const expectedError = Object.assign(new Error(`${layout} capability sentinel`), {
      code: "CAPABILITY_SENTINEL",
    })
    capture.failNext(expectedError)
    const before = capture.calls.length
    const artifact = await runBenchmark(
      parseArgs([
        "--layout",
        layout,
        "--providers",
        "mount-rs-split-foundationdb-r2",
        "--sizes",
        "1",
        "--payload-bytes",
        "1",
        "--iterations",
        "1",
      ]),
      environment,
    )
    assert.equal(capture.calls.length, before + 1, `${layout} must reach NAPI constructor`)
    assert.equal(artifact.status, "failed", `${layout} capability failure must fail the run`)
    assert.equal(artifact.counts.providersSkipped, 0)
    assert.equal(artifact.counts.configurationFailures, 0)
    assert.equal(artifact.providers[0].status, "failed")
    assert.equal(artifact.providers[0].setupError.code, "CAPABILITY_SENTINEL")
    assert.equal(
      artifact.providers[0].requiredEnvVars.includes(
        "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX",
      ),
      false,
    )
  }
}

async function testCompactArtifactSeparatesSelectionFromPersistedProof() {
  const artifact = await runBenchmark(
    {
      ...parseArgs([]),
      layout: "compact",
      providers: ["mount-rs-split-sqlite"],
      sizes: [1],
      payloadBytes: 1,
      iterations: 1,
      timeoutMs: 100,
      cleanupTimeoutMs: 100,
    },
    {},
  )
  assert.deepEqual(artifact.providers[0].layoutSelection, {
    requested: "compact",
    selected: "compact",
    selectionEvidence: "createChunkedDriver-constructor-accepted",
    persistedMarkerEvidence: "not-observed-by-benchmark-runner",
  })
}

async function testRequiredProviderConfiguration() {
  const result = await runBenchmark(
    parseArgs([
      "--providers",
      "mount-rs-split-tidb-r2",
      "--sizes",
      "1",
      "--iterations",
      "1",
      "--require-configured",
    ]),
    {},
  )
  assert.equal(result.status, "failed")
  assert.equal(result.counts.providersSkipped, 1)
  assert.equal(result.configurationFailures[0].provider, "mount-rs-split-tidb-r2")
  assert.ok(
    result.configurationFailures[0].missingConfiguration.includes(
      "MOUNT_RS_TIDB_URL (or TIDB_URL)",
    ),
  )
  assert.ok(
    result.configurationFailures[0].missingConfiguration.includes(
      "MOUNT_RS_R2_ENDPOINT (or R2_ENDPOINT)",
    ),
  )
}

const W26_TEST_REVISION = "a".repeat(40)

function qualificationArtifact(
  provider = "mount-rs-split-sqlite-r2",
  revision = W26_TEST_REVISION,
) {
  const providers = Array.isArray(provider) ? provider : [provider]
  const size = {
    status: "ok",
    summary: {
      writeMs: { median: 1, p95: 2, p99: 3 },
      readMs: { median: 1, p95: 2, p99: 3 },
      throughputMbps: { median: 1, p95: 2, p99: 3 },
      deleteMs: { median: 1, p95: 2, p99: 3 },
      successRate: 1,
      elapsedMs: 1_000,
      operationsPerLifecycle: 3,
      successfulIterations: W26_IOPS_PROFILE.iterations,
      failedIterations: 0,
      successfulOperations: W26_IOPS_PROFILE.iterations * 3,
      attemptedOperations: W26_IOPS_PROFILE.iterations * 3,
      iops: 1_200,
      iopsTarget: W26_IOPS_MINIMUM,
      iopsTargetMet: true,
      timeoutCount: 0,
      cleanupFailureCount: 0,
      operationSuccess: {
        write: W26_IOPS_PROFILE.iterations,
        read: W26_IOPS_PROFILE.iterations,
        delete: W26_IOPS_PROFILE.iterations,
        verifiedReads: W26_IOPS_PROFILE.iterations,
      },
      statSampleCounts: {
        writeMs: W26_IOPS_PROFILE.iterations,
        readMs: W26_IOPS_PROFILE.iterations,
        throughputMbps: W26_IOPS_PROFILE.iterations,
        deleteMs: W26_IOPS_PROFILE.iterations,
      },
    },
  }
  return {
    schemaVersion: "mount-rs.storage-benchmark.v1",
    status: "ok",
    environment: {
      sourceControl: {
        mountRs: {
          revision,
          revisionVerified: true,
          dirty: false,
          dirtyEntryCount: 0,
        },
      },
    },
    config: {
      sizesMiB: [1],
      payloadSizesBytes: [W26_IOPS_PROFILE.payloadBytes],
      payloadBytes: W26_IOPS_PROFILE.payloadBytes,
      iterations: W26_IOPS_PROFILE.iterations,
      concurrency: W26_IOPS_PROFILE.concurrency,
      minIops: W26_IOPS_MINIMUM,
      requireConfigured: true,
    },
    counts: {
      providersRequested: providers.length,
      providersFailed: 0,
      providersSkipped: 0,
      configurationFailures: 0,
      sizeResultsFailed: 0,
      sizeResultsSkipped: 0,
    },
    configurationFailures: [],
    providers: providers.map((providerId) => ({
      provider: providerId,
      status: "ok",
      cleanup: {
        remainingPaths: 0,
        failures: [],
        resource: { status: "ok" },
      },
      sizes: [size],
    })),
  }
}

async function testQualificationArtifact() {
  const artifact = qualificationArtifact()
  assert.deepEqual(
    validateArtifact(artifact, {
      providers: ["mount-rs-split-sqlite-r2"],
      minimumIops: W26_IOPS_MINIMUM,
    }),
    {
      providers: ["mount-rs-split-sqlite-r2"],
      minimumIops: W26_IOPS_MINIMUM,
      sizesMiB: [1],
      profile: { ...W26_IOPS_PROFILE },
    },
  )
  for (const extra of [{ layout: "inode" }, { workload: "steady-overwrite" }]) {
    assert.throws(() => validateArtifact({ ...artifact, config: { ...artifact.config, ...extra } }, { providers: ["mount-rs-split-sqlite-r2"], minimumIops: W26_IOPS_MINIMUM }), /layout|workload/)
  }
  assert.throws(
    () => validateArtifact({ ...artifact, config: { ...artifact.config, minIops: 999 } }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /config\.minIops-must-be-safe-integer-at-least-1000/,
  )
  assert.throws(
    () => validateArtifact({ ...artifact, counts: { ...artifact.counts, providersSkipped: 1 } }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /counts\.providersSkipped-must-equal-0/,
  )
  assert.throws(
    () => validateArtifact({
      ...artifact,
      config: { ...artifact.config, payloadSizesBytes: [8192] },
    }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /config\.payloadSizesBytes-must-match-fixed-payload-profile/,
  )
  assert.throws(
    () => validateArtifact({
      ...artifact,
      providers: [{
        ...artifact.providers[0],
        sizes: [{
          ...artifact.providers[0].sizes[0],
          summary: { ...artifact.providers[0].sizes[0].summary, timeoutCount: 1 },
        }],
      }],
    }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /size\.summary\.timeoutCount-must-equal-0/,
  )
}

function rawQualificationArtifact(providers = ["mount-rs-split-sqlite-r2"], sizes = [1]) {
  const artifact = qualificationArtifact(providers)
  artifact.config.storageDiagnosticsEnabled = true
  artifact.config.sizesMiB = sizes
  artifact.config.payloadSizesBytes = sizes.map(() => W26_IOPS_PROFILE.payloadBytes)
  const definitions = providerById({})
  for (const provider of artifact.providers) {
    Object.assign(provider, providerSummary(definitions.get(provider.provider)))
    const original = provider.sizes[0]
    provider.sizes = sizes.map((sizeMiB) => ({ ...structuredClone(original), sizeMiB, fileSizeBytes: 4096, writePayloadBytes: 4096 }))
    provider.storageDiagnostics = { enabled: true, scope: "process; quiescent boundaries required", phases: sizes.map(() => {
      const native = diagnosticSnapshot(3, "7", [rawApiInstance(3)])
      native.complete = true
      native.issues = []
      native.storage.in_flight_start = "0"
      native.storage.in_flight_end = "0"
      for (const row of native.storage.entries) {
        Object.assign(row, { in_flight_start: "0", in_flight_end: "0" })
        delete row.in_flight
      }
      Object.assign(native.r2, { complete: true, instance_ids_start: ["19"], instance_ids_end: ["19"], missing_instance_ids: [] })
      for (const instance of native.r2.instances) {
        instance.opened_during_phase = false
        const raw = instance.raw_api
        Object.assign(raw, { in_flight_start: "0", in_flight_end: "0", pending_claims_start: "0", pending_claims_end: "0", saturated_start: false, saturated_end: false })
        delete raw.in_flight
        delete raw.pending_claims
        delete raw.saturated
        for (const row of raw.entries) {
          Object.assign(row, { latency_max_ns_start: row.latency_max_ns, latency_max_ns_end: row.latency_max_ns, exact_phase_max_ns: "unavailable" })
          delete row.latency_max_ns
        }
      }
      return { name: "workload-4096bytes", quiescent: true, native }
    }) }
  }
  return artifact
}

async function testQualificationProviderCoverage() {
  const expected = ["mount-rs-split-sqlite-r2", "mount-rs-split-pglite-r2"]
  const omittedPglite = rawQualificationArtifact(expected)
  omittedPglite.providers[1] = structuredClone(omittedPglite.providers[0])
  const retained = JSON.stringify(omittedPglite)
  assert.throws(
    () => validateArtifact(omittedPglite, { providers: expected }),
    /providers-do-not-match-requested-provider-set/,
    "duplicate valid SQLite records must not replace required PGlite throughput and raw evidence",
  )
  assert.equal(JSON.stringify(omittedPglite), retained, "failed coverage must preserve the artifact")
  const complete = rawQualificationArtifact(expected, [1, 4])
  complete.providers.reverse()
  assert.doesNotThrow(() => validateArtifact(complete, { providers: expected }), "distinct providers retain full coverage regardless of artifact order")
  assert.doesNotThrow(() => validateArtifact(qualificationArtifact(expected), { providers: expected }), "default provider coverage remains valid")
  assert.throws(() => validateArtifact(rawQualificationArtifact(), { providers: [expected[0], expected[0]] }), /providers-must-contain-unique-providers/)
}

async function testRawQualificationArtifact() {
  const options = { providers: ["mount-rs-split-sqlite-r2"] }
  const missing = qualificationArtifact()
  missing.config.storageDiagnosticsEnabled = true
  assert.throws(() => validateArtifact(missing, options), /diagnostic/, "opt-in throughput alone must not pass")
  assert.deepEqual(validateArtifact(rawQualificationArtifact(), options), validateArtifact(qualificationArtifact(), options))
  assert.doesNotThrow(() => validateArtifact(rawQualificationArtifact(options.providers, [1, 4]), options), "repeated workload names represent distinct configured results")
  const providerEnabled = rawQualificationArtifact()
  delete providerEnabled.config.storageDiagnosticsEnabled
  assert.doesNotThrow(() => validateArtifact(providerEnabled, options))
  const phase = (value) => value.providers[0].storageDiagnostics.phases[0]
  const row = (value) => phase(value).native.r2.instances[0].raw_api.entries[0]
  const badCases = [
    ["missing diagnostics", (value) => { delete value.providers[0].storageDiagnostics }],
    ["config true/provider false", (value) => { value.providers[0].storageDiagnostics.enabled = false }],
    ["malformed config flag", (value) => { value.config.storageDiagnosticsEnabled = "true" }],
    ["malformed provider flag", (value) => { value.providers[0].storageDiagnostics.enabled = 1 }],
    ["missing provider metadata", (value) => { delete value.providers[0].blockProvider }],
    ["changed provider metadata", (value) => { value.providers[0].metadataProvider = "memory" }],
    ["missing workload", (value) => { value.providers[0].storageDiagnostics.phases = [] }],
    ["duplicate workload occurrence", (value) => { value.providers[0].storageDiagnostics.phases.push(structuredClone(phase(value))) }],
    ["wrong workload name", (value) => { phase(value).name = "workload-8192bytes" }],
    ["size mismatch", (value) => { value.providers[0].sizes[0].sizeMiB = 4 }],
    ["payload mismatch", (value) => { value.providers[0].sizes[0].fileSizeBytes = 8192 }],
    ["not quiescent", (value) => { phase(value).quiescent = false }],
    ["incomplete native", (value) => { phase(value).native.complete = false }],
    ["native issues", (value) => { phase(value).native.issues = ["pending operation"] }],
    ["global storage in flight", (value) => { phase(value).native.storage.in_flight_start = "1" }],
    ["global numeric gauge", (value) => { phase(value).native.storage.in_flight_end = 0 }],
    ["native v1", (value) => { phase(value).native.schema_version = "mount-rs.storage-diagnostics.v1" }],
    ["empty registry", (value) => { phase(value).native.r2.instances = [] }],
    ["duplicate identity", (value) => { phase(value).native.r2.instances.push(structuredClone(phase(value).native.r2.instances[0])) }],
    ["new identity", (value) => { phase(value).native.r2.instances[0].opened_during_phase = true }],
    ["changed start identity", (value) => { phase(value).native.r2.instance_ids_start = ["20"] }],
    ["missing identity", (value) => { phase(value).native.r2.missing_instance_ids = ["20"] }],
    ["missing raw", (value) => { delete phase(value).native.r2.instances[0].raw_api }],
    ["missing raw row", (value) => { phase(value).native.r2.instances[0].raw_api.entries.pop() }],
    ["raw row order", (value) => { phase(value).native.r2.instances[0].raw_api.entries.reverse() }],
    ["raw numeric counter", (value) => { row(value).calls = 3 }],
    ["raw outcomes", (value) => { row(value).success = "2" }],
    ["raw histogram", (value) => { row(value).latency_log2_us[1] = "2" }],
    ["raw maximum reset", (value) => { row(value).latency_max_ns_start = "1001" }],
    ["raw maximum exceeds phase elapsed", (value) => { row(value).latency_max_ns_end = "3001" }],
    ["zero calls advance maximum", (value) => { Object.assign(row(value), { calls: "0", success: "0", elapsed_ns: "0", attempted_bytes: "0", confirmed_bytes: "0", latency_log2_us: Array(32).fill("0"), latency_max_ns_end: "1001" }) }],
    ["zero calls retain submitted bytes", (value) => { Object.assign(row(value), { calls: "0", success: "0", elapsed_ns: "0", attempted_bytes: "1", confirmed_bytes: "0", latency_log2_us: Array(32).fill("0") }) }],
    ["invented phase maximum", (value) => { row(value).exact_phase_max_ns = "1000" }],
    ["raw bytes", (value) => { row(value).confirmed_bytes = "999999" }],
    ["raw byte applicability", (value) => { phase(value).native.r2.instances[0].raw_api.entries[1].returned_bytes = "1" }],
    ["raw in flight", (value) => { phase(value).native.r2.instances[0].raw_api.in_flight_start = "1" }],
    ["raw pending claims", (value) => { phase(value).native.r2.instances[0].raw_api.pending_claims_end = "1" }],
    ["raw claim outcomes", (value) => { phase(value).native.r2.instances[0].raw_api.claims.follower_claims = "1" }],
    ["raw saturation", (value) => { phase(value).native.r2.instances[0].raw_api.saturated_end = true }],
    ["raw metadata", (value) => { phase(value).native.measurement.r2_api.reconcile_listing = "instrumented" }],
  ]
  for (const [label, mutate] of badCases) {
    const value = rawQualificationArtifact()
    mutate(value)
    const retained = JSON.stringify(value)
    assert.throws(() => validateArtifact(value, options), /diagnostic/, label)
    assert.equal(JSON.stringify(value), retained, `${label}: verifier must preserve failed artifacts`)
  }
  const changedAcrossWorkloads = rawQualificationArtifact(options.providers, [1, 4])
  const second = changedAcrossWorkloads.providers[0].storageDiagnostics.phases[1].native.r2
  second.instances[0].id = "20"
  second.instance_ids_start = ["20"]
  second.instance_ids_end = ["20"]
  assert.throws(() => validateArtifact(changedAcrossWorkloads, options), /diagnostic/)
  const floor = rawQualificationArtifact()
  floor.providers[0].sizes[0].summary.iops = 999
  assert.throws(() => validateArtifact(floor, options), /iops-below-1000/)
  const mixed = rawQualificationArtifact(["mount-rs-split-sqlite-r2", "mount-rs-split-pglite-r2"])
  delete mixed.config.storageDiagnosticsEnabled
  mixed.providers[1].storageDiagnostics.enabled = false
  assert.throws(() => validateArtifact(mixed, { providers: mixed.providers.map((value) => value.provider) }), /diagnostic/)
  const nonR2 = rawQualificationArtifact(["mount-rs-memory"])
  nonR2.providers[0].storageDiagnostics.phases[0].native.r2.instances = []
  nonR2.providers[0].storageDiagnostics.phases[0].native.r2.instance_ids_start = []
  nonR2.providers[0].storageDiagnostics.phases[0].native.r2.instance_ids_end = []
  assert.doesNotThrow(() => validateArtifact(nonR2, { providers: ["mount-rs-memory"] }))
  const integrated = rawQualificationArtifact()
  const samplers = { now: () => 0, cpu: () => ({ user: 0, system: 0 }), resources: () => ({ voluntaryContextSwitches: 0, involuntaryContextSwitches: 0 }), memory: () => ({ rss: 0 }) }
  const endpoint = (calls) => takePhaseSnapshot(() => JSON.stringify(diagnosticSnapshot(calls, "7", [rawApiInstance(calls)])), samplers)
  integrated.providers[0].storageDiagnostics.phases = [finishPhase("workload-4096bytes", endpoint(2), endpoint(5))]
  assert.doesNotThrow(() => validateArtifact(integrated, options), "actual extraction and artifact validation agree")
  integrated.providers[0].storageDiagnostics.phases = [finishPhase("workload-4096bytes", endpoint(2), endpoint(2))]
  assert.doesNotThrow(() => validateArtifact(integrated, options), "zero phase calls do not erase the cumulative maximum")
}

async function testEvidencePacket() {
  const packet = {
    expectedRevision: W26_TEST_REVISION,
    policyLog: [
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=sqlite blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=pglite blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=tidb blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=foundationdb blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=insecure-blocks",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=inline-secret",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=foundationdb-unsafe",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=tidb-tls-weak",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_PASS providers=sqlite,pglite,tidb,foundationdb",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=slo",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=inline-secret",
    ].join("\n"),
    baseLog: [
      "OZONE_HEALTHY endpoint=http://127.0.0.1:9876 release=2.2.1",
      "OZONE_READY endpoint=http://127.0.0.1:9876 image=apache/ozone:2.2.1",
      "OZONE_FAULT_WINDOW_PASS container=ozone",
      "OZONE_RESTART_READY endpoint=http://127.0.0.1:9876",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    compositionsLog: [
      "OZONE_SQLITE_CHUNKED_COMPOSITION_PASS metadata=/tmp/ozone.sqlite revision=18",
      "OZONE_PGLITE_CHUNKED_COMPOSITION_PASS volume=mount-rs-ozone/metadata revision=18",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-sqlite-seed entries=2",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-pglite-seed entries=2",
      "test actual_binary_runs_live_ozone_split_provider_self_test ... ok",
      "SUMMARY node-sdk pass=8 skip=1 fail=0",
      "OZONE_NODE_CLI_PASS prefix=mount-rs-ozone/cli",
      "OZONE_CLI_REMOTE_HTTP_PASS mode=rust prefix=mount-rs-ozone/cli-http",
      "OZONE_COMPOSITION_PGLITE_READY endpoint=127.0.0.1:1",
      "OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000 output=artifacts/ozone-iops.json",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
      "OZONE_COMPOSITION_CLEANUP_PASS",
    ].join("\n"),
    compositionsArtifact: qualificationArtifact([
      "mount-rs-split-sqlite-r2",
      "mount-rs-split-pglite-r2",
    ]),
    tidbLog: [
      "TIDB_RUSTFS_CHUNKED_BOUNDED_READDIR_PASS phase=seed entries=2",
      "TIDB_CHUNKED_RUSTFS_SEED_PASS",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=seed prefix=mount-rs/tidb",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=mount-rs/tidb",
      "TIDB_ACCEPTANCE evidence=durable",
      "TIDB_OZONE_IOPS_PASS provider=tidb-r2 target=1000 output=artifacts/ozone-tidb-iops.json",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    tidbArtifact: qualificationArtifact("mount-rs-split-tidb-r2"),
    foundationdbLog: [
      "OZONE_READY endpoint=http://127.0.0.1:9876 image=apache/ozone:2.2.1",
      "OZONE_BLOCK_CONTRACT_PASS prefix=mount-rs-ozone/foundationdb",
      "OZONE_FAULT_WINDOW_PASS container=ozone",
      "OZONE_GATEWAY_FAILURE_PASS prefix=mount-rs-ozone/foundationdb error=transport",
      "OZONE_RESTART_READY endpoint=http://127.0.0.1:9876",
      "OZONE_RESTART_REOPEN_PASS prefix=mount-rs-ozone/foundationdb",
      "FOUNDATIONDB_CONFIGURED topology=durable redundancy=double storage=ssd servers=fdb1,fdb2,fdb3",
      "FOUNDATIONDB_TRANSACTION_READY server=fdb1",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=seed prefix=mount-rs/foundationdb",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=mount-rs/foundationdb",
      "FOUNDATIONDB_NAPI_PASS image=node:24-bookworm",
      "FOUNDATIONDB_OZONE_IOPS_PASS provider=foundationdb-r2 target=1000 output=artifacts/ozone-foundationdb-iops.json",
      "FOUNDATIONDB_TEST_PASS topology=durable manifest=providers/mount-rs-foundationdb/Cargo.toml platform=linux/amd64",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    foundationdbArtifact: qualificationArtifact("mount-rs-split-foundationdb-r2"),
  }

  assert.deepEqual(validateEvidencePacket(packet), {
    revision: W26_TEST_REVISION,
    artifacts: ["ozone-compositions", "ozone-tidb", "ozone-foundationdb"],
    policyMarkers: 11,
  })
  assert.throws(
    () => validateEvidencePacket({
      ...packet,
      compositionsLog: packet.compositionsLog.replace(
        "SUMMARY node-sdk pass=8 skip=1 fail=0",
        "SUMMARY node-sdk pass=7 skip=1 fail=0",
      ),
    }),
    /ozone-compositions-log-missing-marker=SUMMARY node-sdk pass=8 skip=1 fail=0/,
  )
  assert.throws(
    () => validateEvidencePacket({ ...packet, tidbLog: packet.tidbLog.replace("TIDB_ACCEPTANCE ", "") }),
    /tidb-log-missing-marker=TIDB_ACCEPTANCE/,
  )
  assert.throws(
    () => validateEvidencePacket({
      ...packet,
      compositionsLog: packet.compositionsLog.replace("OZONE_NODE_CLI_PASS ", ""),
    }),
    /ozone-compositions-log-missing-marker=OZONE_NODE_CLI_PASS/,
  )
  const mismatchedFoundationDb = structuredClone(packet.foundationdbArtifact)
  mismatchedFoundationDb.environment.sourceControl.mountRs.revision = "b".repeat(40)
  assert.throws(
    () => validateEvidencePacket({ ...packet, foundationdbArtifact: mismatchedFoundationDb }),
    /foundationdb-artifact-source-revision-does-not-match-packet/,
  )
}

async function testProductionRolloutContract() {
  const fixturePath = new URL(
    "../../tests/ozone/production-rollout-contract.json",
    import.meta.url,
  )
  const contract = JSON.parse(await readFile(fixturePath, "utf8"))
  assert.deepEqual(validateContract(contract), {
    status: "pass",
    metadataProviders: ["sqlite", "pglite", "tidb", "foundationdb"],
    customerOwnedRecovery: true,
  })

  assert.throws(
    () => validateContract({
      ...contract,
      service: { ...contract.service, rtoMinutes: 10 },
    }),
    /service\.rtoMinutes-must-be-5/,
  )
  assert.throws(
    () => validateContract({
      ...contract,
      ozone: {
        ...contract.ozone,
        auth: { ...contract.ozone.auth, secretKeyRef: "inline-secret" },
      },
    }),
    /secretKeyRef-must-not-be-inline/,
  )
}

async function testW26WorkflowKeepsProvenanceClean() {
  const workflow = await readFile(".github/workflows/ci.yml", "utf8")
  const compositionStart = workflow.indexOf("  ozone-compositions:")
  const compositionEnd = workflow.indexOf("  ozone-tidb:", compositionStart)
  assert.notEqual(compositionStart, -1)
  assert.notEqual(compositionEnd, -1)
  const compositionJob = workflow.slice(compositionStart, compositionEnd)
  assert.match(
    compositionJob,
    /MOUNT_RS_OZONE_IOPS_OUTPUT: \$\{\{ runner\.temp \}\}\/w26-ozone-compositions\/ozone-iops\.json/u,
  )
  assert.match(
    compositionJob,
    /tee "\$RUNNER_TEMP\/w26-ozone-compositions\/ozone-compositions\.log"/u,
  )
  assert.doesNotMatch(compositionJob, /tee artifacts\/ozone-compositions\.log/u)
  assert.doesNotMatch(compositionJob, /artifacts\/ozone-iops\.json/u)

  const rustStart = workflow.indexOf("  rust:")
  const rustEnd = workflow.indexOf("  http-observability:", rustStart)
  assert.notEqual(rustStart, -1)
  assert.notEqual(rustEnd, -1)
  assert.match(workflow.slice(rustStart, rustEnd), /timeout-minutes: 25/u)
}

async function testExecutionSurfaceLabels() {
  const definitions = providerById({})
  assert.deepEqual(definitions.get("mount-rs-memory").executionSurface, {
    callerRuntime: "node",
    implementationLanguage: "rust",
    apiBinding: "public-napi",
    nativeAddon: true,
    directRust: "not-run",
  })
  assert.deepEqual(definitions.get("mountx-memory").executionSurface, {
    callerRuntime: "node",
    implementationLanguage: "typescript",
    apiBinding: "actual-typescript-oracle",
    nativeAddon: false,
    directRust: "not-run",
  })
}

async function testOzoneProviderMatrix() {
  const r2Environment = {
    MOUNT_RS_R2_ENDPOINT: "https://ozone.example.test",
    MOUNT_RS_R2_BUCKET: "bucket",
    MOUNT_RS_R2_ACCESS_KEY_ID: "access-key",
    MOUNT_RS_R2_SECRET_ACCESS_KEY: "secret-value",
  }
  const definitions = providerById(r2Environment)
  for (const provider of [
    "mount-rs-split-sqlite-r2",
    "mount-rs-split-pglite-r2",
    "mount-rs-split-tidb-r2",
    "mount-rs-split-foundationdb-r2",
  ]) {
    assert.ok(definitions.has(provider), `missing Ozone provider definition: ${provider}`)
  }
  assert.equal(
    definitions.get("mount-rs-split-sqlite-r2").availability(r2Environment).configured,
    true,
  )
  assert.equal(
    definitions.get("mount-rs-split-pglite-r2").availability(r2Environment).configured,
    false,
  )
  assert.equal(
    definitions.get("mount-rs-split-tidb-r2").availability(r2Environment).configured,
    false,
  )
  assert.equal(
    definitions.get("mount-rs-split-foundationdb-r2").availability(r2Environment).configured,
    false,
  )

  const configuredDefinitions = providerById({
    ...r2Environment,
    MOUNT_RS_PGLITE_DATABASE_URL: "postgres://pglite",
    MOUNT_RS_TIDB_URL: "mysql://tidb",
    MOUNT_RS_NAPI_FOUNDATIONDB: "1",
    MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE: "/run/fdb/fdb.cluster",
  })
  assert.equal(
    configuredDefinitions.get("mount-rs-split-pglite-r2").availability({}).configured,
    true,
  )
  assert.equal(
    configuredDefinitions.get("mount-rs-split-tidb-r2").availability({}).configured,
    true,
  )
  assert.equal(
    configuredDefinitions.get("mount-rs-split-foundationdb-r2").availability({}).configured,
    true,
  )

  const summary = JSON.stringify(
    providerSummary(
      configuredDefinitions.get("mount-rs-split-pglite-r2"),
      65_536,
    ),
  )
  assert.equal(summary.includes("secret-value"), false)
  assert.equal(summary.includes("access-key"), false)
}

const fdbConfig = { clusterFile: "/owned/fdb.cluster", leaseAuthority: "shared-provider", sharedProvider: true, authorityPrefix: "owned-authority" }
assert.equal(foundationDbMetadataOptions(fdbConfig, { runId: "test", layout: "legacy" }).authorityPrefix, "owned-authority")
const inodeFdb = foundationDbMetadataOptions(fdbConfig, { runId: "test", layout: "inode" })
assert.equal(inodeFdb.leaseAuthority, "revision-cas")
assert.equal("authorityPrefix" in inodeFdb, false)
const compactFdb = foundationDbMetadataOptions(fdbConfig, { runId: "test", layout: "compact" })
assert.equal(compactFdb.leaseAuthority, "revision-cas")
assert.equal("authorityPrefix" in compactFdb, false)

async function testSteadyOverwriteOracle() {
  const initial = Buffer.from([0x51, 1, 2, 3, 0xa7])
  const calls = []
  const handle = {
    async write(bytes, offset, length, position) {
      calls.push(["write", position, length])
      bytes.copy(initial, position, offset, offset + length)
      return { bytesWritten: length }
    },
    async read(target, offset, length, position) {
      calls.push(["read", position, length])
      initial.copy(target, offset, position, position + length)
      return { bytesRead: length, buffer: target }
    },
  }
  const args = { definition: { id: "steady-test" }, handle, payload: Buffer.from([1, 2, 3]), path: "/owned", iteration: 1, generation: 1, options: { timeoutMs: 100, cleanupTimeoutMs: 100 }, pendingOperations: new Map() }
  const success = await runSteadySample(args)
  assert.equal(success.success, true)
  assert.equal(success.deleteSucceeded, null)
  assert.deepEqual(calls, [["write", 1, 3], ["read", 0, 5]])
  initial[0] = 0 // An unchanged boundary is still part of the full byte oracle.
  const corrupt = await runSteadySample({ ...args, iteration: 2, generation: 2 })
  assert.equal(corrupt.success, false)
  assert.equal(corrupt.payloadVerified, false)
  const partial = await runSteadySample({ ...args, handle: { ...handle, async write() { return { bytesWritten: 1 } } } })
  assert.equal(partial.success, false)
  assert.equal(partial.readSucceeded, false)
  let attempts = 0
  const pendingArgs = { ...args, options: { timeoutMs: 5, cleanupTimeoutMs: 5 }, handle: { async write() { attempts += 1; return new Promise(() => {}) } }, pendingOperations: new Map() }
  assert.equal((await runSteadySample(pendingArgs)).success, false)
  assert.equal(pendingArgs.pendingOperations.size, 1)
  assert.equal((await runSteadySample(pendingArgs)).success, false)
  assert.equal(attempts, 1, "unresolved operation must not be replayed on its handle")
}
async function testSteadyGenerationsRejectDroppedWrites() {
  function lane() {
    const payload = Buffer.alloc(8, 0x39)
    const file = Buffer.concat([Buffer.from([0x51]), payload, Buffer.from([0xa7])])
    const state = { payload, file, drop: false }
    state.handle = {
      async write(bytes, offset, length, position) {
        if (!state.drop) bytes.copy(file, position, offset, offset + length)
        return { bytesWritten: length }
      },
      async read(target) { file.copy(target); return { bytesRead: file.length, buffer: target } },
    }
    return state
  }
  async function sample(state, iteration, generation) {
    return runSteadySample({ definition: { id: "generation-test" }, handle: state.handle, payload: state.payload, path: "/owned", iteration, generation, options: { timeoutMs: 100, cleanupTimeoutMs: 100 }, pendingOperations: new Map() })
  }
  const distinct = lane()
  const seen = new Set([distinct.file.toString("hex")])
  for (const generation of [1, 256, 257, Number.MAX_SAFE_INTEGER]) {
    assert.equal((await sample(distinct, generation, generation)).success, true)
    seen.add(distinct.file.toString("hex"))
  }
  assert.equal(seen.size, 5, "all supported generations must differ from setup and each other")
  await assert.rejects(() => runSteadySample({ definition: { id: "tiny" }, payload: Buffer.alloc(1), generation: 256, path: "/tiny", iteration: 256, pendingOperations: new Map() }), /generation.*capacity/)
  const first256 = lane()
  first256.drop = true
  const firstOverwrite = await sample(first256, 256, 1) // First operation of lane 256.
  const recurring = lane()
  assert.equal((await sample(recurring, 1, 1)).success, true)
  recurring.drop = true
  const stale257 = await sample(recurring, 257, 2) // Scheduler returns to the same lane.
  const sequence256 = lane()
  sequence256.drop = true
  const wrappedSequence = await sample(sequence256, 256, 256)
  const recurring257 = lane()
  assert.equal((await sample(recurring257, 1, 1)).success, true)
  recurring257.drop = true
  const wrappedStale = await sample(recurring257, 257, 257)
  assert.deepEqual(
    [firstOverwrite.success, stale257.success, wrappedSequence.success, wrappedStale.success],
    [false, false, false, false],
    "iteration 256/concurrency 256 and same-lane 1/257 must reject dropped writes",
  )
}
if (process.argv.includes("--diagnostics-only")) {
  const failures = []
  for (const test of [testStorageDriverFieldDeltas, testStorageFamilyMetadata, testStoragePhaseDiagnostics, testRawObjectStorePhaseDiagnostics, testObjectStoreLocalPhaseDiagnostics, testRawQualificationArtifact, testQualificationProviderCoverage, testObserverEndpointsExcludeSnapshotWork, testQualificationArtifact]) {
    try { await test(); console.log(`${test.name}: PASS`) }
    catch (error) { failures.push(test.name); console.error(`${test.name}: FAIL`, error) }
  }
  if (failures.length) process.exitCode = 1
  else console.log("storage diagnostic unit tests: PASS")
} else {
await testSteadyGenerationsRejectDroppedWrites()
await testSteadyOverwriteOracle()
await testStats()
await testStoragePhaseDiagnostics()
await testStorageDriverFieldDeltas()
await testStorageFamilyMetadata()
await testRawObjectStorePhaseDiagnostics()
await testObjectStoreLocalPhaseDiagnostics()
await testObserverEndpointsExcludeSnapshotWork()
await testOzoneMetricsReachAllNodeProcesses()
await testErrors()
await testCli()
await testCompactRejectsNonSplitBeforeProviderIo()
await testSplitFactoryLayoutOptions()
await testFoundationDbAvailabilityFollowsSelectedLayout()
await testCompactArtifactSeparatesSelectionFromPersistedProof()
await testRequiredProviderConfiguration()
await testQualificationArtifact()
await testRawQualificationArtifact()
await testQualificationProviderCoverage()
await testEvidencePacket()
await testProductionRolloutContract()
await testW26WorkflowKeepsProvenanceClean()
await testExecutionSurfaceLabels()
await testOzoneProviderMatrix()
await testDeferredWriteCleanup()
await testPendingProviderInvalidatesFollowingPhaseAttribution()
console.log("storage benchmark unit tests: PASS")
}
