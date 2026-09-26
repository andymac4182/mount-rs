import { isDeepStrictEqual } from "node:util"

// Opt-in quiescent phase snapshots. Native counters are exact decimal strings.
const decimal = /^(?:0|[1-9]\d*)$/u
const u64Maximum = 18446744073709551615n
export const NATIVE_DIAGNOSTICS_SCHEMA = "mount-rs.storage-diagnostics.v3"
const rawSchema = "mount-rs.object-store-api.v1"
const rawScope = "one_object_store_block_store_instance"
const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const claimFields = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]
const rawFields = ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes"]
const localSchema = "mount-rs.object-store-local.v1"
const localFields = ["calls", "success", "error", "cancelled", "elapsed_ns", "input_bytes", "output_bytes"]
export const OBJECT_STORE_LOCAL_NAMES = ["sha256.digest", "block_id.encode", "copy.upload_payload", "copy.cache_insert", "copy.return_vec", "cache.lock_acquire", "put.follower_wait"]
export const OBJECT_STORE_LOCAL_MEASUREMENT = {
  schema: localSchema, scope: "live_registered_split_r2_block_store_instances",
  calls: "fixed_local_adapter_work_invocations; not_backend_requests_or_allocations",
  duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap",
  input_bytes: "entered_digest_encoding_and_copy_input; waits_zero",
  output_bytes: "completed_digest_32_id_65_and_actual_copy_bytes; waits_zero",
  cache_lock_scope: "mutex_acquisition_including_wait; excludes_lock_hold_and_lru_work",
  latency_max: "cumulative_per_instance; exact_phase_max_unavailable", adapter_compression: "not_used",
  excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "client_internal_work", "cache_key_and_lru_work", "upload_claim_setup"],
}
const logicalR2Fields = ["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]
export const STORAGE_OPERATION_NAMES = [
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
  "tidb.sql.flush_probe"
]
const storageNames = STORAGE_OPERATION_NAMES
export const STORAGE_CALL_SEMANTICS = "fixed_label_provider_and_driver_operations; families_overlap_and_are_not_application_iops"
export const STORAGE_BYTE_SEMANTICS = "known_successful_payload_bytes_only; zero_does_not_establish_no_payload"
export const STORAGE_ROW_SEMANTICS = "known_returned_sql_rows; observations_count_successes_with_known_rows; excludes_affected_rows"
export const STORAGE_OPERATION_FAMILIES = {
  napi_provider: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("metadata.") || name.startsWith("blocks.")), calls: "napi_dynamic_provider_method_invocations", bytes: "known_successful_block_put_input_and_get_or_migration_payload_bytes; metadata_bytes_unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  sdk_provider: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("sdk.")), calls: "direct_sdk_provider_method_invocations_including_synchronous_methods", bytes: "known_successful_block_put_input_and_get_or_migration_payload_bytes; metadata_bytes_unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  pglite_client_lock: { operations: ["pglite.client_lock_wait"], calls: "client_lock_acquisition_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_client_lock_await_nanoseconds" },
  tidb_pool_checkout: { operations: ["tidb.pool.checkout"], calls: "pool_checkout_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_checkout_nanoseconds_including_lazy_connect_and_session_configuration; queue_only_wait_unavailable" },
  tidb_session: { operations: ["tidb.session.configure"], calls: "session_configuration_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  tidb_open: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("tidb.open.")), calls: "open_schema_and_metadata_initialization_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  tidb_transaction: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("tidb.tx.")), calls: "transaction_lifecycle_invocations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  tidb_sql: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("tidb.sql.")), calls: "categorized_sql_adapter_invocations; not_internal_requests", bytes: "known_selected_successful_payload_bytes_only; other_sql_bytes_unavailable", returned_rows: STORAGE_ROW_SEMANTICS, duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
}
export const TIDB_DIAGNOSTIC_COVERAGE = {
  schema: "mount-rs-tidb-client-diagnostic-coverage-v1", status: "source_sites_instrumented",
  pool_checkout_sites: "34", session_configure_sites: "1", schema_initialize_sites: "1", metadata_open_sites: "1",
  transaction_begin_sites: "3", transaction_commit_sites: "1", transaction_rollback_sites: "5", sql_statement_sites: "56",
  operations: ["tidb.pool.checkout", "tidb.session.configure", "tidb.open.schema", "tidb.open.metadata_row", "tidb.tx.begin.metadata", "tidb.tx.begin.inode", "tidb.tx.begin.compact_read", "tidb.tx.commit", "tidb.tx.rollback", "tidb.sql.session", "tidb.sql.ddl", "tidb.sql.metadata_read", "tidb.sql.metadata_write", "tidb.sql.inode_read", "tidb.sql.inode_write", "tidb.sql.block_read", "tidb.sql.block_write", "tidb.sql.flush_probe"],
  sql_returned_rows_scope: "successful SELECT Option/Vec results only; exec_iter affected_rows excluded",
  sql_payload_bytes_scope: "successful block INSERT submitted bytes and block-body SELECT returned bytes only; other SQL payload bytes unavailable",
  pool_checkout_scope: "pool.get_conn await includes possible lazy connection and session setup; queue-only wait unavailable",
  unavailable: ["pool_queue_only_wait", "separate_driver_connect_handshake", "server_sql_execution_time", "transaction_lifetime_and_implicit_drop_rollback", "affected_rows", "other_sql_payload_bytes", "sql_wire_bytes_and_client_internal_retries", "tikv_and_physical_device_io"],
}
export const STORAGE_INSTRUMENTED_OPERATION_NAMES = storageNames.filter((name) => !name.startsWith("tidb.") || TIDB_DIAGNOSTIC_COVERAGE.operations.includes(name))
const rawMeasurement = {
  schema: rawSchema, scope: "live_registered_split_r2_block_store_instances",
  calls: "object_store_adapter_method_invocations; not_http_attempts_or_internal_retries",
  duration: "inclusive_wall_nanoseconds_at_invoked_adapter_await; excludes_argument_preparation",
  upload_bytes: "attempted=submitted_payload; confirmed=put_opts_ok_only",
  returned_bytes: "successful_body_materialization_before_integrity_validation",
  latency_max: "cumulative_per_instance; exact_phase_max_unavailable", reconcile_listing: "unavailable",
  excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "internal_client_retries"],
}
const histogramIntervals = Array.from({ length: 32 }, (_, bucket) => bucket === 0
  ? { lower_inclusive_us: "0", upper_exclusive_us: "1" }
  : bucket === 31
    ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true }
    : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) })
