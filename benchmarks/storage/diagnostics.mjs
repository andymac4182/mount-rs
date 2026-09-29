import { isDeepStrictEqual } from "node:util"

// Opt-in quiescent phase snapshots. Native counters are exact decimal strings.
const decimal = /^(?:0|[1-9]\d*)$/u
const u64Maximum = 18446744073709551615n
const signedDecimal = /^(?:0|[1-9]\d*|-[1-9]\d*)$/u
const i64Minimum = -9223372036854775808n
const i64Maximum = 9223372036854775807n
const sqliteCategories = ["SELECT", "INSERT", "UPDATE", "DELETE", "BEGIN", "COMMIT", "ROLLBACK", "PRAGMA", "OTHER"]
const sqlitePagerFields = ["cache_hits", "cache_misses", "page_writes", "cache_spills"]
const sqliteConfigurationFields = ["journal_mode", "locking_mode", "is_autocommit", "synchronous", "busy_timeout_ms", "fullfsync", "checkpoint_fullfsync", "wal_autocheckpoint_pages", "cache_size"]
const sqliteJournalModes = ["delete", "truncate", "persist", "memory", "wal", "off"]
const sqliteLockingModes = ["normal", "exclusive"]
const sqliteTimingFields = ["completed", "overflow", "elapsed_ns", "max_elapsed_ns", "invalid_elapsed", "histogram_log2_us"]
const sqliteScopes = {
  pager_sampling: "before observer queries; reset after queries; repeated nonreset samples may include prior observer pager work",
  sql_profile_scope: "SQLite PROFILE completion notifications, not successes; approximate VFS wall clock, bundled SQLite 1ms resolution; excludes post-PROFILE WAL callbacks",
  connection_lock_hold_scope: "Instant wall time from acquired provider connection mutex guard to guard Drop before unlock; includes SQL, busy waits and commit; excludes acquisition and observer locks",
  provider_begin_scope: "Instant wall time for BEGIN IMMEDIATE call including SQLite busy waiting; block_put and all MRC5 compact transactions only; encloses BEGIN SQL PROFILE interval",
  provider_commit_scope: "Instant wall time for Transaction::commit call including error Drop rollback; block_put and all MRC5 compact transactions only; encloses COMMIT SQL PROFILE interval",
  wal_state_scope: "sequential main WAL frame gauges from drained observer PRAGMA wal_checkpoint(NOOP); no backfill; may initialize or read WAL state; gauges are shared across connections to one database and are not additive or checkpoint work counts",
}
const sqliteTimingMeasurement = {
  duration: "inclusive_wall_nanoseconds; lock_wait_hold_begin_commit_and_sql_profile_overlap",
  latency_max: "cumulative_per_connection; exact_phase_max_unavailable",
  latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, bucket) => ({
    lower_inclusive_us: bucket === 0 ? "0" : String(2 ** bucket),
    upper_exclusive_us: bucket === 31 ? null : String(2 ** (bucket + 1)),
    ...(bucket === 31 ? { terminal_overflow: true } : {}),
  })) },
}
const sqliteObserverScope = "Instant wall time for sequential registry lock and per-connection observer collection; excludes final outer JSON serialization; not workload time"
export const SQLITE_VFS_MEASUREMENT = {
  schema: "mount-rs.sqlite-vfs.v1",
  scope: "process selected observed native VFS invocations including external connections and connection close; excludes observer read/write/sync and checkpoint signals, SHM, mmap and other VFS operations; not syscalls or physical device IOPS",
  byte_scope: "requested native xRead/xWrite bytes; confirmed only on SQLITE_OK; partial error bytes unavailable; xSync bytes are zero",
  lifecycle_scope: "native file/context lifecycle including observer sidecar opens and closes; live gauges are never reset",
  checkpoint_scope: "matched Instant wall time from CKPT_START arrival to CKPT_DONE arrival; backfill copy window only, after initial WAL sync and before final database truncate/sync; signals do not prove checkpoint success",
}
export const SQLITE_VFS_ENTRY_NAMES = ["main_database", "main_journal", "wal", "temporary", "other"].flatMap((role) => ["read", "write", "sync"].map((operation) => `${role}.${operation}`))
const sqliteVfsCounterFields = ["open_attempts", "open_errors", "files_opened", "close_calls", "close_errors"]
const sqliteVfsGaugeFields = ["in_flight", "live_files", "registered_vfs", "live_contexts"]
const sqliteVfsByteFields = ["requested_bytes", "confirmed_bytes", "short_reads"]
const sqliteCheckpointCounterFields = ["starts", "dones", "unmatched_starts", "unmatched_dones", "aborted_windows"]
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
export const RUSTFS_LOCAL_MEASUREMENT = {
  ...OBJECT_STORE_LOCAL_MEASUREMENT,
  scope: "live_registered_split_rustfs_block_store_instances",
  excluded: OBJECT_STORE_LOCAL_MEASUREMENT.excluded.map((name) => name === "unregistered_rust_factories_and_mount_r2" ? "unregistered_rustfs_factories" : name),
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
  "tidb.sql.flush_probe",
  "foundationdb.transaction.create",
  "foundationdb.transaction.closure_attempt",
  "foundationdb.read.get",
  "foundationdb.read.get_key",
  "foundationdb.read.get_range_page",
  "foundationdb.transaction.commit",
  "foundationdb.transaction.on_error",
  "blob_cache.miss.admission_wait",
  "blob_cache.miss.singleflight_wait",
  "blob_cache.ram.lookup",
  "blob_cache.disk.lookup",
  "blob_cache.peer.connection_lock_wait",
  "blob_cache.peer.connection_establish",
  "client.quic.open_bi",
  "client.quic.request_send",
  "client.quic.response_receive",
  "blob_cache.peer.request_byte_admission_wait",
  "blob_cache.peer.open_bi",
  "blob_cache.peer.request_send",
  "blob_cache.peer.response_receive",
  "blob_cache.peer.get",
  "blob_cache.peer.get_miss",
  "client.websocket.tcp_connect",
  "client.websocket.tls_handshake",
  "client.websocket.upgrade",
  "client.websocket.socket_lock_wait",
  "client.websocket.request_encode",
  "client.websocket.request_send",
  "client.websocket.response_receive",
  "client.websocket.response_decode",
  "client.quic.connection_setup",
  "blob_cache.discovery.locate",
  "object_store.backing_marker.probe.get",
  "object_store.backing_marker.probe.body_read",
  "object_store.backing_marker.data.get",
  "object_store.backing_marker.data.body_read",
  "object_store.backing_marker.probe.create",
  "object_store.backing_marker.retry_backoff",
  "sdk.metadata.compact_root_file_capability",
  "sdk.metadata.load_compact_root_file",
]
const storageNames = STORAGE_OPERATION_NAMES
export const STORAGE_CALL_SEMANTICS = "fixed_label_provider_and_driver_operations; families_overlap_and_are_not_application_iops"
export const STORAGE_BYTE_SEMANTICS = "known_successful_stage_specific_bytes; payload_or_plaintext_envelope_as_declared_by_family; zero_does_not_establish_no_payload"
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
  foundationdb_transaction: { operations: ["foundationdb.transaction.create", "foundationdb.transaction.closure_attempt", "foundationdb.transaction.commit", "foundationdb.transaction.on_error"], calls: "provider_closure_attempts_and_native_create_commit_on_error_invocations; distinct_units_not_logical_transactions", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  foundationdb_read: { operations: ["foundationdb.read.get", "foundationdb.read.get_key", "foundationdb.read.get_range_page"], calls: "native_client_read_method_invocations; range_page_calls_not_key_value_count", bytes: "known_selected_successful_returned_value_key_and_range_page_key_value_payload_bytes_only", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  blob_cache: { operations: ["blob_cache.miss.admission_wait", "blob_cache.miss.singleflight_wait", "blob_cache.ram.lookup", "blob_cache.disk.lookup", "blob_cache.peer.connection_lock_wait", "blob_cache.peer.connection_establish"], calls: "cache_stage_invocations; lookups_include_hits_and_misses; waits_count_acquisitions_or_termination", bytes: "known_successful_ram_and_disk_lookup_returned_payload_bytes_only; waits_and_connection_stages_zero; misses_zero", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
  client_quic: { operations: ["client.quic.open_bi"], calls: "stream_acquisition_invocations; includes_success_error_and_cancellation; not_requests_or_acknowledgments", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_open_bi_await_nanoseconds; not_exclusive_cpu_or_network_time" },
  client_quic_request_send: { operations: ["client.quic.request_send"], calls: "request_send_stage_invocations; includes_success_error_and_cancellation; not_acknowledgments", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_write_and_fin_submission_nanoseconds; not_acknowledgment_or_exclusive_cpu_time" },
  client_quic_response_receive: { operations: ["client.quic.response_receive"], calls: "response_receive_stage_invocations; includes_success_error_and_cancellation; not_server_operations", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_decode_and_eof_validation_nanoseconds; not_exclusive_cpu_or_network_time" },
  blob_cache_peer_request_byte_admission_wait: { operations: ["blob_cache.peer.request_byte_admission_wait"], calls: "request_byte_permit_acquisition_invocations; includes_success_error_and_cancellation", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_request_byte_permit_await_nanoseconds; overlaps_get_duration" },
  blob_cache_peer_open_bi: { operations: ["blob_cache.peer.open_bi"], calls: "stream_acquisition_invocations; includes_success_error_and_cancellation; not_requests_or_acknowledgments", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_open_bi_await_nanoseconds; overlaps_get_duration" },
  blob_cache_peer_request_send: { operations: ["blob_cache.peer.request_send"], calls: "request_send_stage_invocations; includes_success_error_and_cancellation; not_acknowledgments", bytes: "known_successfully_submitted_plaintext_request_header_and_payload_bytes; not_wire_bytes_or_acknowledgments", returned_rows: "unavailable", duration: "inclusive_write_and_fin_submission_nanoseconds; overlaps_get_duration" },
  blob_cache_peer_response_receive: { operations: ["blob_cache.peer.response_receive"], calls: "response_receive_stage_invocations; includes_success_error_and_cancellation; not_backing_reads", bytes: "known_successfully_validated_plaintext_status_and_body_bytes; not_wire_bytes", returned_rows: "unavailable", duration: "inclusive_response_read_and_validation_nanoseconds; overlaps_get_duration" },
  blob_cache_peer_get: { operations: ["blob_cache.peer.get"], calls: "logical_peer_get_and_get_shared_invocations; includes_hits_misses_errors_and_cancellation", bytes: "known_successful_logical_get_payload_bytes; misses_zero", returned_rows: "unavailable", duration: "inclusive_get_method_nanoseconds; includes_request_and_existing_return_conversion; overlaps_transport_stages" },
  blob_cache_peer_get_miss: { operations: ["blob_cache.peer.get_miss"], calls: "successful_get_miss_classifications; not_peer_requests", bytes: "unavailable", returned_rows: "unavailable", duration: "classification_marker_nanoseconds; excludes_get_request_duration" },
  client_websocket: { operations: ["client.websocket.tcp_connect", "client.websocket.tls_handshake", "client.websocket.upgrade", "client.websocket.socket_lock_wait", "client.websocket.request_encode", "client.websocket.request_send", "client.websocket.response_receive", "client.websocket.response_decode"], calls: "client_stage_invocations; includes_success_error_and_cancellation; not_requests_or_acknowledgments", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_stage_wall_nanoseconds; nested_and_parallel_spans_overlap; not_exclusive_cpu_or_network_time" },
  client_quic_connection_setup: { operations: ["client.quic.connection_setup"], calls: "quic_transport_setup_attempts; excludes_credentials_hello_and_websocket_fallback", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_tls_config_endpoint_connect_and_alpn_validation_nanoseconds; not_exclusive_cpu_or_network_time" },
  blob_cache_discovery: { operations: ["blob_cache.discovery.locate"], calls: "discovery_locate_invocations; empty_and_fallback_peer_lists_are_success; not_peer_gets_or_directory_health", bytes: "unavailable", returned_rows: "unavailable", duration: "inclusive_locate_await_nanoseconds; excludes_peer_filtering_queries_and_hedging" },
  object_store_backing_marker: { operations: STORAGE_OPERATION_NAMES.filter((name) => name.startsWith("object_store.backing_marker.")), calls: "object_store_marker_get_body_create_and_backoff_invocations; includes_success_error_and_cancellation; not_http_attempts_or_application_iops", bytes: "known_successful_materialized_body_bytes_before_identity_validation_and_accepted_create_input_bytes; get_backoff_error_and_cancellation_bytes_unavailable", returned_rows: "unavailable", duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap" },
}
export const TIDB_DIAGNOSTIC_COVERAGE = {
  schema: "mount-rs-tidb-client-diagnostic-coverage-v1", status: "source_sites_instrumented",
  pool_checkout_sites: "35", session_configure_sites: "1", schema_initialize_sites: "1", metadata_open_sites: "1",
  transaction_begin_sites: "3", transaction_commit_sites: "1", transaction_rollback_sites: "5", sql_statement_sites: "57",
  operations: ["tidb.pool.checkout", "tidb.session.configure", "tidb.open.schema", "tidb.open.metadata_row", "tidb.tx.begin.metadata", "tidb.tx.begin.inode", "tidb.tx.begin.compact_read", "tidb.tx.commit", "tidb.tx.rollback", "tidb.sql.session", "tidb.sql.ddl", "tidb.sql.metadata_read", "tidb.sql.metadata_write", "tidb.sql.inode_read", "tidb.sql.inode_write", "tidb.sql.block_read", "tidb.sql.block_write", "tidb.sql.flush_probe"],
  sql_returned_rows_scope: "successful SELECT Option/Vec results only; exec_iter affected_rows excluded",
  sql_payload_bytes_scope: "successful block INSERT submitted bytes and block-body SELECT returned bytes only; other SQL payload bytes unavailable",
  pool_checkout_scope: "pool.get_conn await includes possible lazy connection and session setup; queue-only wait unavailable",
  unavailable: ["pool_queue_only_wait", "separate_driver_connect_handshake", "server_sql_execution_time", "transaction_lifetime_and_implicit_drop_rollback", "affected_rows", "other_sql_payload_bytes", "sql_wire_bytes_and_client_internal_retries", "tikv_and_physical_device_io"],
}
// A bank label does not establish source coverage. Keep the reviewed legacy
// allowlist fixed; FoundationDB availability comes from each native snapshot.
const legacyInstrumentedNames = new Set([
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
  "sdk.blocks.reconcile"
])
export const FOUNDATIONDB_DIAGNOSTIC_COVERAGE = {
  "schema": "mount-rs-foundationdb-client-diagnostic-coverage-v1",
  "status": "source_sites_instrumented",
  "transaction_runner_sites": "6",
  "point_get_sites": "2",
  "get_key_sites": "1",
  "range_consumer_sites": "3",
  "operations": [
    "foundationdb.transaction.create",
    "foundationdb.transaction.closure_attempt",
    "foundationdb.read.get",
    "foundationdb.read.get_key",
    "foundationdb.read.get_range_page",
    "foundationdb.transaction.commit",
    "foundationdb.transaction.on_error"
  ],
  "attempt_scope": "audited_provider_closure_and_native_client_dispatch_attempts; process_fixed_label_bank",
  "payload_bytes_scope": "successful_get_value_get_key_key_and_range_page_key_value_return_lengths_only; no_key_contents",
  "returned_rows_scope": "unavailable; storage_returned_rows_are_SQL_only",
  "unavailable": [
    "client_internal_retries",
    "wire_rpc_count",
    "wire_bytes",
    "server_execution_time",
    "physical_device_iops",
    "exclusive_cpu",
    "transaction_lifetime",
    "native_operation_settlement_after_future_drop",
    "durability_or_rollback_after_commit_error_or_cancellation"
  ]
}
export const FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE = {
  "schema": "mount-rs-foundationdb-client-diagnostic-coverage-v1",
  "status": "unavailable",
  "reason": "feature_disabled_or_unsupported_target",
  "operations": []
}
// Compatibility export describes feature-off coverage, not every bank row.
// This literal inventory audits producer sites independently of registry declarations.
export const OBJECT_STORE_BACKING_MARKER_INSTRUMENTED_OPERATIONS = Object.freeze([
  "object_store.backing_marker.probe.get",
  "object_store.backing_marker.probe.body_read",
  "object_store.backing_marker.data.get",
  "object_store.backing_marker.data.body_read",
  "object_store.backing_marker.probe.create",
  "object_store.backing_marker.retry_backoff",
])
export const STORAGE_INSTRUMENTED_OPERATION_NAMES = storageNames.filter((name) => legacyInstrumentedNames.has(name) || TIDB_DIAGNOSTIC_COVERAGE.operations.includes(name) || OBJECT_STORE_BACKING_MARKER_INSTRUMENTED_OPERATIONS.includes(name))
function foundationdbCoverage(value) {
  if (isDeepStrictEqual(value, FOUNDATIONDB_DIAGNOSTIC_COVERAGE)) return FOUNDATIONDB_DIAGNOSTIC_COVERAGE
  if (isDeepStrictEqual(value, FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE)) return FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE
  return undefined
}
export function storageInstrumentedOperationNames(coverage) {
  const audited = foundationdbCoverage(coverage)
  if (!audited) invalid("FoundationDB coverage metadata unavailable")
  return storageNames.filter((name) => legacyInstrumentedNames.has(name) || TIDB_DIAGNOSTIC_COVERAGE.operations.includes(name) || audited.operations.includes(name) || OBJECT_STORE_BACKING_MARKER_INSTRUMENTED_OPERATIONS.includes(name))
}
function validInstrumentedOperations(measurement) {
  const coverage = foundationdbCoverage(measurement?.foundationdb_coverage)
  if (!coverage) return undefined
  const names = storageInstrumentedOperationNames(coverage)
  return isDeepStrictEqual(measurement?.storage_instrumented_operations, names) ? names : undefined
}
const rawMeasurement = {
  schema: rawSchema, scope: "live_registered_split_r2_block_store_instances",
  calls: "object_store_adapter_method_invocations; not_http_attempts_or_internal_retries",
  duration: "inclusive_wall_nanoseconds_at_invoked_adapter_await; excludes_argument_preparation",
  upload_bytes: "attempted=submitted_payload; confirmed=put_opts_ok_only",
  returned_bytes: "successful_body_materialization_before_integrity_validation",
  latency_max: "cumulative_per_instance; exact_phase_max_unavailable", reconcile_listing: "unavailable",
  excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "internal_client_retries"],
}
export const RUSTFS_API_MEASUREMENT = {
  ...rawMeasurement,
  scope: "live_registered_split_rustfs_block_store_instances",
  excluded: rawMeasurement.excluded.map((name) => name === "unregistered_rust_factories_and_mount_r2" ? "unregistered_rustfs_factories" : name),
}

function objectStoreFamily(family) {
  if (family === "r2") return { name: "R2", api: rawMeasurement, local: OBJECT_STORE_LOCAL_MEASUREMENT }
  if (family === "rustfs") return { name: "RustFS", api: RUSTFS_API_MEASUREMENT, local: RUSTFS_LOCAL_MEASUREMENT }
  invalid("object-store diagnostic family unavailable")
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
    if (entry.name.startsWith("foundationdb.transaction.") && entry.bytes !== "0") invalid("FoundationDB transaction payload bytes unavailable")
    if (BigInt(entry.returned_row_observations) > BigInt(entry.success)) invalid("returned-row observations exceed successful calls")
    if (entry.returned_row_observations === "0" && entry.returned_rows !== "0") invalid("returned rows have no known observations")
    if (!Array.isArray(entry.latency_log2_us) || entry.latency_log2_us.length !== 32 || entry.latency_log2_us.reduce((sum, count) => sum + integer(count), 0n) !== BigInt(entry.calls)) invalid("storage histogram does not reconcile")
    if (phase) {
      if (integer(entry.in_flight_start) !== 0n || integer(entry.in_flight_end) !== 0n) invalid("storage operation row pending")
    } else integer(entry.in_flight)
  }
}
function exactFields(value, fields, reason) {
  if (!object(value) || Object.keys(value).length !== fields.length || !fields.every((field) => Object.hasOwn(value, field))) invalid(reason)
}
function checkedTotal(values, reason) {
  const total = values.reduce((sum, value) => sum + integer(value), 0n)
  if (total > u64Maximum) invalid(reason)
  return total
}
function signedInteger(value) {
  if (typeof value !== "string" || value.length > 20 || !signedDecimal.test(value)) invalid("invalid SQLite configuration integer")
  const number = BigInt(value)
  if (number < i64Minimum || number > i64Maximum) invalid("SQLite configuration exceeds i64")
  return number
}
function validateSqliteConfiguration(configuration) {
  exactFields(configuration, sqliteConfigurationFields, "SQLite configuration fields unavailable")
  if (!sqliteJournalModes.includes(configuration.journal_mode) || !sqliteLockingModes.includes(configuration.locking_mode) || typeof configuration.is_autocommit !== "boolean") invalid("SQLite configuration enum unavailable")
  if (!configuration.is_autocommit) invalid("SQLite transaction crossed phase boundary")
  for (const field of sqliteConfigurationFields.slice(3)) {
    const number = signedInteger(configuration[field])
    if (field !== "cache_size" && number < 0n) invalid("negative SQLite configuration")
    if (["fullfsync", "checkpoint_fullfsync"].includes(field) && number > 1n) invalid("SQLite boolean configuration unavailable")
    if (field === "synchronous" && number > 3n) invalid("SQLite synchronous configuration unavailable")
  }
}
function validateSqliteTiming(timing, errors = false, phase = false) {
  const maximumFields = phase ? ["max_elapsed_ns_start", "max_elapsed_ns_end", "exact_phase_max_ns"] : ["max_elapsed_ns"]
  exactFields(timing, [...sqliteTimingFields.filter((field) => field !== "max_elapsed_ns"), ...maximumFields, ...(errors ? ["errors"] : [])], "SQLite timing fields unavailable")
  if (timing.overflow !== false) invalid("SQLite timing overflow state unavailable")
  const completed = integer(timing.completed)
  const elapsed = integer(timing.elapsed_ns)
  const invalidElapsed = integer(timing.invalid_elapsed)
  if (!Array.isArray(timing.histogram_log2_us) || timing.histogram_log2_us.length !== 32) invalid("SQLite timing histogram shape changed")
  const valid = checkedTotal(timing.histogram_log2_us, "SQLite timing histogram overflow")
  if (valid + invalidElapsed !== completed) invalid("SQLite timing histogram does not reconcile")
  if (invalidElapsed > 0n) invalid("SQLite duration samples unavailable")
  if (errors && integer(timing.errors) > completed) invalid("SQLite timing errors exceed completions")
  if (valid === 0n && elapsed !== 0n) invalid("SQLite elapsed time has no valid observations")
  // Duration-invalid callbacks have no bucket; valid zero-nanosecond callbacks do.
  const minimum = timing.histogram_log2_us.reduce((sum, count, bucket) => sum + BigInt(count) * (bucket === 0 ? 0n : (1n << BigInt(bucket)) * 1000n), 0n)
  if (elapsed < minimum) invalid("SQLite timing below histogram bounds")
  const maximum = integer(phase ? timing.max_elapsed_ns_end : timing.max_elapsed_ns)
  const upper = timing.histogram_log2_us.reduce((sum, count, bucket) => {
    const bucketMaximum = bucket === 31 ? maximum : (1n << BigInt(bucket + 1)) * 1000n - 1n
    return sum + BigInt(count) * (maximum < bucketMaximum ? maximum : bucketMaximum)
  }, 0n)
  if (elapsed > upper) invalid("SQLite elapsed time exceeds histogram and maximum bounds")
  if (!phase) {
    if (maximum > elapsed || (valid === 0n && maximum !== 0n)) invalid("SQLite cumulative maximum inconsistent")
    if (valid > 0n) {
      const top = timing.histogram_log2_us.findLastIndex((count) => count !== "0")
      const lower = top === 0 ? 0n : (1n << BigInt(top)) * 1000n
      const upper = top === 31 ? null : (1n << BigInt(top + 1)) * 1000n
      if (maximum < lower || (upper !== null && maximum >= upper)) invalid("SQLite maximum outside histogram bounds")
    }
  } else {
    const start = integer(timing.max_elapsed_ns_start)
    const end = integer(timing.max_elapsed_ns_end)
    if (end < start) invalid("SQLite cumulative maximum reset")
    if ((valid === 0n && end !== start) || (end > start && end > elapsed)) invalid("SQLite phase maximum inconsistent")
    if (timing.exact_phase_max_ns !== "unavailable") invalid("exact SQLite phase maximum unavailable")
  }
}
function validateSqliteWalState(state, configuration) {
  if (configuration.journal_mode !== "wal") {
    exactFields(state, ["status"], "SQLite WAL state fields unavailable")
    if (state.status !== "not_wal") invalid("SQLite WAL state does not match configuration")
  } else {
    if (state?.status !== "available") invalid("SQLite WAL boundary gauges unavailable")
    exactFields(state, ["status", "log_frames", "checkpointed_frames", "uncheckpointed_frames"], "SQLite WAL state fields unavailable")
    const log = integer(state.log_frames)
    const checkpointed = integer(state.checkpointed_frames)
    if (checkpointed > log || integer(state.uncheckpointed_frames) !== log - checkpointed) invalid("SQLite WAL frame gauges do not reconcile")
  }
}
function validateSqliteConnection(entry) {
  if (!object(entry) || Object.hasOwn(entry, "error")) invalid("SQLite counter unavailable")
  exactFields(entry, ["connection_id", "counter_overflow", "pager", "page_size", "pager_read_bytes_estimate", "pager_write_bytes_estimate", "sql_statements", "sql_categories", "configuration", "sql_profile", "connection_lock", "connection_lock_hold", "provider_begin", "provider_commit", "wal_state", "vfs_observed", ...Object.keys(sqliteScopes)], "SQLite connection fields unavailable")
  if (typeof entry.vfs_observed !== "boolean") invalid("SQLite VFS connection coverage unavailable")
  if (integer(entry.connection_id) === 0n) invalid("SQLite connection identity unavailable")
  if (entry.counter_overflow !== false) invalid("SQLite counter overflow state unavailable")
  exactFields(entry.pager, sqlitePagerFields, "SQLite pager fields unavailable")
  sqlitePagerFields.forEach((field) => integer(entry.pager[field]))
  const pageSize = integer(entry.page_size)
  if (pageSize === 0n) invalid("SQLite page size unavailable")
  for (const [estimate, field] of [["pager_read_bytes_estimate", "cache_misses"], ["pager_write_bytes_estimate", "page_writes"]]) {
    if (integer(entry[estimate]) !== integer(entry.pager[field]) * pageSize) invalid("SQLite pager byte estimate inconsistent")
  }
  if (!object(entry.sql_categories) || Object.keys(entry.sql_categories).some((name) => !sqliteCategories.includes(name))) invalid("SQLite SQL category coverage unavailable")
  if (checkedTotal(Object.values(entry.sql_categories), "SQLite statement counter overflow") !== integer(entry.sql_statements)) invalid("SQLite statement categories do not reconcile")
  exactFields(entry.sql_profile, sqliteCategories, "SQLite SQL profile coverage unavailable")
  sqliteCategories.forEach((name) => validateSqliteTiming(entry.sql_profile[name]))
  validateSqliteTiming(entry.connection_lock, true)
  validateSqliteTiming(entry.connection_lock_hold)
  validateSqliteTiming(entry.provider_begin, true)
  validateSqliteTiming(entry.provider_commit, true)
  for (const [field, scope] of Object.entries(sqliteScopes)) if (entry[field] !== scope) invalid("SQLite measurement scope unavailable")
  validateSqliteConfiguration(entry.configuration)
  validateSqliteWalState(entry.wal_state, entry.configuration)
}
function sqliteConnectionMap(bank) {
  exactFields(bank, ["connections", "sql_statements", "observer_elapsed_ns", "observer_scope", "vfs"], "SQLite connection registry unavailable")
  if (!Array.isArray(bank.connections)) invalid("SQLite connection registry unavailable")
  integer(bank.observer_elapsed_ns)
  if (bank.observer_scope !== sqliteObserverScope) invalid("SQLite observer measurement scope unavailable")
  const map = new Map()
  for (const entry of bank.connections) {
    validateSqliteConnection(entry)
    if (map.has(entry.connection_id)) invalid("duplicate SQLite connection identity")
    map.set(entry.connection_id, entry)
  }
  if (checkedTotal(bank.connections.map((entry) => entry.sql_statements), "SQLite registry counter overflow") !== integer(bank.sql_statements)) invalid("SQLite registry statement total does not reconcile")
  return map
}
function sqliteTimingDelta(before, after, errors = false) {
  const old = before ?? { completed: "0", overflow: false, elapsed_ns: "0", max_elapsed_ns: "0", invalid_elapsed: "0", histogram_log2_us: Array(32).fill("0"), ...(errors ? { errors: "0" } : {}) }
  const delta = { completed: subtract(after.completed, old.completed), overflow: false,
    elapsed_ns: subtract(after.elapsed_ns, old.elapsed_ns), invalid_elapsed: subtract(after.invalid_elapsed, old.invalid_elapsed),
    histogram_log2_us: after.histogram_log2_us.map((count, index) => subtract(count, old.histogram_log2_us[index])),
    max_elapsed_ns_start: old.max_elapsed_ns, max_elapsed_ns_end: after.max_elapsed_ns, exact_phase_max_ns: "unavailable",
    ...(errors ? { errors: subtract(after.errors, old.errors) } : {}) }
  validateSqliteTiming(delta, errors, true)
  return delta
}
function sqliteVfsTiming(row, phase = false) {
  return Object.fromEntries([...sqliteTimingFields.filter((field) => field !== "max_elapsed_ns"), ...(phase ? ["max_elapsed_ns_start", "max_elapsed_ns_end", "exact_phase_max_ns"] : ["max_elapsed_ns"]), "errors"].map((field) => [field, row[field]]))
}
function validateSqliteVfsRows(entries, phase = false) {
  if (!Array.isArray(entries) || entries.length !== SQLITE_VFS_ENTRY_NAMES.length) invalid("SQLite VFS role coverage unavailable")
  for (const [index, row] of entries.entries()) {
    exactFields(row, ["name", ...Object.keys(sqliteVfsTiming(row, phase)), ...sqliteVfsByteFields], "SQLite VFS row fields unavailable")
    if (row.name !== SQLITE_VFS_ENTRY_NAMES[index]) invalid("SQLite VFS role names or order changed")
    validateSqliteTiming(sqliteVfsTiming(row, phase), true, phase)
    const requested = integer(row.requested_bytes)
    const confirmed = integer(row.confirmed_bytes)
    const shortReads = integer(row.short_reads)
    if (confirmed > requested || (integer(row.completed) === integer(row.errors) && confirmed !== 0n)) invalid("SQLite VFS confirmed bytes inconsistent")
    if (integer(row.completed) === 0n && requested !== 0n) invalid("SQLite VFS requested bytes have no calls")
    if (row.name.endsWith(".sync") && (requested !== 0n || confirmed !== 0n)) invalid("SQLite VFS sync byte units changed")
    if (shortReads > integer(row.errors) || (!row.name.endsWith(".read") && shortReads !== 0n)) invalid("SQLite VFS short-read counts inconsistent")
  }
}
function validateSqliteCheckpoint(checkpoint, phase = false) {
  const gaugeFields = phase ? ["active_windows_start", "active_windows_end"] : ["active_windows"]
  exactFields(checkpoint, [...sqliteCheckpointCounterFields, ...gaugeFields, "paired"], "SQLite checkpoint fields unavailable")
  sqliteCheckpointCounterFields.forEach((field) => integer(checkpoint[field]))
  validateSqliteTiming(checkpoint.paired, false, phase)
  const active = phase ? integer(checkpoint.active_windows_end) - integer(checkpoint.active_windows_start) : integer(checkpoint.active_windows)
  if (integer(checkpoint.starts) !== integer(checkpoint.paired.completed) + integer(checkpoint.unmatched_starts) + integer(checkpoint.aborted_windows) + active ||
      integer(checkpoint.dones) !== integer(checkpoint.paired.completed) + integer(checkpoint.unmatched_dones)) invalid("SQLite checkpoint signals do not reconcile")
  if (gaugeFields.some((field) => integer(checkpoint[field]) !== 0n)) invalid("SQLite checkpoint window crossed phase boundary")
  if (["unmatched_starts", "unmatched_dones", "aborted_windows"].some((field) => checkpoint[field] !== "0")) invalid("SQLite checkpoint duration coverage incomplete")
}
function validateSqliteVfsOpenOutcomes(bank) {
  const attempts = integer(bank.open_attempts)
  const errors = integer(bank.open_errors)
  const files = integer(bank.files_opened)
  // Every successful xOpen owns a file; a failed xOpen with non-null methods also owns a closable file.
  if (errors > attempts || files > attempts || attempts - errors > files) invalid("SQLite VFS open outcomes do not reconcile")
}
function validateSqliteVfs(bank) {
  if (!object(bank) || Object.hasOwn(bank, "error")) invalid("SQLite VFS diagnostics unavailable")
  exactFields(bank, [...Object.keys(SQLITE_VFS_MEASUREMENT), "overflow", ...sqliteVfsCounterFields, ...sqliteVfsGaugeFields, "entries", "checkpoint"], "SQLite VFS bank fields unavailable")
  for (const [field, expected] of Object.entries(SQLITE_VFS_MEASUREMENT)) if (bank[field] !== expected) invalid("SQLite VFS measurement metadata unavailable")
  if (bank.overflow !== false) invalid("SQLite VFS overflow state unavailable")
  sqliteVfsCounterFields.forEach((field) => integer(bank[field]))
  sqliteVfsGaugeFields.forEach((field) => integer(bank[field]))
  if (integer(bank.in_flight) !== 0n) invalid("SQLite VFS method crossed phase boundary")
  validateSqliteVfsOpenOutcomes(bank)
  if (integer(bank.close_errors) > integer(bank.close_calls)) invalid("SQLite VFS file outcomes do not reconcile")
  validateSqliteVfsRows(bank.entries)
  validateSqliteCheckpoint(bank.checkpoint)
}
function sqliteVfsDelta(before, after) {
  validateSqliteVfs(before)
  validateSqliteVfs(after)
  const delta = { ...SQLITE_VFS_MEASUREMENT, complete: true, issues: [],
    overflow_start: before.overflow, overflow_end: after.overflow,
    ...Object.fromEntries(sqliteVfsCounterFields.map((field) => [field, subtract(after[field], before[field])])),
    ...Object.fromEntries(sqliteVfsGaugeFields.flatMap((field) => [[`${field}_start`, before[field]], [`${field}_end`, after[field]]])),
    timing_measurement: { duration: "inclusive_wall_nanoseconds; VFS_methods_and_checkpoint_copy_windows_overlap", latency_max: "cumulative_process_bank; exact_phase_max_unavailable", latency_histogram: structuredClone(sqliteTimingMeasurement.latency_histogram) },
    entries: after.entries.map((row, index) => ({ name: SQLITE_VFS_ENTRY_NAMES[index], ...sqliteTimingDelta(sqliteVfsTiming(before.entries[index]), sqliteVfsTiming(row), true),
      ...Object.fromEntries(sqliteVfsByteFields.map((field) => [field, subtract(row[field], before.entries[index][field])])) })),
    checkpoint: { ...Object.fromEntries(sqliteCheckpointCounterFields.map((field) => [field, subtract(after.checkpoint[field], before.checkpoint[field])])),
      active_windows_start: before.checkpoint.active_windows, active_windows_end: after.checkpoint.active_windows,
      paired: sqliteTimingDelta(before.checkpoint.paired, after.checkpoint.paired) },
  }
  validateSqliteVfsOpenOutcomes(delta)
  if (integer(delta.close_errors) > integer(delta.close_calls)) invalid("SQLite VFS phase close outcomes do not reconcile")
  if (integer(delta.registered_vfs_end) < integer(delta.registered_vfs_start)) invalid("SQLite VFS registered wrappers disappeared")
  if (integer(delta.live_files_end) !== integer(delta.live_files_start) + integer(delta.files_opened) - integer(delta.close_calls)) invalid("SQLite VFS live-file gauge does not reconcile")
  validateSqliteVfsRows(delta.entries, true)
  validateSqliteCheckpoint(delta.checkpoint, true)
  return delta
}
function sqliteVfsPhase(before, after) {
  const vfs = sqliteVfsDelta(before?.vfs, after?.vfs)
  if ([before?.connections, after?.connections].some((entries) => !Array.isArray(entries) || entries.some((entry) => !object(entry) || entry.vfs_observed !== true))) {
    vfs.complete = false
    vfs.issues.push("SQLite VFS provider attribution unavailable")
  }
  return vfs
}
function connectionDelta(before, after, vfs) {
  const previous = sqliteConnectionMap(before)
  const current = sqliteConnectionMap(after)
  const missing = [...previous.keys()].filter((id) => !current.has(id))
  const connections = after.connections.map((entry) => {
    const old = previous.get(entry.connection_id)
    if (old && (!isDeepStrictEqual(old.configuration, entry.configuration) || old.page_size !== entry.page_size)) invalid("SQLite connection configuration changed")
    const pager = {}
    for (const label of sqlitePagerFields) pager[label] = subtract(entry.pager[label], old ? old.pager[label] : "0")
    const sql_categories = {}
    for (const label of sqliteCategories.filter((name) => Object.hasOwn(entry.sql_categories, name) || (old && Object.hasOwn(old.sql_categories, name)))) {
      sql_categories[label] = subtract(entry.sql_categories[label] ?? "0", old?.sql_categories[label] ?? "0")
    }
    return { connection_id: entry.connection_id, page_size: entry.page_size, pager,
      vfs_observed_start: old ? old.vfs_observed : "unavailable", vfs_observed_end: entry.vfs_observed,
      pager_read_bytes_estimate: subtract(entry.pager_read_bytes_estimate, old?.pager_read_bytes_estimate),
      pager_write_bytes_estimate: subtract(entry.pager_write_bytes_estimate, old?.pager_write_bytes_estimate),
      sql_statements: subtract(entry.sql_statements, old?.sql_statements), sql_categories,
      counter_overflow_start: old ? old.counter_overflow : "unavailable", counter_overflow_end: entry.counter_overflow,
      configuration: structuredClone(entry.configuration), configuration_start: old ? structuredClone(old.configuration) : "unavailable",
      configuration_unchanged: old ? true : "unavailable", boundary_gauges_complete: Boolean(old),
      ...sqliteScopes,
      sql_profile: Object.fromEntries(sqliteCategories.map((name) => [name, sqliteTimingDelta(old?.sql_profile[name], entry.sql_profile[name])])),
      connection_lock: sqliteTimingDelta(old?.connection_lock, entry.connection_lock, true),
      connection_lock_hold: sqliteTimingDelta(old?.connection_lock_hold, entry.connection_lock_hold),
      provider_begin: sqliteTimingDelta(old?.provider_begin, entry.provider_begin, true),
      provider_commit: sqliteTimingDelta(old?.provider_commit, entry.provider_commit, true),
      wal_state_start: old ? structuredClone(old.wal_state) : { status: "unavailable", reason: "connection_opened_during_phase" },
      wal_state_end: structuredClone(entry.wal_state),
      opened_during_phase: !old }
  })
  return { scope: "process_live_connections", status: previous.size || current.size ? "observed" : "no_live_connections",
    timing_measurement: structuredClone(sqliteTimingMeasurement),
    observer_elapsed_ns_start: before.observer_elapsed_ns, observer_elapsed_ns_end: after.observer_elapsed_ns,
    observer_scope: sqliteObserverScope,
    connection_ids_start: [...previous.keys()], connection_ids_end: [...current.keys()],
    sql_statements: missing.length ? "unavailable" : subtract(after.sql_statements, before.sql_statements),
    connections, missing_connection_ids: missing, vfs, complete: missing.length === 0 && vfs.complete }
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
export function validateLocalPhaseDiagnostics(local, measurement, family = "r2") {
  if (!isDeepStrictEqual(measurement, objectStoreFamily(family).local)) invalid("local phase measurement metadata unavailable")
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
function localDelta(before, after, beforeMeasurement, afterMeasurement, opened, family = "r2") {
  if (!object(after) || !opened && !object(before)) return { status: "unavailable", complete: false, issues: [before === null && after === null ? "local diagnostics disabled" : "local diagnostics unavailable or changed availability"] }
  try {
    const expected = objectStoreFamily(family).local
    if (!isDeepStrictEqual(beforeMeasurement, expected) || !isDeepStrictEqual(afterMeasurement, expected)) invalid("local measurement metadata unavailable")
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
function objectStoreDelta(before, after, beforeLocalMeasurement, afterLocalMeasurement, family = "r2") {
  const { name } = objectStoreFamily(family)
  if (before?.scope !== "process_live_instances" || after?.scope !== before.scope || before.available === false || after.available === false || before.internal_successful_retries !== "unavailable" || after.internal_successful_retries !== "unavailable") invalid(`${name} registry metadata unavailable`)
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
      local_work: localDelta(old?.local_work, entry.local_work, beforeLocalMeasurement, afterLocalMeasurement, !old, family),
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
  const signedCounter = (value) => typeof value === "string" && value.length <= 20 && signedDecimal.test(value) && BigInt(value) >= i64Minimum && BigInt(value) <= i64Maximum ? value : { unavailable: "invalid_signed_integer" }
  const sqliteTiming = (value, errors = false) => ({
    ...Object.fromEntries(["completed", "elapsed_ns", "max_elapsed_ns", "invalid_elapsed", ...(errors ? ["errors"] : [])].map((field) => [field, counter(value?.[field])])),
    overflow: typeof value?.overflow === "boolean" ? value.overflow : "unavailable",
    histogram_log2_us: array(value?.histogram_log2_us, 32, counter),
  })
  const sqliteVfs = (value) => ({
    available: object(value) && !Object.hasOwn(value, "error") && Array.isArray(value.entries),
    ...(object(value) && Object.hasOwn(value, "error") ? { error: "unavailable" } : {}),
    ...Object.fromEntries(Object.entries(SQLITE_VFS_MEASUREMENT).map(([field, expected]) => [field, value?.[field] === expected ? expected : "unavailable"])),
    overflow: typeof value?.overflow === "boolean" ? value.overflow : "unavailable",
    ...Object.fromEntries([...sqliteVfsCounterFields, ...sqliteVfsGaugeFields].map((field) => [field, counter(value?.[field])])),
    entries: array(value?.entries, SQLITE_VFS_ENTRY_NAMES.length, (entry) => ({
      name: SQLITE_VFS_ENTRY_NAMES.includes(entry?.name) ? entry.name : "unrecognized",
      ...sqliteTiming(entry, true), ...Object.fromEntries(sqliteVfsByteFields.map((field) => [field, counter(entry?.[field])])),
    })),
    checkpoint: { ...Object.fromEntries(sqliteCheckpointCounterFields.map((field) => [field, counter(value?.checkpoint?.[field])])),
      active_windows: counter(value?.checkpoint?.active_windows), paired: sqliteTiming(value?.checkpoint?.paired) },
  })
  const sqlite = (value) => ({
    available: object(value) && !Object.hasOwn(value, "error") && Array.isArray(value.connections),
    sql_statements: counter(value?.sql_statements), observer_elapsed_ns: counter(value?.observer_elapsed_ns),
    observer_scope: value?.observer_scope === sqliteObserverScope ? sqliteObserverScope : "unavailable",
    vfs: sqliteVfs(value?.vfs),
    connections: array(value?.connections, 32, (entry) => ({
      connection_id: counter(entry?.connection_id), counter_overflow: typeof entry?.counter_overflow === "boolean" ? entry.counter_overflow : "unavailable",
      vfs_observed: typeof entry?.vfs_observed === "boolean" ? entry.vfs_observed : "unavailable",
      ...(entry && Object.hasOwn(entry, "error") ? { error: "unavailable" } : {}),
      pager: Object.fromEntries(sqlitePagerFields.map((field) => [field, counter(entry?.pager?.[field])])),
      ...Object.fromEntries(["page_size", "pager_read_bytes_estimate", "pager_write_bytes_estimate", "sql_statements"].map((field) => [field, counter(entry?.[field])])),
      sql_categories: Object.fromEntries(sqliteCategories.filter((name) => Object.hasOwn(entry?.sql_categories ?? {}, name)).map((name) => [name, counter(entry.sql_categories[name])])),
      sql_profile: Object.fromEntries(sqliteCategories.map((name) => [name, sqliteTiming(entry?.sql_profile?.[name])])),
      ...Object.fromEntries(["connection_lock", "connection_lock_hold", "provider_begin", "provider_commit"].map((field) => [field, sqliteTiming(entry?.[field], field !== "connection_lock_hold")])),
      ...Object.fromEntries(Object.entries(sqliteScopes).map(([field, scope]) => [field, entry?.[field] === scope ? scope : "unavailable"])),
      configuration: {
        journal_mode: sqliteJournalModes.includes(entry?.configuration?.journal_mode) ? entry.configuration.journal_mode : "unavailable",
        locking_mode: sqliteLockingModes.includes(entry?.configuration?.locking_mode) ? entry.configuration.locking_mode : "unavailable",
        is_autocommit: typeof entry?.configuration?.is_autocommit === "boolean" ? entry.configuration.is_autocommit : "unavailable",
        ...Object.fromEntries(sqliteConfigurationFields.slice(3).map((field) => [field, signedCounter(entry?.configuration?.[field])])),
      },
      wal_state: { status: ["not_wal", "available", "busy", "unavailable"].includes(entry?.wal_state?.status) ? entry.wal_state.status : "unavailable",
        ...Object.fromEntries(["log_frames", "checkpointed_frames", "uncheckpointed_frames"].filter((field) => Object.hasOwn(entry?.wal_state ?? {}, field)).map((field) => [field, counter(entry.wal_state[field])])) },
    })),
  })
  const registry = (value) => ({ scope: value?.scope === "process_live_instances" ? "process_live_instances" : "unavailable",
    available: value?.available !== false && Array.isArray(value?.instances),
    instances: array(value?.instances, 32, (entry) => ({
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
    })) })
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
        storage_instrumented_operations: validInstrumentedOperations(value.measurement) || "unavailable",
        tidb_coverage: isDeepStrictEqual(value.measurement?.tidb_coverage, TIDB_DIAGNOSTIC_COVERAGE) ? structuredClone(TIDB_DIAGNOSTIC_COVERAGE) : "unavailable",
        foundationdb_coverage: foundationdbCoverage(value.measurement?.foundationdb_coverage) ? structuredClone(foundationdbCoverage(value.measurement.foundationdb_coverage)) : "unavailable",
        r2_api: isDeepStrictEqual(value.measurement?.r2_api, rawMeasurement) ? structuredClone(rawMeasurement) : "unavailable",
        r2_local: isDeepStrictEqual(value.measurement?.r2_local, OBJECT_STORE_LOCAL_MEASUREMENT) ? structuredClone(OBJECT_STORE_LOCAL_MEASUREMENT) : "unavailable",
        ...(Object.hasOwn(value, "rustfs") ? {
          rustfs_api: isDeepStrictEqual(value.measurement?.rustfs_api, RUSTFS_API_MEASUREMENT) ? structuredClone(RUSTFS_API_MEASUREMENT) : "unavailable",
          rustfs_local: isDeepStrictEqual(value.measurement?.rustfs_local, RUSTFS_LOCAL_MEASUREMENT) ? structuredClone(RUSTFS_LOCAL_MEASUREMENT) : "unavailable",
        } : {}),
        latency_histogram: isDeepStrictEqual(value.measurement?.latency_histogram, { unit: "microseconds", intervals: histogramIntervals }) ? { unit: "microseconds", intervals: histogramIntervals } : "unavailable" },
      storage: { in_flight: counter(value.storage?.in_flight), entries: rows(value.storage?.entries, storageNames, ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "in_flight", "elapsed_ns"]) },
      sqlite: sqlite(value.sqlite),
      r2: registry(value.r2),
      ...(Object.hasOwn(value, "rustfs") ? { rustfs: registry(value.rustfs) } : {}),
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
    if (delta.sqlite?.vfs) {
      // Global file callbacks outlive the live connection registry. Preserve
      // their independently validated delta without certifying the whole phase.
      if (delta.sqlite.missing_connection_ids?.length) delta.sqlite = { complete: false, vfs: delta.sqlite.vfs }
      else delta.sqlite.complete = false
    } else delete delta.sqlite
  }
  return delta
}

export function deltaNativeSnapshots(before, after) {
  let vfs
  try {
    if (before.schema_version !== NATIVE_DIAGNOSTICS_SCHEMA || after.schema_version !== before.schema_version || before.enabled !== true || after.enabled !== true) invalid("diagnostic version or enabled state changed")
    if (before.scope !== "process" || after.scope !== before.scope || before.quiescent_snapshot_required !== true || after.quiescent_snapshot_required !== true || before.elapsed_semantics !== "inclusive_wall_nanoseconds" || after.elapsed_semantics !== before.elapsed_semantics) invalid("native diagnostic scope changed")
    const withoutLocal = (measurement) => object(measurement) ? Object.fromEntries(Object.entries(measurement).filter(([name]) => !["r2_local", "rustfs_local"].includes(name))) : measurement
    if (!before.measurement || !isDeepStrictEqual(withoutLocal(before.measurement), withoutLocal(after.measurement)) ||
        !isDeepStrictEqual(after.measurement.latency_histogram, { unit: "microseconds", intervals: histogramIntervals }) ||
        after.measurement.storage_calls !== STORAGE_CALL_SEMANTICS ||
        after.measurement.storage_bytes !== STORAGE_BYTE_SEMANTICS ||
        after.measurement.storage_rows !== STORAGE_ROW_SEMANTICS ||
        !isDeepStrictEqual(after.measurement.storage_operations, storageNames) ||
        !isDeepStrictEqual(after.measurement.storage_families, STORAGE_OPERATION_FAMILIES) ||
        !validInstrumentedOperations(after.measurement) ||
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
    const hasRustfs = Object.hasOwn(before, "rustfs") || Object.hasOwn(after, "rustfs")
    if (hasRustfs && (after.measurement.rustfs !== "live_store_logical_calls_and_cache_hits; not_http_attempts" || !isDeepStrictEqual(after.measurement.rustfs_api, RUSTFS_API_MEASUREMENT))) invalid("RustFS measurement metadata changed")
    // Derive the process bank once, after trusted metadata and before unrelated bank validation.
    vfs = sqliteVfsPhase(before.sqlite, after.sqlite)
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
    const sqlite = connectionDelta(before.sqlite, after.sqlite, vfs)
    const r2 = objectStoreDelta(before.r2, after.r2, before.measurement.r2_local, after.measurement.r2_local)
    let rustfs
    if (hasRustfs) {
      rustfs = objectStoreDelta(before.rustfs, after.rustfs, before.measurement.rustfs_local, after.measurement.rustfs_local, "rustfs")
    }
    const issues = []
    if (typeof storage.in_flight_start !== "string" || typeof storage.in_flight_end !== "string" ||
        !decimal.test(storage.in_flight_start) || !decimal.test(storage.in_flight_end)) invalid("invalid in-flight gauge")
    if (BigInt(storage.in_flight_start) !== 0n || BigInt(storage.in_flight_end) !== 0n) issues.push("instrumented storage operation crossed phase boundary")
    if (storage.entries.some((entry) => integer(entry.in_flight_start) !== 0n || integer(entry.in_flight_end) !== 0n)) issues.push("instrumented storage row crossed phase boundary")
    if (sqlite.missing_connection_ids.length) issues.push("SQLite connection closed during phase")
    issues.push(...sqlite.vfs.issues)
    if (!r2.complete) issues.push("R2 instance closed during phase")
    if (rustfs && !rustfs.complete) issues.push("RustFS instance closed during phase")
    const result = { schema_version: NATIVE_DIAGNOSTICS_SCHEMA, complete: issues.length === 0, issues,
      measurement: { ...Object.fromEntries(["storage_calls", "storage_bytes", "storage_rows", "storage_operations", "storage_families", "storage_instrumented_operations", "tidb_coverage", "storage_duration", "latency_histogram", "forwarding_boxes", "profile", "sqlite", "r2", "r2_api", "unavailable"].map((field) => [field, after.measurement[field]])),
        foundationdb_coverage: structuredClone(foundationdbCoverage(after.measurement.foundationdb_coverage)),
        r2_local: isDeepStrictEqual(after.measurement.r2_local, OBJECT_STORE_LOCAL_MEASUREMENT) ? structuredClone(OBJECT_STORE_LOCAL_MEASUREMENT) : "unavailable",
        ...(rustfs ? { rustfs: after.measurement.rustfs, rustfs_api: structuredClone(RUSTFS_API_MEASUREMENT),
          rustfs_local: isDeepStrictEqual(after.measurement.rustfs_local, RUSTFS_LOCAL_MEASUREMENT) ? structuredClone(RUSTFS_LOCAL_MEASUREMENT) : "unavailable" } : {}) },
      backend_waits: after.backend_waits, http_attempts: after.http_attempts,
      physical_device_iops: after.physical_device_iops, storage, profile, sqlite, r2, ...(rustfs ? { rustfs } : {}) }
    return retainIncompleteEvidence(result, before, after)
  } catch (error) { return { complete: false, issues: [error instanceof DiagnosticValidationError ? error.message : "invalid native diagnostic shape", ...(vfs?.issues ?? [])],
    ...(vfs ? { sqlite: { complete: false, vfs } } : {}), observations: sanitizedObservations(before, after) } }
}

// The artifact gate validates processed evidence independently of complete=true.
export function validateRawPhaseDiagnostics(phase, family = "r2") {
  const { name, api } = objectStoreFamily(family)
  if (!object(phase) || phase.quiescent !== true || !object(phase.native)) invalid("workload diagnostics unavailable or not quiescent")
  const native = phase.native
  if (family === "rustfs" && native.measurement?.rustfs !== "live_store_logical_calls_and_cache_hits; not_http_attempts") invalid("workload RustFS logical measurement metadata changed")
  if (native.schema_version !== NATIVE_DIAGNOSTICS_SCHEMA || native.complete !== true || !Array.isArray(native.issues) || native.issues.length !== 0) invalid("workload native diagnostics incomplete")
  if (integer(native.storage?.in_flight_start) !== 0n || integer(native.storage?.in_flight_end) !== 0n) invalid("workload global storage operation pending")
  validateStorageRows(native.storage?.entries, true)
  if (native.measurement?.storage_calls !== STORAGE_CALL_SEMANTICS || native.measurement?.storage_bytes !== STORAGE_BYTE_SEMANTICS || native.measurement?.storage_rows !== STORAGE_ROW_SEMANTICS ||
      !isDeepStrictEqual(native.measurement?.storage_operations, storageNames) || !isDeepStrictEqual(native.measurement?.storage_families, STORAGE_OPERATION_FAMILIES) ||
      !validInstrumentedOperations(native.measurement) ||
      !isDeepStrictEqual(native.measurement?.tidb_coverage, TIDB_DIAGNOSTIC_COVERAGE)) invalid("workload storage measurement metadata changed")
  if (!isDeepStrictEqual(native.measurement?.[`${family}_api`], api) || !isDeepStrictEqual(native.measurement?.latency_histogram, { unit: "microseconds", intervals: histogramIntervals })) invalid("workload raw measurement metadata changed")
  const r2 = native[family]
  if (!object(r2) || r2.scope !== "process_live_instances" || r2.complete !== true || r2.internal_successful_retries !== "unavailable" || !Array.isArray(r2.missing_instance_ids) || r2.missing_instance_ids.length !== 0) invalid(`workload ${name} registry incomplete`)
  const instances = instanceMap(r2.instances)
  if (instances.size === 0) invalid(`workload ${name} registry empty`)
  const expectedIds = [...instances.keys()]
  for (const ids of [r2.instance_ids_start, r2.instance_ids_end]) {
    if (!Array.isArray(ids) || ids.length !== instances.size || new Set(ids).size !== ids.length || !ids.every((id) => expectedIds.includes(id))) invalid(`workload ${name} identities changed`)
    ids.forEach((id) => integer(id, true))
  }
  for (const instance of instances.values()) {
    if (instance.opened_during_phase !== false) invalid(`workload ${name} instance opened during phase`)
    for (const field of logicalR2Fields) integer(instance[field])
    const raw = instance.raw_api
    if (!object(raw) || raw.schema !== rawSchema || raw.scope !== rawScope || raw.saturated_start !== false || raw.saturated_end !== false) invalid("workload raw API schema or saturation unavailable")
    for (const field of ["in_flight_start", "in_flight_end", "pending_claims_start", "pending_claims_end"]) if (integer(raw[field], true) !== 0n) invalid("workload raw operation or claim pending")
    validateClaims(raw.claims)
    validateRawRows(raw.entries, true)
  }
  return expectedIds
}

export const PROCESS_RESOURCE_MEASUREMENT = Object.freeze({
  schema: "mount-rs.process-resources.v1",
  filesystem_operations: "process_getrusage_block_accounting; not_syscalls_or_device_iops",
  page_faults: "process_getrusage_minor_and_major_faults",
  memory: "endpoint_gauges; signed_delta_not_allocation_churn",
  peak_rss: "process_lifetime_high_water_mark; not_phase_peak",
})
const processMemoryFields = ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"]
const processResourceRows = {
  minor_page_faults: "minorPageFault", major_page_faults: "majorPageFault",
  filesystem_input_operations: "fsRead", filesystem_output_operations: "fsWrite",
}
function sampledInteger(value) {
  return Number.isSafeInteger(value) && value >= 0 ? BigInt(value) : undefined
}
function sampledDelta(before, after) {
  const start = sampledInteger(before), end = sampledInteger(after)
  return start !== undefined && end !== undefined && end >= start ? end - start : undefined
}
function sampledMemory(source) {
  return Object.fromEntries(processMemoryFields.map((field) => [field, sampledInteger(source?.[field])?.toString() ?? "unavailable"]))
}
function processResources(before, after) {
  const samePlatform = typeof before.platform === "string" && before.platform === after.platform
  // libuv's Windows fault and I/O fields do not have these POSIX meanings.
  const supported = samePlatform && ["linux", "darwin"].includes(before.platform)
  const resources_work = {}, resources_observer = {}
  for (const [field, input] of Object.entries(processResourceRows)) {
    const work = supported ? sampledDelta(before.resourcesEnd?.[input], after.resourcesStart?.[input]) : undefined
    const first = supported ? sampledDelta(before.resourcesStart?.[input], before.resourcesEnd?.[input]) : undefined
    const last = supported ? sampledDelta(after.resourcesStart?.[input], after.resourcesEnd?.[input]) : undefined
    resources_work[field] = work?.toString() ?? "unavailable"
    resources_observer[field] = first !== undefined && last !== undefined ? (first + last).toString() : "unavailable"
  }
  const memory_start_bytes = sampledMemory(before.memory), memory_end_bytes = sampledMemory(after.memory)
  const memory_delta_bytes = Object.fromEntries(processMemoryFields.map((field) => {
    const start = sampledInteger(before.memory?.[field]), end = sampledInteger(after.memory?.[field])
    return [field, start !== undefined && end !== undefined ? (end - start).toString() : "unavailable"]
  }))
  const peakStart = sampledInteger(before.resourcesEnd?.maxRSS), peakEnd = sampledInteger(after.resourcesStart?.maxRSS)
  return { resources_work, resources_observer, memory_start_bytes, memory_end_bytes, memory_delta_bytes,
    lifetime_peak_rss_bytes: { start: peakStart !== undefined ? (peakStart * 1024n).toString() : "unavailable",
      end: samePlatform && peakStart !== undefined && peakEnd !== undefined && peakEnd >= peakStart ? (peakEnd * 1024n).toString() : "unavailable",
      scope: PROCESS_RESOURCE_MEASUREMENT.peak_rss },
    resource_measurement: { ...PROCESS_RESOURCE_MEASUREMENT } }
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
  return { started, ended: samplers.now(), cpuStart, cpuEnd, resourcesStart, resourcesEnd, memory, native, platform: samplers.platform ?? process.platform }
}

export function finishPhase(name, before, after, quiescent = true) {
  const delta = before.native && after.native ? deltaNativeSnapshots(before.native, after.native) : { complete: false, issues: ["native diagnostics unavailable"] }
  if (!quiescent) { delta.complete = false; delta.issues.push("native operations crossed phase boundary") }
  for (const family of ["r2", "rustfs"]) {
    if (name.startsWith("workload-") && delta[family]?.instances.some((instance) => instance.opened_during_phase)) {
      delta.complete = false
      delta.issues.push(`${objectStoreFamily(family).name} instance opened during workload phase`)
    }
  }
  retainIncompleteEvidence(delta, before.native, after.native)
  const cpu = { user_us: String(after.cpuStart.user - before.cpuEnd.user), system_us: String(after.cpuStart.system - before.cpuEnd.system) }
  const observerCpu = { user_us: String((before.cpuEnd.user - before.cpuStart.user) + (after.cpuEnd.user - after.cpuStart.user)), system_us: String((before.cpuEnd.system - before.cpuStart.system) + (after.cpuEnd.system - after.cpuStart.system)) }
  const resources = { voluntary_context_switches: String(after.resourcesStart.voluntaryContextSwitches - before.resourcesEnd.voluntaryContextSwitches), involuntary_context_switches: String(after.resourcesStart.involuntaryContextSwitches - before.resourcesEnd.involuntaryContextSwitches) }
  const observations = processResources(before, after)
  return { name, elapsed_ms: after.started - before.ended, observer_snapshot_ms: (before.ended - before.started) + (after.ended - after.started), quiescent, native: delta, process: { cpu_work: cpu, cpu_observer: observerCpu, ...observations, resources_work: { ...resources, ...observations.resources_work }, native_allocation_count: "unavailable", js_allocation_count: "unavailable" } }
}

export function logPhaseSummary(phase) {
  const entries = phase.native.storage?.entries
  const available = Array.isArray(entries)
  const instrumented = validInstrumentedOperations(phase.native.measurement) || []
  const families = Object.fromEntries(Object.entries(STORAGE_OPERATION_FAMILIES).map(([name, semantics]) => {
    const observed = semantics.operations.filter((operation) => instrumented.includes(operation))
    const covered = observed.length === semantics.operations.length
    const coverage = { instrumented: covered, instrumented_operations: observed, available: available && covered }
    if (!coverage.available) return [name, coverage]
    const rows = entries.filter((entry) => semantics.operations.includes(entry.name))
    return [name, { ...coverage, ...Object.fromEntries(["calls", "bytes", "error", "cancelled", "elapsed_ns", "returned_rows", "returned_row_observations"].map((field) => [field, rows.reduce((sum, entry) => sum + BigInt(entry[field]), 0n).toString()])) }]
  }))
  const localSummary = (family) => {
    const instances = Array.isArray(phase.native[family]?.instances) ? phase.native[family].instances : []
    const observedLocal = instances.flatMap((instance) => {
      try { return [validateLocalPhaseDiagnostics(instance.local_work, phase.native.measurement?.[`${family}_local`], family)] } catch { return [] }
    })
    const summary = { status: observedLocal.length ? observedLocal.length === instances.length ? "observed" : "partial" : "unavailable",
      observed_instances: observedLocal.length, unavailable_instances: instances.length - observedLocal.length,
      scope: "inclusive_local_wall_time; concurrent_spans_overlap; copy_bytes_are_not_network_bytes; not_cpu_or_device_iops" }
    if (observedLocal.length) summary.entries = OBJECT_STORE_LOCAL_NAMES.map((name, index) => ({ name,
      ...Object.fromEntries(localFields.map((field) => [field, observedLocal.reduce((sum, local) => sum + BigInt(local.entries[index][field]), 0n).toString()])) }))
    return summary
  }
  process.stderr.write(`MOUNT_RS_STORAGE_PHASE ${JSON.stringify({ name: phase.name, complete: phase.native.complete, elapsed_ms: phase.elapsed_ms, families,
    local_work: localSummary("r2"), ...(Object.hasOwn(phase.native, "rustfs") ? { rustfs_local_work: localSummary("rustfs") } : {}),
    scope: "inclusive_instrumented_operations; families_overlap; bytes_only_known_payload; rows_only_known_observations; not_application_or_device_iops" })}\n`)
}