class DiagnosticValidationError extends Error {}
function invalid(reason) { throw new DiagnosticValidationError(reason) }
function object(value) { return value !== null && typeof value === "object" && !Array.isArray(value) }
function integer(value, rejectSaturated = false) {
  if (typeof value !== "string" || value.length > 20 || !decimal.test(value)) invalid("invalid decimal counter")
  const number = BigInt(value)
  if (number > u64Maximum) invalid("counter exceeds u64")
  if (rejectSaturated && number === u64Maximum) invalid("saturated raw counter unavailable")
  return number
}
function subtract(now, old = "0") {
  const difference = integer(now) - integer(old)
  if (difference < 0n) invalid("counter reset")
  return difference.toString()
}
function entriesDelta(before, after, idKey, fields) {
  if (!Array.isArray(before) || !Array.isArray(after) || before.length !== after.length) invalid("counter shape changed")
  return after.map((entry, index) => {
    const old = before[index]
    if (entry[idKey] !== old[idKey]) invalid("counter names changed")
    const delta = { [idKey]: entry[idKey] }
    for (const field of fields) {
      if (Array.isArray(entry[field]) !== Array.isArray(old[field]) ||
          (Array.isArray(entry[field]) && entry[field].length !== old[field].length)) invalid("counter histogram shape changed")
      delta[field] = Array.isArray(entry[field])
        ? entry[field].map((value, bucket) => subtract(value, old[field]?.[bucket]))
        : subtract(entry[field], old[field])
    }
    return delta
  })
}
function validateStorageRows(entries, phase = false) {
  if (!Array.isArray(entries) || entries.length !== storageNames.length) invalid("storage operation coverage changed")
  for (const [index, entry] of entries.entries()) {
    if (!object(entry) || entry.name !== storageNames[index]) invalid("storage operation names or order changed")
    for (const field of ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns"]) integer(entry[field])
    if (BigInt(entry.calls) !== BigInt(entry.success) + BigInt(entry.error) + BigInt(entry.cancelled)) invalid("storage outcomes do not reconcile")
    if (!entry.name.startsWith("tidb.sql.") && (entry.returned_rows !== "0" || entry.returned_row_observations !== "0")) invalid("returned-row observation outside SQL family")
    if (BigInt(entry.returned_row_observations) > BigInt(entry.success)) invalid("returned-row observations exceed successful calls")
    if (entry.returned_row_observations === "0" && entry.returned_rows !== "0") invalid("returned rows have no known observations")
    if (!Array.isArray(entry.latency_log2_us) || entry.latency_log2_us.length !== 32 || entry.latency_log2_us.reduce((sum, count) => sum + integer(count), 0n) !== BigInt(entry.calls)) invalid("storage histogram does not reconcile")
    if (phase) {
      if (integer(entry.in_flight_start) !== 0n || integer(entry.in_flight_end) !== 0n) invalid("storage operation row pending")
    } else integer(entry.in_flight)
  }
}
function connectionDelta(before, after) {
  const previous = new Map(before.map((entry) => [entry.connection_id, entry]))
  const current = new Map(after.map((entry) => [entry.connection_id, entry]))
  const missing = [...previous.keys()].filter((id) => !current.has(id))
  const connections = after.map((entry) => {
    const old = previous.get(entry.connection_id)
    if (entry.error || old?.error) invalid("SQLite counter unavailable")
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
function validateClaims(claims) {
  if (!object(claims) || Object.keys(claims).length !== claimFields.length) invalid("raw claim fields unavailable")
  for (const field of claimFields) integer(claims[field], true)
  for (const role of ["leader", "follower"]) {
    if (BigInt(claims[`${role}_claims`]) !== BigInt(claims[`${role}_success`]) + BigInt(claims[`${role}_error`]) + BigInt(claims[`${role}_cancelled`])) invalid("raw claim outcomes do not reconcile")
  }
}
function validateRawRows(entries, phase = false) {
  if (!Array.isArray(entries) || entries.length !== rawNames.length) invalid("raw operation rows unavailable")
  for (const [index, entry] of entries.entries()) {
    if (!object(entry) || entry.name !== rawNames[index]) invalid("raw operation names or order changed")
    for (const field of rawFields) integer(entry[field], true)
    if (BigInt(entry.calls) !== BigInt(entry.success) + BigInt(entry.error) + BigInt(entry.cancelled)) invalid("raw call outcomes do not reconcile")
    if (!Array.isArray(entry.latency_log2_us) || entry.latency_log2_us.length !== 32) invalid("raw histogram shape changed")
    const histogramTotal = entry.latency_log2_us.reduce((sum, value) => sum + integer(value, true), 0n)
    if (histogramTotal !== BigInt(entry.calls)) invalid("raw histogram total does not reconcile")
    if (entry.calls === "0" && rawFields.some((field) => entry[field] !== "0")) invalid("raw zero-call totals inconsistent")
    if (phase) {
      const start = integer(entry.latency_max_ns_start, true)
      const end = integer(entry.latency_max_ns_end, true)
      if (end < start) invalid("raw cumulative maximum reset")
      if ((entry.calls === "0" && end !== start) || (end > start && end > BigInt(entry.elapsed_ns))) invalid("raw phase maximum inconsistent")
      if (entry.exact_phase_max_ns !== "unavailable") invalid("exact raw phase maximum unavailable")
    } else {
      const maximum = integer(entry.latency_max_ns, true)
      if (maximum > BigInt(entry.elapsed_ns) || (entry.calls === "0" && maximum !== 0n)) invalid("raw cumulative maximum inconsistent")
    }
    if (index === 0) {
      if (entry.returned_bytes !== "0" || BigInt(entry.confirmed_bytes) > BigInt(entry.attempted_bytes) || (entry.success === "0" && entry.confirmed_bytes !== "0")) invalid("raw put byte semantics inconsistent")
    } else if (entry.attempted_bytes !== "0" || entry.confirmed_bytes !== "0" || (!entry.name.startsWith("body_read.") && entry.returned_bytes !== "0") || (entry.success === "0" && entry.returned_bytes !== "0")) invalid("raw byte applicability inconsistent")
  }
}
function validateRawSnapshot(raw) {
  if (!object(raw) || raw.schema !== rawSchema || raw.scope !== rawScope) invalid("raw API schema or scope unavailable")
  if (raw.saturated !== false) invalid("raw API saturation state unavailable")
  if (integer(raw.in_flight, true) !== 0n || integer(raw.pending_claims, true) !== 0n) invalid("raw operation or claim crossed phase boundary")
  validateClaims(raw.claims)
  validateRawRows(raw.entries)
}
function instanceMap(instances) {
  if (!Array.isArray(instances)) invalid("R2 instance registry unavailable")
  const map = new Map()
  for (const entry of instances) {
    if (!object(entry)) invalid("invalid R2 instance")
    integer(entry.id, true)
    if (map.has(entry.id)) invalid("duplicate R2 instance identity")
    map.set(entry.id, entry)
  }
  return map
}
function rawDelta(before, after) {
  const old = before ?? { in_flight: "0", pending_claims: "0", saturated: false,
    claims: Object.fromEntries(claimFields.map((field) => [field, "0"])),
    entries: rawNames.map((name) => ({ name, ...Object.fromEntries([...rawFields, "latency_max_ns"].map((field) => [field, "0"])), latency_log2_us: Array(32).fill("0") })) }
  const entries = entriesDelta(old.entries, after.entries, "name", [...rawFields, "latency_log2_us"])
  for (const [index, row] of entries.entries()) {
    const start = old.entries[index].latency_max_ns
    const end = after.entries[index].latency_max_ns
    if (integer(end, true) < integer(start, true)) invalid("raw cumulative maximum reset")
    Object.assign(row, { latency_max_ns_start: start, latency_max_ns_end: end, exact_phase_max_ns: "unavailable" })
  }
  const claims = Object.fromEntries(claimFields.map((field) => [field, subtract(after.claims[field], old.claims[field])]))
  validateClaims(claims)
  validateRawRows(entries, true)
  return { schema: rawSchema, scope: rawScope, saturated_start: old.saturated, saturated_end: after.saturated,
    in_flight_start: old.in_flight, in_flight_end: after.in_flight, pending_claims_start: old.pending_claims, pending_claims_end: after.pending_claims, claims, entries }
}
function validateLocalRows(entries, phase = false) {
  if (!Array.isArray(entries) || entries.length !== OBJECT_STORE_LOCAL_NAMES.length) invalid("local operation rows unavailable")
  for (const [index, entry] of entries.entries()) {
    if (!object(entry) || entry.name !== OBJECT_STORE_LOCAL_NAMES[index]) invalid("local operation names or order changed")
    for (const field of localFields) integer(entry[field], true)
    if (BigInt(entry.calls) !== BigInt(entry.success) + BigInt(entry.error) + BigInt(entry.cancelled)) invalid("local call outcomes do not reconcile")
    if (!Array.isArray(entry.latency_log2_us) || entry.latency_log2_us.length !== 32 || entry.latency_log2_us.reduce((sum, value) => sum + integer(value, true), 0n) !== BigInt(entry.calls)) invalid("local histogram does not reconcile")
    if (entry.calls === "0" && localFields.some((field) => entry[field] !== "0")) invalid("local zero-call totals inconsistent")
    if (phase) {
      const start = integer(entry.latency_max_ns_start, true), end = integer(entry.latency_max_ns_end, true)
      if (end < start || entry.calls === "0" && end !== start || end > start && end > BigInt(entry.elapsed_ns) || entry.exact_phase_max_ns !== "unavailable") invalid("local phase maximum inconsistent")
    } else {
      const maximum = integer(entry.latency_max_ns, true)
      if (maximum > BigInt(entry.elapsed_ns) || entry.calls === "0" && maximum !== 0n) invalid("local cumulative maximum inconsistent")
    }
    const input = BigInt(entry.input_bytes), output = BigInt(entry.output_bytes), success = BigInt(entry.success)
    if (index === 0 && output !== success * 32n || index === 1 && (input !== BigInt(entry.calls) * 32n || output !== success * 65n) || index >= 2 && index <= 4 && (output > input || success === 0n && output !== 0n) || index >= 5 && (input !== 0n || output !== 0n)) invalid("local byte applicability inconsistent")
  }
}
function validateLocalSnapshot(local) {
  if (!object(local) || local.schema !== localSchema || local.scope !== rawScope) invalid("local schema or scope unavailable")
  if (local.saturated !== false) invalid("local saturation state unavailable")
  if (integer(local.in_flight, true) !== 0n) invalid("local operation crossed phase boundary")
  validateLocalRows(local.entries)
}
export function validateLocalPhaseDiagnostics(local, measurement) {
  if (!isDeepStrictEqual(measurement, OBJECT_STORE_LOCAL_MEASUREMENT)) invalid("local phase measurement metadata unavailable")
  if (!object(local) || local.status !== "observed" || local.complete !== true || local.schema !== localSchema || local.scope !== rawScope) invalid("local phase schema or availability unavailable")
  if (local.saturated_start !== false || local.saturated_end !== false || integer(local.in_flight_start, true) !== 0n || integer(local.in_flight_end, true) !== 0n) invalid("local phase saturation or boundary unavailable")
  validateLocalRows(local.entries, true)
  return local
}
function localObservation(local) {
  if (!object(local)) return { unavailable: local === null ? "disabled" : "missing" }
  const counter = (value) => typeof value === "string" && value.length <= 20 && decimal.test(value) && BigInt(value) <= u64Maximum ? value : "unavailable"
  const rows = Array.isArray(local.entries) ? local.entries.slice(0, OBJECT_STORE_LOCAL_NAMES.length) : []
  return { schema: local.schema === localSchema ? localSchema : "unavailable", scope: local.scope === rawScope ? rawScope : "unavailable",
    saturated: typeof local.saturated === "boolean" ? local.saturated : "unavailable", in_flight: counter(local.in_flight),
    entries: OBJECT_STORE_LOCAL_NAMES.map((name) => {
      const matches = rows.filter((entry) => entry?.name === name), row = matches.length === 1 ? matches[0] : null
      return { name, ...Object.fromEntries([...localFields, "latency_max_ns"].map((field) => [field, counter(row?.[field])])),
        latency_log2_us: Array.from({ length: 32 }, (_, index) => counter(row?.latency_log2_us?.[index])) }
    }) }
}
function localDelta(before, after, beforeMeasurement, afterMeasurement, opened) {
  if (!object(after) || !opened && !object(before)) return { status: "unavailable", complete: false, issues: [before === null && after === null ? "local diagnostics disabled" : "local diagnostics unavailable or changed availability"] }
  try {
    if (!isDeepStrictEqual(beforeMeasurement, OBJECT_STORE_LOCAL_MEASUREMENT) || !isDeepStrictEqual(afterMeasurement, OBJECT_STORE_LOCAL_MEASUREMENT)) invalid("local measurement metadata unavailable")
    if (!opened) validateLocalSnapshot(before)
    validateLocalSnapshot(after)
    const old = before ?? { saturated: false, in_flight: "0", entries: OBJECT_STORE_LOCAL_NAMES.map((name) => ({ name, ...Object.fromEntries([...localFields, "latency_max_ns"].map((field) => [field, "0"])), latency_log2_us: Array(32).fill("0") })) }
    const entries = entriesDelta(old.entries, after.entries, "name", [...localFields, "latency_log2_us"])
    for (const [index, row] of entries.entries()) Object.assign(row, { latency_max_ns_start: old.entries[index].latency_max_ns, latency_max_ns_end: after.entries[index].latency_max_ns, exact_phase_max_ns: "unavailable" })
    validateLocalRows(entries, true)
    return { status: "observed", complete: true, schema: localSchema, scope: rawScope, saturated_start: old.saturated, saturated_end: after.saturated,
      in_flight_start: old.in_flight, in_flight_end: after.in_flight, entries }
  } catch (error) {
    return { status: "invalid", complete: false, issues: [error instanceof DiagnosticValidationError ? error.message : "invalid local diagnostic shape"], observations: { before: localObservation(before), after: localObservation(after) } }
  }
}
function r2Delta(before, after, beforeLocalMeasurement, afterLocalMeasurement) {
  if (before?.scope !== "process_live_instances" || after?.scope !== before.scope || before.available === false || after.available === false || before.internal_successful_retries !== "unavailable" || after.internal_successful_retries !== "unavailable") invalid("R2 registry metadata unavailable")
  const previous = instanceMap(before.instances)
  const current = instanceMap(after.instances)
  for (const entry of [...previous.values(), ...current.values()]) {
    for (const field of logicalR2Fields) integer(entry[field])
    validateRawSnapshot(entry.raw_api)
  }
  const missing = [...previous.keys()].filter((id) => !current.has(id))
  return { scope: "process_live_instances", instance_ids_start: [...previous.keys()], instance_ids_end: [...current.keys()], instances: after.instances.map((entry) => {
    const old = previous.get(entry.id)
    return { id: entry.id, opened_during_phase: !old, raw_api: rawDelta(old?.raw_api, entry.raw_api),
      local_work: localDelta(old?.local_work, entry.local_work, beforeLocalMeasurement, afterLocalMeasurement, !old),
      ...Object.fromEntries(logicalR2Fields.map((field) => [field, subtract(entry[field], old?.[field])])) }
  }), missing_instance_ids: missing, complete: missing.length === 0, internal_successful_retries: "unavailable" }
}

// Invalid observations retain only fixed labels and bounded integer evidence.
// Arbitrary metadata, identifiers, SQL categories and error strings are omitted.
function sanitizedObservations(before, after) {
  let truncated = false
  const counter = (value) => {
    if (typeof value === "string" && value.length <= 20 && decimal.test(value) && BigInt(value) <= u64Maximum) return value
    return { unavailable: value === undefined ? "missing" : value === null ? "null" : Array.isArray(value) ? "array" : typeof value === "number" ? "number" : typeof value === "string" ? "invalid_decimal" : "invalid_type" }
  }
  const array = (value, limit, mapper) => {
    if (!Array.isArray(value)) return { unavailable: "not_array" }
    if (value.length > limit) truncated = true
    return { length: value.length, values: value.slice(0, limit).map(mapper) }
  }
  const rows = (value, names, fields) => array(value, names.length, (entry) => ({
    name: names.includes(entry?.name) ? entry.name : "unrecognized",
    ...Object.fromEntries(fields.map((field) => [field, counter(entry?.[field])])),
    latency_log2_us: array(entry?.latency_log2_us, 32, counter),
  }))
  const snapshot = (value) => {
    if (!object(value)) return { available: false }
    return {
      schema_version: [NATIVE_DIAGNOSTICS_SCHEMA, "mount-rs.storage-diagnostics.v1"].includes(value.schema_version) ? value.schema_version : "unavailable",
      enabled: typeof value.enabled === "boolean" ? value.enabled : "unavailable",
      scope: value.scope === "process" ? "process" : "unavailable",
      measurement: {
        storage_calls: value.measurement?.storage_calls === STORAGE_CALL_SEMANTICS ? STORAGE_CALL_SEMANTICS : "unavailable",
        storage_bytes: value.measurement?.storage_bytes === STORAGE_BYTE_SEMANTICS ? STORAGE_BYTE_SEMANTICS : "unavailable",
        storage_rows: value.measurement?.storage_rows === STORAGE_ROW_SEMANTICS ? STORAGE_ROW_SEMANTICS : "unavailable",
        storage_operations: isDeepStrictEqual(value.measurement?.storage_operations, storageNames) ? [...storageNames] : "unavailable",
        storage_families: isDeepStrictEqual(value.measurement?.storage_families, STORAGE_OPERATION_FAMILIES) ? structuredClone(STORAGE_OPERATION_FAMILIES) : "unavailable",
        storage_instrumented_operations: isDeepStrictEqual(value.measurement?.storage_instrumented_operations, STORAGE_INSTRUMENTED_OPERATION_NAMES) ? [...STORAGE_INSTRUMENTED_OPERATION_NAMES] : "unavailable",
        tidb_coverage: isDeepStrictEqual(value.measurement?.tidb_coverage, TIDB_DIAGNOSTIC_COVERAGE) ? structuredClone(TIDB_DIAGNOSTIC_COVERAGE) : "unavailable",
        r2_api: isDeepStrictEqual(value.measurement?.r2_api, rawMeasurement) ? structuredClone(rawMeasurement) : "unavailable",
        r2_local: isDeepStrictEqual(value.measurement?.r2_local, OBJECT_STORE_LOCAL_MEASUREMENT) ? structuredClone(OBJECT_STORE_LOCAL_MEASUREMENT) : "unavailable",
        latency_histogram: isDeepStrictEqual(value.measurement?.latency_histogram, { unit: "microseconds", intervals: histogramIntervals }) ? { unit: "microseconds", intervals: histogramIntervals } : "unavailable" },
      storage: { in_flight: counter(value.storage?.in_flight), entries: rows(value.storage?.entries, storageNames, ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "in_flight", "elapsed_ns"]) },
      r2: { scope: value.r2?.scope === "process_live_instances" ? "process_live_instances" : "unavailable",
        available: value.r2?.available !== false && Array.isArray(value.r2?.instances),
        instances: array(value.r2?.instances, 32, (entry) => ({
          id: counter(entry?.id), ...Object.fromEntries(logicalR2Fields.map((field) => [field, counter(entry?.[field])])),
          raw_api: object(entry?.raw_api) ? {
            schema: entry.raw_api.schema === rawSchema ? rawSchema : "unavailable",
            scope: entry.raw_api.scope === rawScope ? rawScope : "unavailable",
            saturated: typeof entry.raw_api.saturated === "boolean" ? entry.raw_api.saturated : "unavailable",
            in_flight: counter(entry.raw_api.in_flight), pending_claims: counter(entry.raw_api.pending_claims),
            claims: Object.fromEntries(claimFields.map((field) => [field, counter(entry.raw_api.claims?.[field])])),
            entries: rows(entry.raw_api.entries, rawNames, [...rawFields, "latency_max_ns"]),
          } : { unavailable: entry?.raw_api === null ? "null" : "missing" },
          local_work: localObservation(entry?.local_work),
        })) },
    }
  }
  const observations = { before: snapshot(before), after: snapshot(after) }
  return { ...observations, truncated }
}

function retainIncompleteEvidence(delta, before, after) {
  if (!delta.complete) {
    delta.observations ??= sanitizedObservations(before, after)
    // These existing banks have labels outside this raw API's closed schema.
    // Keep their whitelisted endpoint counters in observations on failure.
    delete delta.storage
    delete delta.profile
    delete delta.sqlite
  }
  return delta
}

export function deltaNativeSnapshots(before, after) {
  try {
    if (before.schema_version !== NATIVE_DIAGNOSTICS_SCHEMA || after.schema_version !== before.schema_version || before.enabled !== true || after.enabled !== true) invalid("diagnostic version or enabled state changed")
    if (before.scope !== "process" || after.scope !== before.scope || before.quiescent_snapshot_required !== true || after.quiescent_snapshot_required !== true || before.elapsed_semantics !== "inclusive_wall_nanoseconds" || after.elapsed_semantics !== before.elapsed_semantics) invalid("native diagnostic scope changed")
    const withoutLocal = (measurement) => object(measurement) ? Object.fromEntries(Object.entries(measurement).filter(([name]) => name !== "r2_local")) : measurement
    if (!before.measurement || !isDeepStrictEqual(withoutLocal(before.measurement), withoutLocal(after.measurement)) ||
        !isDeepStrictEqual(after.measurement.latency_histogram, { unit: "microseconds", intervals: histogramIntervals }) ||
        after.measurement.storage_calls !== STORAGE_CALL_SEMANTICS ||
        after.measurement.storage_bytes !== STORAGE_BYTE_SEMANTICS ||
        after.measurement.storage_rows !== STORAGE_ROW_SEMANTICS ||
        !isDeepStrictEqual(after.measurement.storage_operations, storageNames) ||
        !isDeepStrictEqual(after.measurement.storage_families, STORAGE_OPERATION_FAMILIES) ||
        !isDeepStrictEqual(after.measurement.storage_instrumented_operations, STORAGE_INSTRUMENTED_OPERATION_NAMES) ||
        !isDeepStrictEqual(after.measurement.tidb_coverage, TIDB_DIAGNOSTIC_COVERAGE) ||
        after.measurement.storage_duration !== "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" ||
        after.measurement.sqlite !== "live_connection_pager_and_sql_category_counters; pager_bytes_are_page_size_estimates" ||
        after.measurement.profile !== "existing_core_profile_counters" ||
        after.measurement.r2 !== "live_store_logical_calls_and_cache_hits; not_http_attempts" ||
        !isDeepStrictEqual(after.measurement.r2_api, rawMeasurement) ||
        !isDeepStrictEqual(after.measurement.unavailable, {
          http_attempts: "unavailable", internal_successful_retries: "unavailable",
          physical_device_iops: "unavailable", tidb_pool_wait: "isolated_queue_only_wait_unavailable",
          native_allocation_count: "unavailable", js_allocation_count: "unavailable",
        }) ||
        after.measurement.forwarding_boxes !== "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations") invalid("measurement metadata changed")
    if (!isDeepStrictEqual(before.backend_waits, after.backend_waits) || !isDeepStrictEqual(after.backend_waits, { pglite_client_lock: "instrumented", tidb_pool: "instrumented_inclusive_checkout_including_lazy_connect_and_session_configuration" }) ||
        before.http_attempts !== after.http_attempts || before.physical_device_iops !== after.physical_device_iops || after.http_attempts !== "unavailable" || after.physical_device_iops !== "unavailable") invalid("availability metadata changed")
    validateStorageRows(before.storage?.entries)
    validateStorageRows(after.storage?.entries)
    const boxBefore = before.storage.forwarding_boxes
    const boxAfter = after.storage.forwarding_boxes
    if (boxBefore?.sites !== "napi_dynamic_provider_forwarding_future" || boxAfter?.sites !== boxBefore.sites) invalid("forwarding allocation site changed")
    const storage = { in_flight_start: before.storage.in_flight, in_flight_end: after.storage.in_flight,
      forwarding_boxes: { sites: boxAfter.sites, calls: subtract(boxAfter.calls, boxBefore.calls), requested_object_bytes: subtract(boxAfter.requested_object_bytes, boxBefore.requested_object_bytes) },
      entries: entriesDelta(before.storage.entries, after.storage.entries, "name", ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "latency_log2_us"]).map((entry, index) => ({
        ...entry, in_flight_start: before.storage.entries[index].in_flight,
        in_flight_end: after.storage.entries[index].in_flight,
      })) }
    validateStorageRows(storage.entries, true)
    const profile = { entries: entriesDelta(before.profile.entries, after.profile.entries, "name", ["calls", "elapsed_ns", "units"]) }
    const sqlite = connectionDelta(before.sqlite.connections, after.sqlite.connections)
    const r2 = r2Delta(before.r2, after.r2, before.measurement.r2_local, after.measurement.r2_local)
    const issues = []
    if (typeof storage.in_flight_start !== "string" || typeof storage.in_flight_end !== "string" ||
        !decimal.test(storage.in_flight_start) || !decimal.test(storage.in_flight_end)) invalid("invalid in-flight gauge")
    if (BigInt(storage.in_flight_start) !== 0n || BigInt(storage.in_flight_end) !== 0n) issues.push("instrumented storage operation crossed phase boundary")
    if (storage.entries.some((entry) => integer(entry.in_flight_start) !== 0n || integer(entry.in_flight_end) !== 0n)) issues.push("instrumented storage row crossed phase boundary")
    if (!sqlite.complete) issues.push("SQLite connection closed during phase")
    if (!r2.complete) issues.push("R2 instance closed during phase")
    const result = { schema_version: NATIVE_DIAGNOSTICS_SCHEMA, complete: issues.length === 0, issues,
      measurement: { ...Object.fromEntries(["storage_calls", "storage_bytes", "storage_rows", "storage_operations", "storage_families", "storage_instrumented_operations", "tidb_coverage", "storage_duration", "latency_histogram", "forwarding_boxes", "profile", "sqlite", "r2", "r2_api", "unavailable"].map((field) => [field, after.measurement[field]])),
        r2_local: isDeepStrictEqual(after.measurement.r2_local, OBJECT_STORE_LOCAL_MEASUREMENT) ? structuredClone(OBJECT_STORE_LOCAL_MEASUREMENT) : "unavailable" },
      backend_waits: after.backend_waits, http_attempts: after.http_attempts,
      physical_device_iops: after.physical_device_iops, storage, profile, sqlite, r2 }
    return retainIncompleteEvidence(result, before, after)
  } catch (error) { return { complete: false, issues: [error instanceof DiagnosticValidationError ? error.message : "invalid native diagnostic shape"], observations: sanitizedObservations(before, after) } }
}

// The artifact gate validates processed evidence independently of complete=true.
export function validateRawPhaseDiagnostics(phase) {
  if (!object(phase) || phase.quiescent !== true || !object(phase.native)) invalid("workload diagnostics unavailable or not quiescent")
  const native = phase.native
  if (native.schema_version !== NATIVE_DIAGNOSTICS_SCHEMA || native.complete !== true || !Array.isArray(native.issues) || native.issues.length !== 0) invalid("workload native diagnostics incomplete")
  if (integer(native.storage?.in_flight_start) !== 0n || integer(native.storage?.in_flight_end) !== 0n) invalid("workload global storage operation pending")
  validateStorageRows(native.storage?.entries, true)
  if (native.measurement?.storage_calls !== STORAGE_CALL_SEMANTICS || native.measurement?.storage_bytes !== STORAGE_BYTE_SEMANTICS || native.measurement?.storage_rows !== STORAGE_ROW_SEMANTICS ||
      !isDeepStrictEqual(native.measurement?.storage_operations, storageNames) || !isDeepStrictEqual(native.measurement?.storage_families, STORAGE_OPERATION_FAMILIES) ||
      !isDeepStrictEqual(native.measurement?.storage_instrumented_operations, STORAGE_INSTRUMENTED_OPERATION_NAMES) ||
      !isDeepStrictEqual(native.measurement?.tidb_coverage, TIDB_DIAGNOSTIC_COVERAGE)) invalid("workload storage measurement metadata changed")
  if (!isDeepStrictEqual(native.measurement?.r2_api, rawMeasurement) || !isDeepStrictEqual(native.measurement?.latency_histogram, { unit: "microseconds", intervals: histogramIntervals })) invalid("workload raw measurement metadata changed")
  const r2 = native.r2
  if (!object(r2) || r2.scope !== "process_live_instances" || r2.complete !== true || r2.internal_successful_retries !== "unavailable" || !Array.isArray(r2.missing_instance_ids) || r2.missing_instance_ids.length !== 0) invalid("workload R2 registry incomplete")
  const instances = instanceMap(r2.instances)
  if (instances.size === 0) invalid("workload R2 registry empty")
  const expectedIds = [...instances.keys()]
  for (const ids of [r2.instance_ids_start, r2.instance_ids_end]) {
    if (!Array.isArray(ids) || ids.length !== instances.size || new Set(ids).size !== ids.length || !ids.every((id) => expectedIds.includes(id))) invalid("workload R2 identities changed")
    ids.forEach((id) => integer(id, true))
  }
  for (const instance of instances.values()) {
    if (instance.opened_during_phase !== false) invalid("workload R2 instance opened during phase")
    for (const field of logicalR2Fields) integer(instance[field])
    const raw = instance.raw_api
    if (!object(raw) || raw.schema !== rawSchema || raw.scope !== rawScope || raw.saturated_start !== false || raw.saturated_end !== false) invalid("workload raw API schema or saturation unavailable")
    for (const field of ["in_flight_start", "in_flight_end", "pending_claims_start", "pending_claims_end"]) if (integer(raw[field], true) !== 0n) invalid("workload raw operation or claim pending")
    validateClaims(raw.claims)
    validateRawRows(raw.entries, true)
  }
  return expectedIds
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
  if (name.startsWith("workload-") && delta.r2?.instances.some((instance) => instance.opened_during_phase)) {
    delta.complete = false
    delta.issues.push("R2 instance opened during workload phase")
  }
  retainIncompleteEvidence(delta, before.native, after.native)
  const cpu = { user_us: String(after.cpuStart.user - before.cpuEnd.user), system_us: String(after.cpuStart.system - before.cpuEnd.system) }
  const observerCpu = { user_us: String((before.cpuEnd.user - before.cpuStart.user) + (after.cpuEnd.user - after.cpuStart.user)), system_us: String((before.cpuEnd.system - before.cpuStart.system) + (after.cpuEnd.system - after.cpuStart.system)) }
  const resources = { voluntary_context_switches: String(after.resourcesStart.voluntaryContextSwitches - before.resourcesEnd.voluntaryContextSwitches), involuntary_context_switches: String(after.resourcesStart.involuntaryContextSwitches - before.resourcesEnd.involuntaryContextSwitches) }
  return { name, elapsed_ms: after.started - before.ended, observer_snapshot_ms: (before.ended - before.started) + (after.ended - after.started), quiescent, native: delta, process: { cpu_work: cpu, cpu_observer: observerCpu, memory_end_bytes: Object.fromEntries(Object.entries(after.memory).map(([key, value]) => [key, String(value)])), resources_work: resources, native_allocation_count: "unavailable", js_allocation_count: "unavailable" } }
}

export function logPhaseSummary(phase) {
  const entries = phase.native.storage?.entries
  const available = Array.isArray(entries)
  const instrumented = phase.native.measurement?.storage_instrumented_operations || []
  const families = Object.fromEntries(Object.entries(STORAGE_OPERATION_FAMILIES).map(([name, semantics]) => {
    const observed = semantics.operations.filter((operation) => instrumented.includes(operation))
    const coverage = { instrumented: observed.length === semantics.operations.length, instrumented_operations: observed, available }
    if (!available) return [name, coverage]
    const rows = entries.filter((entry) => semantics.operations.includes(entry.name))
    return [name, { ...coverage, ...Object.fromEntries(["calls", "bytes", "error", "cancelled", "elapsed_ns", "returned_rows", "returned_row_observations"].map((field) => [field, rows.reduce((sum, entry) => sum + BigInt(entry[field]), 0n).toString()])) }]
  }))
  const instances = Array.isArray(phase.native.r2?.instances) ? phase.native.r2.instances : []
  const observedLocal = instances.flatMap((instance) => {
    try { return [validateLocalPhaseDiagnostics(instance.local_work, phase.native.measurement?.r2_local)] } catch { return [] }
  })
  const localWork = { status: observedLocal.length ? observedLocal.length === instances.length ? "observed" : "partial" : "unavailable",
    observed_instances: observedLocal.length, unavailable_instances: instances.length - observedLocal.length,
    scope: "inclusive_local_wall_time; concurrent_spans_overlap; copy_bytes_are_not_network_bytes; not_cpu_or_device_iops" }
  if (observedLocal.length) localWork.entries = OBJECT_STORE_LOCAL_NAMES.map((name, index) => ({ name,
    ...Object.fromEntries(localFields.map((field) => [field, observedLocal.reduce((sum, local) => sum + BigInt(local.entries[index][field]), 0n).toString()])) }))
  process.stderr.write(`MOUNT_RS_STORAGE_PHASE ${JSON.stringify({ name: phase.name, complete: phase.native.complete, elapsed_ms: phase.elapsed_ms, families, local_work: localWork, scope: "inclusive_instrumented_operations; families_overlap; bytes_only_known_payload; rows_only_known_observations; not_application_or_device_iops" })}\n`)
}
