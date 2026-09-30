#!/usr/bin/env python3
"""Run fixed Unix cache/remote failure gates with owned process containment.

Set CARGO_TARGET_DIR to a dedicated cache and, for Redis gates,
MOUNT_RS_CACHE_REDIS_SERVER to an explicit absolute executable path.
Only receipt/log/source-pin files are evidence; fixture contents are private.
Postterminal group signals provide containment, not fixture graceful-cleanup proof.
"""
import hashlib,json,os,pathlib,re,selectors,signal,stat,subprocess,sys,tempfile,time,shutil
BASE=pathlib.Path(__file__).resolve().parent.parent
COMMANDS={'cachetests': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--all-targets', '--locked', '--offline'], 180, 0, 0), 'redisfault': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--ignored', '--exact', 'redis_directory_real_peer_failures_preserve_exact_backing', '--test-threads=1', '--nocapture'], 180, 0, 0), 'rediscleanup': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--ignored', '--exact', 'cancelled_redis_fixture_reaps_owned_child_before_removing_directory', '--test-threads=1', '--nocapture'], 180, 0, 0), 'wsloss': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--test', 'quic_mount', '--locked', '--offline', '--', '--ignored', '--exact', 'websocket_sqlite_commit_survives_lost_wire_reply_without_replay', '--test-threads=1', '--nocapture'], 180, 0, 0), 'remotetests': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--all-targets', '--locked', '--offline'], 180, 0, 0), 'faultclippy': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-blob-cache', '-p', 'mount-rs-remote-client', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0), 'fmt': (['./scripts/cargo-shared', 'fmt', '--all', '--', '--check'], 120, 0, 0)}
COMMANDS.update({
    'wscompactloss': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--test', 'quic_mount', '--locked', '--offline', '--', '--ignored', '--exact', 'automatic_fallback_compact_sqlite_commit_survives_lost_wire_reply_without_replay', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'wsheldmonitor': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--test', 'quic_mount', '--locked', '--offline', '--', 'quic_mount_reply_loss::held_reply_monitor_', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'targetlazy': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '--features', 'sdk-runtime,resource-profiling', '--test', 'quic_production_target', '--locked', '--offline', '--', '--ignored', '--exact', 'ten_process_lazy_startup_preserves_exact_backing_and_workload', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'targetlazyunit': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '--features', 'sdk-runtime,resource-profiling', '--test', 'quic_production_target', '--locked', '--offline', 'lazy_target_', '--', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'balancedprobe': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '--features', 'sdk-runtime,resource-profiling', '--lib', '--locked', '--offline', '--', '--exact', 'dispatch::lazy_handle_tests::authenticated_cold_route_probe_requires_binding_and_definition_without_activation', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'lazycli': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--features', 'local-oidc-fixture,io-profiling', '--lib', '--locked', '--offline', '::lazy_', '--', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'lazyservice': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '--features', 'sdk-runtime,io-profiling', '--test', 'filesystem_runtime', '--test', 'lazy_dispatch', '--test', 'runtime_pool_sqlite', '--locked', '--offline', '--', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'clientsetupmetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--lib', '--locked', '--offline', '--', '--ignored', '--exact', 'connection::metrics_tests::quic_connection_setup_metrics_preserve_outcomes', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'discoverymetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'discovery_metrics', '--locked', '--offline', '--', '--ignored', '--exact', 'discovery_locate_metrics_preserve_bytes_outcomes_and_cancellation', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'wsautoloss': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--test', 'quic_mount', '--locked', '--offline', '--', '--ignored', '--exact', 'automatic_fallback_sqlite_commit_survives_lost_wire_reply_without_replay', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'wsservicedefault': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '--test', 'websocket_diagnostics', '--locked', '--offline', '--', '--test-threads=1'], 180, 0, 0),
    'wsserviceon': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '--features', 'io-profiling', '--test', 'websocket_diagnostics', '--locked', '--offline', '--', '--test-threads=1'], 180, 1, 0),
    'wsserviceunit': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '--features', 'io-profiling', '--lib', '--locked', '--offline', 'server::diagnostics::tests::', '--', '--test-threads=1'], 180, 1, 0),
    'wspackages': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '-p', 'mount-rs-remote-client', '-p', 'mount-rs-cli', '--all-targets', '--locked', '--offline'], 180, 0, 0),
    'wspackagesprofiled': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-service', '-p', 'mount-rs-remote-client', '-p', 'mount-rs-cli', '--features', 'mount-rs-service/io-profiling,mount-rs-cli/io-profiling', '--all-targets', '--locked', '--offline'], 180, 0, 0),
    'wsclippy': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-core', '-p', 'mount-rs-service', '-p', 'mount-rs-remote-client', '-p', 'mount-rs-cli', '-p', 'mount-rs-napi', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0),
    'wsclippyprofiled': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-service', '-p', 'mount-rs-remote-client', '-p', 'mount-rs-cli', '--features', 'mount-rs-service/io-profiling,mount-rs-cli/io-profiling', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0),
    'wsclientmetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--lib', '--locked', '--offline', '--', '--ignored', '--exact', 'websocket::metrics_tests::websocket_stages_preserve_serialized_transactions', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'sqlitecache': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--ignored', '--exact', 'sqlite_backed_authenticated_cache_failures_preserve_exact_backing_savings', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'peeriometricstrace': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--lib', '--locked', '--offline', '--', '--ignored', '--exact', 'peer::tests::peer_request_stage_metrics_preserve_bytes_outcomes_and_cancellation', '--test-threads=1', '--nocapture'], 180, 1, 1),
    'peeriometrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--lib', '--locked', '--offline', '--', '--ignored', '--exact', 'peer::tests::peer_request_stage_metrics_preserve_bytes_outcomes_and_cancellation', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'filesystemmetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-chunked', '--test', 'filesystem_causal_metrics', '--locked', '--offline', '--', '--ignored', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'clientmetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-remote-client', '--lib', '--locked', '--offline', '--', '--ignored', '--exact', 'connection::metrics_tests::quic_stream_acquisition_outcomes_preserve_transactions', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'clidiagnostics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--features', 'io-profiling', '--lib', '--locked', '--offline', 'remote::diagnostics::tests::'], 180, 1, 0),
    'clicompact': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--features', 'local-oidc-fixture,io-profiling', '--test', 'configured_remote_compact', '--locked', '--offline', '--', '--exact', 'configured_binary_selects_mrc5_for_signed_quic_and_websocket_reopen', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'napimetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-napi', '--lib', '--locked', '--offline'], 180, 1, 0),
    'consumerclippy': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-cli', '-p', 'mount-rs-napi', '--features', 'mount-rs-cli/local-oidc-fixture,mount-rs-cli/io-profiling', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0),
    'cachemetricstrace': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--ignored', '--exact', 'cache_lookup_stage_metrics_preserve_bytes_and_cancellation', '--test-threads=1', '--nocapture'], 180, 1, 1),
    'cacheconsumerred': (['fnm', 'exec', '--using', 'v24.18.0', 'node', '--test', '--test-name-pattern=old 85-row observation|cache hit-byte events|causal exact old134', 'benchmarks/storage/foundationdb-diagnostics.test.mjs', 'benchmarks/storage/owned-layout-metrics.test.mjs', 'scripts/verify-owned-backing-pilot.test.mjs'], 180, 0, 0),
    'cachemetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--ignored', '--exact', 'cache_lookup_stage_metrics_preserve_bytes_and_cancellation', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'peermetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--lib', '--locked', '--offline', '--', '--ignored', '--exact', 'peer::tests::peer_connection_stage_metrics_preserve_bytes_and_cancellation', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'peerreconnect': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--ignored', '--exact', 'peer_read_reconnect_bypasses_pending_replica_handshake', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'peerport': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--ignored', '--exact', 'stopped_peer_udp_rebind_waits_for_real_driver_release', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'quinnclose': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'quinn_close', '--locked', '--offline', '--', '--exact', 'tests::first_close_initial_drains_without_waiting_for_idle_timeout', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'quinnordinary': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'quinn_close', '--locked', '--offline', '--', '--exact', 'tests::ordinary_initial_then_close_drains_without_waiting_for_idle_timeout', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'quinnruntimeclose': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'quinn_close', '--locked', '--offline', '--', '--ignored', '--exact', 'tests::first_close_initial_releases_real_endpoint_within_close_grace', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'createprep': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-chunked', '--test', 'current_path_create_rebase', '--locked', '--offline', '--', '--ignored', '--exact', 'missing_compact_create_preparation_avoids_full_scan_with_128_siblings', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'createpath': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-chunked', '--test', 'current_path_create_rebase', '--locked', '--offline', '--', '--ignored', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'createguard': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-chunked', '--test', 'compact_snapshot_revision', '--locked', '--offline', '--', '--ignored', '--exact', 'unchanged_compact_refresh_preserves_prepared_create', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'createunit': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-chunked', '--lib', '--locked', '--offline', 'compact_preparation_tests::', '--', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'chunkedtests': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-chunked', '-p', 'mount-rs-sqlite', '--all-targets', '--locked', '--offline'], 180, 0, 0),
    'chunkedclippy': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-chunked', '-p', 'mount-rs-sqlite', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0),
    'cacheprofileoff': (['./scripts/cargo-shared', 'run', '--locked', '--offline', '-p', 'mount-rs-blob-cache', '--example', 'cache_profile'], 180, 0, 0),
    'cacheprofileon': (['./scripts/cargo-shared', 'run', '--locked', '--offline', '-p', 'mount-rs-blob-cache', '--example', 'cache_profile'], 180, 1, 0),
    'storagealloc': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-core', '--test', 'storage_diagnostics_allocations', '--locked', '--offline', '--', '--ignored', '--exact', 'warmed_core_spans_record_without_added_allocations', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'corealloc': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-core', '--test', 'filesystem_causal_profile_allocations', '--locked', '--offline', '--', '--ignored', '--exact', 'warmed_causal_profile_rows_record_without_added_allocations', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'coremetrics': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-core', '--lib', '--locked', '--offline', 'diagnostics::'], 180, 1, 0),
    'cachemetricsclippy': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-core', '-p', 'mount-rs-blob-cache', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0),
    'cacheconsumers': (['fnm', 'exec', '--using', 'v24.18.0', 'node', '--test', 'benchmarks/storage/test.mjs', 'benchmarks/storage/foundationdb-diagnostics.test.mjs', 'benchmarks/storage/owned-layout-metrics.test.mjs', 'benchmarks/storage/owned-backing-pilot.test.mjs', 'scripts/verify-owned-backing-pilot.test.mjs'], 180, 0, 0),
})
COLD_HOLDER_CASES = {
    'holderleasepositive': 'acknowledged_actual_peer_fixture_cleanup_releases_serving_lease_and_cache_owner',
    'holderleasecancelled': 'cancelled_actual_peer_fixture_cleanup_retains_serving_lease_and_cache_owner',
    'holderleaseexpired': 'expired_actual_peer_fixture_cleanup_retains_serving_lease_and_cache_owner',
    'holderfirstseal': 'server_cache::cold_holder::tests::first_close_request_seals_holders_before_held_failed_factory_cleanup',
    'holderroutepublish': 'server_cache::cold_holder::tests::route_poison_during_final_catalog_check_blocks_new_proof_publication',
    'holderroutecoalesce': 'server_cache::cold_holder::tests::route_poison_during_final_catalog_check_blocks_coalesced_proof_reuse',
    'holderdrainpoison': 'server_cache::cold_holder::tests::first_drain_ack_rejects_registry_poison_at_terminal_publication',
    'holderdraintyped': 'server_cache::cold_holder::tests::terminal_publication_poison_preserves_an_existing_typed_drain_error',
    'holderreleaseregistry': 'server_cache::cold_holder::tests::proof_release_refuses_registry_poison_after_final_owner_query',
    'holderreleaseroute': 'server_cache::cold_holder::tests::proof_release_refuses_route_poison_before_final_proof_take',
}
COMMANDS.update({
    'holdersdk': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-sdk', '--all-targets', '--locked', '--offline'], 180, 0, 0),
    'holdercli': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--lib', '--locked', '--offline'], 180, 0, 0),
    'holdercliprofiled': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--features', 'io-profiling', '--lib', '--locked', '--offline'], 180, 1, 0),
    'holderclippy': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-sdk', '-p', 'mount-rs-blob-cache', '-p', 'mount-rs-cli', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0),
    'holderclippyprofiled': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-sdk', '-p', 'mount-rs-blob-cache', '-p', 'mount-rs-cli', '--features', 'mount-rs-cli/io-profiling,mount-rs-cli/local-oidc-fixture', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 1, 0),
    'tidbcoldcompile': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--features', 'local-oidc-fixture', '--test', 'ten_process_cache', '--no-run', '--locked', '--offline'], 180, 0, 0),
    'tidbcoldunit': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--features', 'local-oidc-fixture', '--test', 'ten_process_cache', '--locked', '--offline'], 180, 0, 0),
    'objectstorered': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-core', '--lib', '--locked', '--offline', '--', '--exact', 'diagnostics::object_store::tests::headers_and_body_completion_are_separate_observations', '--test-threads=1'], 180, 0, 0),
    'objectstorecachered': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-object-store-blocks', '--lib', '--locked', '--offline', '--', '--exact', 'object_store_cache_diagnostics_tests::actual_cache_owner_releases_payload_without_observer_retaining_it', '--test-threads=1'], 180, 0, 0),
    'rustfshttpred': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-rustfs', '--lib', '--locked', '--offline', '--', '--exact', 'http_observation_tests::actual_s3_retry_counts_two_dispatches_with_original_signing_and_create_headers', '--test-threads=1'], 180, 0, 0),
    'objectstoreunit': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-core', '--lib', '--locked', '--offline', 'diagnostics::object_store::tests::', '--', '--test-threads=1'], 180, 0, 0),
    'objectstorecacheunit': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-object-store-blocks', '--lib', '--locked', '--offline', 'object_store_cache_diagnostics_tests::', '--', '--test-threads=1'], 180, 0, 0),
    'rustfshttpunit': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-rustfs', '--lib', '--locked', '--offline', 'http_observation_tests::', '--', '--test-threads=1'], 180, 0, 0),
    'rustfsownedprefix': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-rustfs', '--lib', '--locked', '--offline', 'owned_prefix_tests::', '--', '--test-threads=1'], 180, 0, 0),
    'objectstorealloc': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-core', '--test', 'storage_diagnostics_allocations', '--locked', '--offline', '--', '--exact', 'warmed_object_store_guards_and_fixed_snapshots_do_not_add_allocations', '--test-threads=1'], 180, 0, 0),
    'objectstorequalification': (['./scripts/cargo-shared', 'test', '-p', 'mount-rs-object-store-blocks', '--lib', '--locked', '--offline', '--', '--ignored', '--exact', 'object_store_cache_diagnostics_tests::actual_qualification_temporary_adapters_release_both_observed_caches', '--test-threads=1'], 180, 1, 0),
    'objectstoreclippy': (['./scripts/cargo-shared', 'clippy', '-p', 'mount-rs-core', '-p', 'mount-rs-object-store-blocks', '-p', 'mount-rs-rustfs', '--all-targets', '--locked', '--offline', '--', '-D', 'warnings'], 180, 0, 0),
})
for kind, case in COLD_HOLDER_CASES.items():
    if case.startswith('server_cache::cold_holder::tests::'):
        command = ['./scripts/cargo-shared', 'test', '-p', 'mount-rs-cli', '--lib', '--locked', '--offline', '--', '--exact', case, '--test-threads=1', '--nocapture']
    else:
        command = ['./scripts/cargo-shared', 'test', '-p', 'mount-rs-blob-cache', '--test', 'distributed_failure', '--locked', '--offline', '--', '--exact', case, '--test-threads=1', '--nocapture']
        if kind != 'holderleasepositive':
            command.append('--ignored')
    COMMANDS[kind] = (command, 180, 0, 0)

EXACT_CASES = {
    'objectstorered': 'diagnostics::object_store::tests::headers_and_body_completion_are_separate_observations',
    'objectstorecachered': 'object_store_cache_diagnostics_tests::actual_cache_owner_releases_payload_without_observer_retaining_it',
    'rustfshttpred': 'http_observation_tests::actual_s3_retry_counts_two_dispatches_with_original_signing_and_create_headers',
    'objectstorealloc': 'warmed_object_store_guards_and_fixed_snapshots_do_not_add_allocations',
    'objectstorequalification': 'object_store_cache_diagnostics_tests::actual_qualification_temporary_adapters_release_both_observed_caches',
    'balancedprobe': 'dispatch::lazy_handle_tests::authenticated_cold_route_probe_requires_binding_and_definition_without_activation',
    'wscompactloss': 'automatic_fallback_compact_sqlite_commit_survives_lost_wire_reply_without_replay',
    'targetlazy': 'ten_process_lazy_startup_preserves_exact_backing_and_workload',
    'clientsetupmetrics': 'connection::metrics_tests::quic_connection_setup_metrics_preserve_outcomes',
    'discoverymetrics': 'discovery_locate_metrics_preserve_bytes_outcomes_and_cancellation',
    'wsautoloss': 'automatic_fallback_sqlite_commit_survives_lost_wire_reply_without_replay',
    'wsclientmetrics': 'websocket::metrics_tests::websocket_stages_preserve_serialized_transactions',
    'sqlitecache': 'sqlite_backed_authenticated_cache_failures_preserve_exact_backing_savings',
    'peeriometricstrace': 'peer::tests::peer_request_stage_metrics_preserve_bytes_outcomes_and_cancellation',
    'peeriometrics': 'peer::tests::peer_request_stage_metrics_preserve_bytes_outcomes_and_cancellation',
    'clientmetrics': 'connection::metrics_tests::quic_stream_acquisition_outcomes_preserve_transactions',
    'peerport': 'stopped_peer_udp_rebind_waits_for_real_driver_release',
    'clicompact': 'configured_binary_selects_mrc5_for_signed_quic_and_websocket_reopen',
    'cachemetricstrace': 'cache_lookup_stage_metrics_preserve_bytes_and_cancellation',
    'redisfault': 'redis_directory_real_peer_failures_preserve_exact_backing',
    'rediscleanup': 'cancelled_redis_fixture_reaps_owned_child_before_removing_directory',
    'wsloss': 'websocket_sqlite_commit_survives_lost_wire_reply_without_replay',
    'cachemetrics': 'cache_lookup_stage_metrics_preserve_bytes_and_cancellation',
    'peermetrics': 'peer::tests::peer_connection_stage_metrics_preserve_bytes_and_cancellation',
    'peerreconnect': 'peer_read_reconnect_bypasses_pending_replica_handshake',
    'quinnclose': 'tests::first_close_initial_drains_without_waiting_for_idle_timeout',
    'quinnordinary': 'tests::ordinary_initial_then_close_drains_without_waiting_for_idle_timeout',
    'quinnruntimeclose': 'tests::first_close_initial_releases_real_endpoint_within_close_grace',
    'createprep': 'missing_compact_create_preparation_avoids_full_scan_with_128_siblings',
    'createguard': 'unchanged_compact_refresh_preserves_prepared_create',
    'storagealloc': 'warmed_core_spans_record_without_added_allocations',
    'corealloc': 'warmed_causal_profile_rows_record_without_added_allocations',
}
EXACT_CASES.update(COLD_HOLDER_CASES)
EXPECTED_PACKAGE_CASES = {
    'holdersdk': tuple('providers::provider_construction_tests::' + name for name in (
        'observed_inspection_cancelled_metadata_constructor_retains_uncertainty',
        'observed_inspection_cancelled_block_constructor_retains_uncertainty',
        'observed_inspection_metadata_error_closes_actual_wire_owner',
        'observed_inspection_block_error_closes_actual_wire_owner',
        'observed_inspection_retained_operation_survives_waiter_timeout',
    )) + tuple('providers::compact_layout_inspection_tests::' + name for name in (
        'observed_compact_inspection_preserves_same_and_split_sqlite_state',
        'observed_compact_inspection_does_not_enroll_a_noncompact_layout',
        'observed_compact_inspection_rejects_foreign_backing_without_repair',
        'observed_compact_inspection_closed_context_rejects_before_registration',
        'observed_inspection_keeps_primary_failure_separate_from_cleanup_failure',
        'observed_inspection_owned_operation_survives_a_cancelled_cleanup_waiter',
    )) + ('retained_unobserved_split_resource_rejects_failed_authority',),
    'holdercli': tuple('server_cache::cold_holder::tests::' + name for name in (
        'cold_holder_admission_uses_configured_scope_and_survives_active_store_eviction',
        'cancelled_waiter_retains_actual_task_and_coalesces_without_new_provider_work',
        'revoked_old_await_cannot_publish_after_exact_catalog_restore',
        'actual_registry_drain_precedes_context_close_even_when_close_waiter_is_cancelled',
    )),
}
EXPECTED_PACKAGE_CASES['holdercliprofiled'] = EXPECTED_PACKAGE_CASES['holdercli']
EXPECTED_PACKAGE_CASES['tidbcoldunit'] = tuple('ten_process_cache_support::cold_retirement::tests::' + name for name in (
    'cold_holder_shutdown_frame_requires_complete_unique_real_contract',
    'cold_oracle_owner_unwind_retains_unproven_actual_resource_group',
    'cold_oracle_owner_releases_only_positive_groups_and_retains_poison',
)) + tuple('ten_process_cache_support::object_store_projection::tests::' + name for name in (
    'cli_complete_codec_preserves_external_generation_hashes_and_u64',
    'cli_near_maximum_counter_keeps_a_one_unit_delta_exact',
    'cli_periodic_and_shutdown_histories_keep_identical_sequences_separate',
    'cli_single_complete_sample_has_no_window',
    'cli_missing_duplicate_or_malformed_frames_cannot_be_salvaged',
    'cli_foreign_pid_generation_or_context_is_rejected',
    'cli_repeated_or_regressed_capture_identity_is_rejected',
    'cli_counter_reset_is_rejected_before_any_window_publication',
    'cli_gauge_decreases_and_monotonic_maxima_keep_endpoint_values',
    'cli_saturation_refuses_windows_but_concurrent_and_cache_quality_are_retained',
    'cli_existing_output_caps_utf8_and_terminal_lf_remain_required',
    'cli_missing_frames_and_incomplete_owner_never_export_zero',
    'worker_disabled_capture_skips_identity_snapshot_sequence_and_writes',
    'worker_exhausted_capture_cannot_reuse_identity_or_write',
    'worker_actual_isolated_bank_captures_bind_output_owner_and_window',
    'worker_mixed_capture_pid_context_generation_or_order_is_rejected',
    'worker_reset_and_saturation_refuse_windows_but_quality_is_retained',
    'worker_reserved_sequence_rejects_callback_identity_drift_before_snapshot_or_write',
    'worker_client_build_windows_preserve_deltas_gauges_maxima_and_reject_resets',
)) + tuple('ten_process_cache_support::progress_trace::tests::' + name for name in (
    'native_progress_disabled_performs_no_output_or_clock_reads_and_requires_exact_opt_in',
    'native_progress_monotonic_pair_preserves_numeric_identity_and_physical_bounds',
    'native_progress_cap_is_atomic_bounded_and_never_reset',
    'native_progress_clock_and_io_failures_never_fake_complete_interval',
    'native_progress_each_pending_and_ready_poll_has_its_own_interval',
)) + tuple('ten_process_cache_support::process::tests::' + name for name in (
    'frame_validation_same_fresh_sequence_then_stale_is_retained_before_rss',
    'frame_validation_final_recompose_retains_loaded_frame_and_first_failure',
    'frame_validation_max_scalars_are_bounded_without_private_strings',
    'frame_validation_malformed_json_has_no_numeric_evidence_or_rss_queries',
    'frame_validation_other_contract_failure_has_categorical_evidence',
    'frame_validation_final_rss_error_does_not_fabricate_frame_failure',
))
EXPECTED_PACKAGE_CASES.update({
    'rustfsownedprefix': tuple('owned_prefix_tests::' + name for name in (
        'empty_observation_uses_one_exact_bounded_signed_list_seam',
        'any_owned_child_including_reserved_and_nested_markers_is_present',
        'sibling_locations_and_incomplete_page_shapes_fail_closed',
        'unsafe_or_oversized_prefixes_are_rejected_before_dispatch',
        'configured_public_method_checks_scope_before_building_a_client',
        'deadline_drops_a_held_listing_once_without_retry',
        'caller_cancellation_drops_the_exact_held_listing',
    )),
    'objectstoreunit': tuple('diagnostics::object_store::tests::' + name for name in (
        'headers_and_body_completion_are_separate_observations',
        'dispatch_errors_pre_header_cancellation_and_body_drop_are_distinct',
        'actual_service_lifetime_can_outlive_bundle_group',
        'failed_and_abandoned_builds_never_commit_residency',
        'enabled_guards_read_clock_and_disabled_guards_do_not',
        'cache_unknown_and_final_release_remove_only_own_contribution',
        'fixed_bank_saturates_and_snapshot_discloses_active_update',
        'client_build_success_error_and_abandonment_are_separate_and_role_bound',
        'client_build_consuming_terminals_are_counted_once_after_drop',
        'client_build_disabled_does_not_read_clock_or_allocate_bank',
        'client_build_saturation_and_inflight_preserve_quality',
    )),
    'objectstorecacheunit': tuple('object_store_cache_diagnostics_tests::' + name for name in (
        'actual_cache_owner_releases_payload_without_observer_retaining_it',
        'cache_replacement_removal_and_lru_keep_actual_residency',
        'byte_cap_and_oversize_rejection_preserve_existing_policy',
        'poisoned_cache_is_unknown_once_and_keeps_existing_bypass',
        'disabled_cache_observation_keeps_cache_behavior',
        'actual_store_clone_shares_cache_until_final_facade_drop',
    )),
    'rustfshttpunit': tuple('http_observation_tests::' + name for name in (
        'actual_s3_retry_counts_two_dispatches_with_original_signing_and_create_headers',
        'unpolled_observed_call_counts_box_construction_without_dispatch_or_cancellation',
        'held_actual_dispatch_drop_preserves_owner_drop_and_records_only_preheader_cancel',
        'body_frames_trailers_pointer_sizehint_and_eof_pass_through_without_copy',
        'typed_transport_and_body_errors_remain_distinct_and_body_prefix_is_counted',
        'pending_response_body_keeps_no_observed_service_and_drop_is_not_dispatch_cancel',
        'initially_empty_head_hint_does_not_infer_eof_and_actual_none_is_positive_control',
        'configured_four_clients_roles_budgets_sharing_and_final_bundle_release_are_preserved',
        'actual_fourth_client_build_failure_releases_prior_clients_and_never_commits_bundle',
        'invalid_prefix_after_four_actual_client_builds_records_bundle_error_and_releases_all',
        'disabled_connector_returns_original_service_without_wrapper_or_observation',
        'allocation_control::pending_call_adds_exactly_one_box_and_disabled_call_preserves_baseline',
        'allocation_control::ready_prebuilt_response_adds_one_future_box_and_one_body_box',
        'connector_build_success_and_typed_error_are_observed_without_http_dispatch',
        'connector_build_panic_unwind_is_abandoned_without_constructed_client_or_dispatch',
        'connector_build_disabled_preserves_success_error_and_options_without_observation',
    )),
})
EXPECTED_SUITES = {
    'wsheldmonitor': (
        'quic_mount_reply_loss::held_reply_monitor_rejects_second_write_even_when_release_is_ready',
        'quic_mount_reply_loss::held_reply_monitor_flushes_ping_and_accepts_pong_before_release',
    ),
    'targetlazyunit': (
        'target::progress::tests::lazy_target_balanced_validation_phases_preserve_closed_progress_accounting',
        'target::process::lazy_target_terminal_requires_the_same_cold_plan_as_ready',
        'target::lazy_runtime::tests::lazy_target_prepared_registration_preserves_full_bytes_and_backing_across_generation_reopen',
        'target::lazy_runtime::tests::lazy_target_cancelled_waiter_retains_real_lease_and_rejoins_same_acknowledged_drain',
        'target::lazy_runtime::tests::lazy_target_backing_mismatch_preserves_typed_error_without_retry_or_context_close',
        'target::lazy_runtime::tests::lazy_target_no_runtime_retains_unknown_context_even_when_injected_keeper_is_dropped',
        'target::lazy_runtime::tests::lazy_target_listener_deadline_starts_one_storage_allowance_without_replacement_or_context_ack',
        'target::lazy_runtime::tests::lazy_target_late_context_handoff_is_rerooted_and_cannot_reuse_cached_success',
        'target::lazy_runtime::tests::lazy_target_keeper_poison_refuses_terminal_ack_and_retains_context',
        'target::backend::receipt_pooling_tests::lazy_target_options_resolve_once_and_preserve_exact_drive_keys',
        'target::backend::receipt_pooling_tests::lazy_target_sqlite_plans_preserve_shared_metadata_blocks_without_env_reads',
        'target::metrics::tests::lazy_target_runtime_shape_preserves_cold_and_actual_backing_distinction',
        'target::metrics::tests::lazy_target_runtime_incomplete_frames_cannot_qualify_or_invent_zero',
        'target::metrics::tests::lazy_target_runtime_delta_keeps_gauges_and_rejects_cross_generation',
        'target::metrics::tests::lazy_target_runtime_boundary_requires_actual_primary_and_all_route_activations',
        'target::metrics::tests::lazy_target_timed_modes_accept_only_balanced_assigned_runtime_owners',
        'target::metrics::tests::lazy_target_timed_modes_reject_unassigned_crossnode_runtime_replication',
        'target::metrics::tests::lazy_target_nonactivating_route_checks_require_zero_opened_owners',
        'target::metrics::tests::lazy_target_postprofile_rotation_requires_only_its_exact_assigned_owners',
        'target::metrics::tests::lazy_target_closed_measured_generation_rejects_unassigned_replication',
    ),
    'lazycli': tuple('remote::tests::'+name for name in (
        'lazy_actual_listener_bind_failures_close_created_resources_without_opening_drives',
        'lazy_cancelled_actual_ready_service_drains_both_retained_listeners_with_cold_providers',
        'lazy_invalid_drive_plan_preserves_cold_providers_and_reports_prepared_routes',
        'lazy_zero_active_drive_capacity_fails_before_opening_any_provider',
    )) + tuple('remote_runtime::lifecycle_tests::'+name for name in (
        'lazy_aborted_worker_retains_both_installed_consuming_futures_and_terminal_failure',
        'lazy_cancelled_close_waiter_joins_one_actual_sdk_drain_before_context_close',
        'lazy_constructor_and_cleanup_failure_retain_context_and_slot_without_retry',
        'lazy_dropping_serving_scope_keeps_actual_pinned_owner_until_one_drain_finishes',
        'lazy_prepared_cli_plans_preserve_persistent_bytes_and_backing_through_capacity_one_reopen',
    )) + tuple('runtime::construction_tests::'+name for name in (
        'lazy_explicit_nonfilesystem_refusal_has_fixed_service_mapping_and_unchanged_cli_rendering',
        'lazy_service_constructor_preserves_typed_provider_error_without_text_parsing',
        'lazy_virtual_root_stat_and_chown_preserve_typed_filesystem_codes_at_service_boundary',
    )),
    'lazyservice': (
        'actual_filesystem_owner_handoff_requires_acknowledged_close_before_next_generation',
        'actual_shutdown::cancelled_then_failed_actual_sdk_shutdown_never_authorizes_a_replacement',
        'canceling_failure_cleanup_waiters_does_not_cancel_or_repeat_its_owned_cleanup',
        'cleanup_panic_preserves_primary_error_and_retains_registered_resource',
        'factory_retains_before_first_poll_and_abandoned_open_is_nonretryable',
        'owner_drop_without_shutdown_never_invents_a_close_receipt',
        'panicking_constructor_retains_its_registered_owner_and_no_retry',
        'primary_constructor_error_survives_authority_cleanup_failure_and_close_rejoin',
        'cancelled_cold_request_keeps_open_owner_without_quarantining_uninvoked_backend',
        'cold_open_rechecks_expiry_before_any_backend_operation',
        'cold_open_rechecks_same_revision_grant_policy_definition_and_permission',
        'expiry_during_post_open_catalog_load_never_reaches_backend',
        'invalid_handle_never_activates_a_cold_drive',
        'lazy_registration_and_denied_scopes_invoke_no_factory',
        'quarantined_generation_during_post_open_authorization_never_reaches_backend',
        'sdk_activation::actual_sdk_activation_rechecks_same_revision_catalog_before_selected_driver_use',
        'sdk_activation::canceled_request_keeps_actual_sdk_activation_owned_without_backend_replay',
        'actual_durable_mrc4_backing_does_not_qualify_for_runtime_eviction',
        'actual_mrc5_bytes_eof_and_backing_survive_repeated_owned_eviction',
        'actual_mrc5_cloned_request_lease_prevents_eviction_without_opening_target',
        'actual_mrc5_failed_publication_quarantines_owner_without_replacement_generation',
        'actual_mrc5_handle_pin_keeps_mutations_and_sparse_eof_through_reopen',
        'shared_sdk_factory_owns_actual_mrc5_reopen_generations_and_context_sibling',
    ),
    'wsservicedefault': ('ordinary_websocket_constructor_has_no_observer', 'explicit_websocket_observer_requires_profiling_feature'),
    'wsserviceon': (
        'ordinary_websocket_constructor_has_no_observer',
        'explicit_websocket_observer_requires_profiling_feature',
        'enabled::held_hello_authentication_and_idle_socket_have_distinct_lifetimes',
        'enabled::held_renewal_is_a_request_and_changed_identity_is_still_rejected',
        'enabled::cancelled_dispatch_settles_before_actual_cleanup_finishes',
        'enabled::successful_binary_write_and_held_read_record_actual_dispatch_results',
        'enabled::denied_dispatch_is_an_error_even_when_response_submission_succeeds',
        'enabled::incomplete_request_body_keeps_the_existing_deadline_and_records_timeout',
    ),
    'wsserviceunit': tuple('server::diagnostics::tests::'+name for name in (
        'auth_stage_scope_helpers_allocate_no_heap_with_preconstructed_observer',
        'websocket_inventory_and_application_snapshot_are_transport_specific',
        'websocket_span_and_auth_scope_helpers_add_no_heap_allocations',
        'websocket_authentication_helper_scopes_catalog_auth_for_hello_and_renewal',
        'websocket_signed_catalog_hello_and_renewal_record_verification_and_grant_stages',
        'websocket_hello_auth_timeout_cancels_catalog_stage_without_changing_deadline',
        'auth_stage_context_restores_across_nested_and_interleaved_polls',
        'auth_stage_failed_fetch_records_error_before_negative_cache_reuse',
        'auth_stage_pending_fetch_and_mutex_wait_retire_on_cancellation',
        'auth_stage_outer_timeout_is_distinct_from_inner_cancellation',
        'entering_mutation_between_end_envelope_reads_cannot_claim_quiescence',
        'spans_hold_activity_until_terminal_and_drop_records_cancellation',
        'saturation_and_mutating_capture_cannot_claim_complete_quiescence',
        'slow_records_are_bounded_and_have_only_fixed_labels',
        'transport_fold_saturates_instead_of_wrapping',
    )),
    'filesystemmetrics': (
        'partial_overwrite_attributes_initial_fallback_and_retry_puts',
        'dropped_pending_put_records_attempted_bytes_and_cancellation',
        'failed_put_preserves_error_and_zero_successful_input_bytes',
        'coalesced_conflicting_batch_separates_candidate_replay_from_sent_replies',
        'canceled_queued_request_is_skipped_and_not_counted_as_committed',
        'canceled_metadata_waiter_has_no_hold_while_publication_owns_gate',
        'manual_clock_lease_recovery_is_observed_under_an_acquired_gate',
        'changed_chunker_replay_attributes_only_actual_reprepared_puts',
        'empty_and_all_zero_whole_files_dispatch_no_block_puts',
        'cancelled_acquired_fallback_records_hold_and_block_rewrite_phase',
        'dropped_follower_after_known_commit_retains_commit_and_closed_reply',
        'sqlite_compact_selected_gates_and_phases_preserve_payload_after_reopen',
    ),
    'createunit': tuple('compact_preparation_tests::'+name for name in (
        'point_receipt_cannot_admit_after_local_revision_advances',
        'missing_preparation_waits_for_full_publication_and_releases_gate_before_blocks',
        'canceled_missing_preparation_retains_no_pending_full_capture',
        'occupied_preparation_uses_full_current_selected_body',
        'damaged_traversed_or_unrelated_guard_refuses_create_without_publication',
        'targeted_create_preserves_selected_physical_body_pairs_and_pending_atime',
        'targeted_create_mutates_unique_namespace_and_preserves_genuine_pinned_reader',
        'legacy_stale_fresh_create_reuses_prepared_blocks_and_one_publication',
        'legacy_stale_fresh_create_occupied_path_retains_conflict_fallback',
    )),
    'createpath': (
        'missing_compact_create_preparation_avoids_full_scan_with_128_siblings',
        'peer_allocation_rebases_prepared_create',
        'retargeted_symlink_rebases_into_current_parent',
        'occupied_current_path_keeps_guard_conflict_and_replay',
        'removed_current_parent_is_rejected',
    ),
    'clidiagnostics': tuple('remote::diagnostics::tests::'+name for name in (
        'typed_process_banks_preserve_registry_scopes_and_raw_u64',
        'disabled_process_banks_do_not_capture_or_export_zero_snapshots',
        'only_exact_profile_one_selects_a_compiled_service_observer',
        'generic_encoding_preserves_raw_u64_above_javascript_integer_precision',
        'websocket_record_preserves_its_application_schema_and_raw_u64',
        'full_current_banks_with_maximum_u64_fit_existing_record_limit',
        'oversized_serializable_value_emits_only_a_bounded_incomplete_record',
        'serialization_failure_uses_a_fixed_incomplete_record',
        'periodic_interval_is_explicit_bounded_and_ignored_without_profiling',
        'periodic_interval_rejects_non_unicode_only_when_selected',
        'disabled_periodic_wait_needs_no_timer_driver_or_capture',
        'periodic_wait_prefers_ready_stop_and_owns_callback_until_drop',
        'periodic_wait_delays_first_capture_and_returns_original_stop_result',
        'periodic_encoding_adds_capture_metadata_and_preserves_shutdown_v2',
        'periodic_encoding_failure_is_bounded_without_losing_capture_identity',
        'disabled_object_store_sideband_never_samples_or_exports_zero_rows',
        'object_store_sideband_uses_exact_periodic_identity_and_one_real_snapshot',
        'object_store_sideband_keeps_max_u64_and_legacy_schema_separate',
        'actual_periodic_router_binds_one_sample_to_zero_one_or_two_listeners',
        'shutdown_object_store_identity_is_lazy_unique_and_exhaustion_closed',
    )),
}

# Fixed export gates: all require one complete named harness, including singleton cases.
OBJECT_STORE_EXPORT_SUITES = {
    'exportcodecred': ('object_store_diagnostics::tests::one_real_bank_capture_is_bound_to_all_seven_frames_before_later_changes',),
    'exportcodecunit': ('object_store_diagnostics::tests::one_real_bank_capture_is_bound_to_all_seven_frames_before_later_changes', 'object_store_diagnostics::tests::disabled_and_unavailable_capture_publish_no_records_and_no_measured_zero', 'object_store_diagnostics::tests::every_maximum_u64_field_survives_all_seven_bounded_records', 'object_store_diagnostics::tests::complete_indexed_set_can_arrive_out_of_order', 'object_store_diagnostics::tests::decoder_rejects_missing_duplicate_conflicting_and_cross_capture_frames', 'object_store_diagnostics::tests::decoder_requires_exact_typed_fixed_rows_without_numeric_coercion', 'object_store_diagnostics::tests::bounded_serializer_never_publishes_partial_or_caller_error_text', 'object_store_diagnostics::tests::sink_failure_leaves_an_unacceptable_partial_set', 'object_store_diagnostics::tests::zero_sequence_and_generation_remain_exact_startup_identity', 'object_store_diagnostics::tests::valid_json_at_frame_limit_is_accepted_and_one_byte_over_is_rejected', 'object_store_diagnostics::tests::complete_client_build_rows_round_trip_maximum_u64_and_reject_malformed_rows'),
    'exportclired': ('remote::diagnostics::tests::actual_periodic_router_binds_one_sample_to_zero_one_or_two_listeners',),
    'exportcliunit': ('remote::diagnostics::tests::disabled_object_store_sideband_never_samples_or_exports_zero_rows', 'remote::diagnostics::tests::object_store_sideband_uses_exact_periodic_identity_and_one_real_snapshot', 'remote::diagnostics::tests::object_store_sideband_keeps_max_u64_and_legacy_schema_separate', 'remote::diagnostics::tests::shutdown_object_store_identity_is_lazy_unique_and_exhaustion_closed'),
    'exportsdkred': ('target::metrics::tests::object_store_actual_local_capture_encloses_once_and_binds_worker_and_controller',),
    'exportsdkunit': ('target::metrics::tests::object_store_actual_local_capture_encloses_once_and_binds_worker_and_controller', 'target::metrics::tests::object_store_disabled_skips_callback_and_unavailable_does_not_export_zero', 'target::metrics::tests::object_store_boundary_identity_checks_pid_and_preserves_zero_sequence_generation', 'target::metrics::tests::object_store_records_own_exact_u64_snapshot_and_reject_cross_boundary_merge', 'target::metrics::tests::object_store_additive_receipt_does_not_enter_existing_phase_deltas', 'target::metrics::tests::object_store_public_local_capture_uses_actual_process_bank_or_disabled_status'),
    'exportsdkpublicon': ('target::metrics::tests::object_store_public_local_capture_uses_actual_process_bank_or_disabled_status',),
    'exportsdkpublicoff': ('target::metrics::tests::object_store_public_local_capture_uses_actual_process_bank_or_disabled_status',),
}
COMMANDS.update({
    'exportcodecred': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-service', '--lib', '--', 'object_store_diagnostics::tests::one_real_bank_capture_is_bound_to_all_seven_frames_before_later_changes', '--exact', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'exportcodecunit': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-service', '--lib', '--', 'object_store_diagnostics::tests::', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'exportclired': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-cli', '--lib', '--', 'remote::diagnostics::tests::actual_periodic_router_binds_one_sample_to_zero_one_or_two_listeners', '--exact', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'exportcliunit': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-cli', '--lib', '--', 'object_store_', '--test-threads=1', '--nocapture'], 180, 0, 0),
    'exportsdkred': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-service', '--features', 'sdk-runtime,resource-profiling', '--test', 'quic_production_target', '--', 'target::metrics::tests::object_store_actual_local_capture_encloses_once_and_binds_worker_and_controller', '--exact', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'exportsdkunit': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-service', '--features', 'sdk-runtime,resource-profiling', '--test', 'quic_production_target', '--', 'target::metrics::tests::object_store_', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'exportsdkpublicon': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-service', '--features', 'sdk-runtime,resource-profiling', '--test', 'quic_production_target', '--', 'target::metrics::tests::object_store_public_local_capture_uses_actual_process_bank_or_disabled_status', '--exact', '--test-threads=1', '--nocapture'], 180, 1, 0),
    'exportsdkpublicoff': (['./scripts/cargo-shared', 'test', '--locked', '--offline', '-p', 'mount-rs-service', '--features', 'sdk-runtime,resource-profiling', '--test', 'quic_production_target', '--', 'target::metrics::tests::object_store_public_local_capture_uses_actual_process_bank_or_disabled_status', '--exact', '--test-threads=1', '--nocapture'], 180, 0, 0),
})
EXPECTED_SUITES.update(OBJECT_STORE_EXPORT_SUITES)
EXACT_CASES.update({
    'exportcodecred': 'object_store_diagnostics::tests::one_real_bank_capture_is_bound_to_all_seven_frames_before_later_changes',
    'exportclired': 'remote::diagnostics::tests::actual_periodic_router_binds_one_sample_to_zero_one_or_two_listeners',
    'exportsdkred': 'target::metrics::tests::object_store_actual_local_capture_encloses_once_and_binds_worker_and_controller',
    'exportsdkpublicon': 'target::metrics::tests::object_store_public_local_capture_uses_actual_process_bank_or_disabled_status',
    'exportsdkpublicoff': 'target::metrics::tests::object_store_public_local_capture_uses_actual_process_bank_or_disabled_status',
})

def named_suite_passed(output, names, nocapture=False):
    count=len(names)
    return bool(
        re.findall(r'^running ([0-9]+) tests?$', output, re.M) == [str(count)]
        and len(re.findall(r'^test result:', output, re.M)) == 1
        and re.search(r'^test result: ok\. '+str(count)+r' passed; 0 failed; 0 ignored;[^\n]*\n+\Z', output, re.M)
        and (
            (lambda observed: len(observed) == count and set(observed) == set(names))(
                re.findall(r'^test ([^\s]+) \.\.\.', output, re.M)
            ) if nocapture else
            all(re.search(r'^test '+re.escape(name)+r' \.\.\. ok$', output, re.M) for name in names)
        )
    )


def lazy_suite_passed(output, kind):
    names = EXPECTED_SUITES[kind]
    if kind == 'lazycli':
        return named_suite_passed(output, names, nocapture=True)
    # One fixed invocation runs exactly these three Cargo-managed binaries.
    # Require each complete named suite, not an aggregate or one passing bin.
    groups = (names[:8], names[8:17], names[17:])
    blocks = re.split(r'(?=^running [0-9]+ tests$)', output, flags=re.M)[1:]
    return len(blocks) == 3 and all(
        named_suite_passed(block, group, nocapture=True)
        for block, group in zip(blocks, groups)
    )


def cache_slow_logging_records(output):
    names = {
        'blob_cache.miss.admission_wait', 'blob_cache.miss.singleflight_wait',
        'blob_cache.ram.lookup', 'blob_cache.disk.lookup',
        'blob_cache.peer.connection_lock_wait', 'blob_cache.peer.connection_establish',
        'blob_cache.peer.request_byte_admission_wait', 'blob_cache.peer.open_bi',
        'blob_cache.peer.request_send', 'blob_cache.peer.response_receive',
        'blob_cache.peer.get', 'blob_cache.peer.get_miss',
    }
    records = []
    # Only complete LF-terminated frames from writeln can qualify a record.
    lines = output.split('\n')
    for index, line in enumerate(lines):
        if not line.startswith('MOUNT_RS_STORAGE_SLOW'):
            continue
        if index == len(lines)-1:
            return None
        match = re.fullmatch(r'MOUNT_RS_STORAGE_SLOW operation=([^ ]+) outcome=(success|error|cancelled) elapsed_us=([0-9]{1,20})', line)
        if match is None or match[1] not in names or len((line+'\n').encode()) > 512:
            return None
        elapsed = int(match[3])
        if not 100000 <= elapsed <= (1 << 64)-1:
            return None
        records.append({'operation': match[1], 'outcome': match[2], 'elapsed_us': elapsed})
    if not 1 <= len(records) <= 16:
        return None
    # Earlier slow stages may legitimately consume the shared record budget.
    return records


def peer_slow_logging_records(output):
    records = cache_slow_logging_records(output)
    if records is None:
        return None
    observed = {(row['operation'], row['outcome']) for row in records}
    required = {('blob_cache.peer.request_byte_admission_wait', 'cancelled'),
                ('blob_cache.peer.get', 'error')}
    return records if required <= observed else None


def slow_logging_file_records(path, peer=False):
    # Preserve physical frame bytes; text-mode universal newlines alter CR/CRLF.
    output = path.read_bytes().decode('utf-8', errors='replace')
    return peer_slow_logging_records(output) if peer else cache_slow_logging_records(output)


SQLITE_REPLY_LOSS_MARKER = b'MOUNT_RS_SQLITE_REPLY_LOSS'
SQLITE_REPLY_LOSS_FIXED = {
    'schema_version': 2,
    'initial_quic_responses': 0,
    'initial_quic_settlement_quiet_ms': 100,
    'protocol_version': 2,
    'client_hellos': 1,
    'denied_partition_hellos': 1,
    'denied_untrusted_tls': 1,
    'drive_permission_denials': 2,
    'write_submissions': 1,
    'completed_write_replies': 1,
    'held_reply': 1,
    'preclose_metadata_checks': 1,
    'preclose_verified_bytes': 65673,
    'preclose_eof': 0,
    'suppressed_response_envelopes': 1,
    # binary::HEADER_BYTES is 32; WriteResult has no body, followed by one
    # empty WebSocket message terminating this complete response envelope.
    'suppressed_response_bytes': 32,
    'completed_write_count': 65673,
    'suppressed_response_messages': 2,
    'downstream_write_response_messages': 0,
    'uncertain_results': 1,
    'fail_closed_followups': 3,
    'replayed_write_submissions': 0,
    'reconnects': 0,
    'quic_datagrams': 0,
    'quic_observation_scope': 'after_initial_settlement_through_post_loss_quiet',
    'no_contact_window_ms': 100,
    'oracle_metadata_checks': 1,
    'oracle_verified_bytes': 65673,
    'oracle_size_bytes': 65673,
    'oracle_eof_count': 0,
    'request_cleanup_completed': 1,
    'relay_cleanup_completed': 1,
    'oracle_cleanup_completed': 1,
    'server_close_wait_completed': 1,
    'process_cleanup_observed': 0,
    'directory_retained': 1,
    'directory_removed': 0,
}
SQLITE_REPLY_LOSS_FIELDS = frozenset(SQLITE_REPLY_LOSS_FIXED) | frozenset((
    'connection_selection', 'initial_quic_datagrams',
    'websocket_contact_elapsed_us', 'deferred_credential_issues',
))


def sqlite_reply_loss_record(stderr_bytes, kind):
    """Validate one complete raw stderr fixture receipt for its selected gate."""
    if kind not in ('wsloss', 'wsautoloss') or type(stderr_bytes) is not bytes:
        return None
    frames = []
    lines = stderr_bytes.split(b'\n')
    for index, line in enumerate(lines):
        if SQLITE_REPLY_LOSS_MARKER not in line:
            continue
        # Preserve LF framing: CRLF, embedded prefixes and unterminated
        # records cannot become valid through text-mode normalization.
        if (index == len(lines)-1 or b'\r' in line or len(line)+1 > 4096
                or not line.startswith(SQLITE_REPLY_LOSS_MARKER+b' ')):
            return None
        frames.append(line[len(SQLITE_REPLY_LOSS_MARKER)+1:])
    if len(frames) != 1:
        return None
    payload = frames[0]
    if not payload.startswith(b'{') or not payload.endswith(b'}'):
        return None

    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate_sqlite_reply_loss_key')
            result[key] = value
        return result

    def non_integer_number(_):
        raise ValueError('non_integer_sqlite_reply_loss_number')

    try:
        record = json.loads(payload.decode('ascii'), object_pairs_hook=unique_object,
                            parse_float=non_integer_number, parse_constant=non_integer_number)
    except (UnicodeDecodeError, ValueError, RecursionError):
        return None
    if type(record) is not dict or set(record) != SQLITE_REPLY_LOSS_FIELDS:
        return None
    for key, expected in SQLITE_REPLY_LOSS_FIXED.items():
        if type(record[key]) is not type(expected) or record[key] != expected:
            return None
    if type(record['connection_selection']) is not str:
        return None
    for key in ('initial_quic_datagrams', 'websocket_contact_elapsed_us', 'deferred_credential_issues'):
        if type(record[key]) is not int:
            return None
    expected_selection = 'auto' if kind == 'wsautoloss' else 'websocket'
    if record['connection_selection'] != expected_selection:
        return None
    if kind == 'wsautoloss':
        if (not 1 <= record['initial_quic_datagrams'] <= 32
                or not 3000000 <= record['websocket_contact_elapsed_us'] <= 30000000
                or record['deferred_credential_issues'] != 1):
            return None
    elif (record['initial_quic_datagrams'] != 0
            or not 0 <= record['websocket_contact_elapsed_us'] <= 30000000
            or record['deferred_credential_issues'] != 0):
        return None
    return record


def sqlite_reply_loss_gate_record(kind, stderr_bytes, unknown):
    if kind not in ('wsloss', 'wsautoloss'):
        return None
    record = sqlite_reply_loss_record(stderr_bytes, kind)
    if record is None and 'sqlite_reply_loss_not_observed_valid' not in unknown:
        unknown.append('sqlite_reply_loss_not_observed_valid')
    return record


COMPACT_SQLITE_REPLY_LOSS_MARKER = b'MOUNT_RS_COMPACT_SQLITE_REPLY_LOSS'
COMPACT_SQLITE_REPLY_LOSS_FIXED = {
    **SQLITE_REPLY_LOSS_FIXED,
    'schema_version': 3,
    'connection_selection': 'auto',
    'deferred_credential_issues': 1,
    'storage_mode': 'MRC5',
    'metadata_provider': 'sqlite',
    'block_provider': 'sqlite',
    'configured_cli_child': 0,
    'compact_metadata_checkpoints': 3,
    'compact_backing_and_generation_preserved': 1,
    'compact_publication_preserved_from_held_commit': 1,
    'full_metadata_preserved_from_held_commit': 1,
}
COMPACT_SQLITE_REPLY_LOSS_FIELDS = frozenset(COMPACT_SQLITE_REPLY_LOSS_FIXED) | frozenset((
    'initial_quic_datagrams', 'websocket_contact_elapsed_us',
))


def compact_sqlite_reply_loss_record(stderr_bytes, kind):
    """Validate one complete schema-3 fixture frame for the fixed compact gate."""
    if kind != 'wscompactloss' or type(stderr_bytes) is not bytes:
        return None
    frames = []
    lines = stderr_bytes.split(b'\n')
    for index, line in enumerate(lines):
        # Mixed legacy/compact evidence cannot qualify the compact contract.
        if SQLITE_REPLY_LOSS_MARKER in line:
            return None
        if COMPACT_SQLITE_REPLY_LOSS_MARKER not in line:
            continue
        if (index == len(lines)-1 or b'\r' in line or len(line)+1 > 4096
                or not line.startswith(COMPACT_SQLITE_REPLY_LOSS_MARKER+b' ')):
            return None
        frames.append(line[len(COMPACT_SQLITE_REPLY_LOSS_MARKER)+1:])
    if len(frames) != 1:
        return None
    payload = frames[0]
    if not payload.startswith(b'{') or not payload.endswith(b'}'):
        return None

    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate_compact_sqlite_reply_loss_key')
            result[key] = value
        return result

    def non_integer_number(_):
        raise ValueError('non_integer_compact_sqlite_reply_loss_number')

    try:
        record = json.loads(payload.decode('ascii'), object_pairs_hook=unique_object,
                            parse_float=non_integer_number, parse_constant=non_integer_number)
    except (UnicodeDecodeError, ValueError, RecursionError):
        return None
    if type(record) is not dict or set(record) != COMPACT_SQLITE_REPLY_LOSS_FIELDS:
        return None
    for key, expected in COMPACT_SQLITE_REPLY_LOSS_FIXED.items():
        if type(record[key]) is not type(expected) or record[key] != expected:
            return None
    for key in ('initial_quic_datagrams', 'websocket_contact_elapsed_us'):
        if type(record[key]) is not int:
            return None
    if (not 1 <= record['initial_quic_datagrams'] <= 32
            or not 3000000 <= record['websocket_contact_elapsed_us'] <= 30000000):
        return None
    return record


def compact_sqlite_reply_loss_gate_record(kind, stderr_bytes, unknown):
    if kind != 'wscompactloss':
        return None
    record = compact_sqlite_reply_loss_record(stderr_bytes, kind)
    if record is None and 'compact_sqlite_reply_loss_not_observed_valid' not in unknown:
        unknown.append('compact_sqlite_reply_loss_not_observed_valid')
    return record


PEER_RECONNECT_PROGRESS = frozenset((
    'before_stop_a', 'after_stop_a',
    'before_blackhole_bind', 'after_blackhole_bind',
    'before_replica_first_poll', 'after_replica_first_poll',
    'before_initial_receive', 'after_initial_receive',
    'before_restart_a', 'after_restart_a', 'incoming_replica_observed',
    'before_store_prepare', 'after_store_prepare',
    'before_public_get', 'after_public_get',
    'before_drop_replica_put', 'after_drop_replica_put',
    'before_direct_get', 'after_direct_get',
    'before_healthy_put', 'after_healthy_put',
    'before_replica_readback_get', 'after_replica_readback_get',
    'before_partition_control', 'after_partition_control',
    'before_runtime_shutdown', 'after_runtime_shutdown',
    'before_pair_shutdown', 'after_server_shutdown', 'after_requester_shutdown',
    'after_pair_shutdown', 'before_cleanup_receipt', 'after_cleanup_receipt',
    'before_amplification_assertions', 'after_amplification_assertions',
))


def peer_reconnect_sampled_progress(data):
    # The caller retains at most first/last 2048 raw bytes. Keep their boundary
    # lines separate: a synthetic sample separator cannot complete a record.
    # This is the last observed marker in those windows, not the last phase run.
    windows = [('short_input', data, False)] if len(data) <= 2048 else [
        ('first_window', data[:2048], False),
        ('last_window', data[-2048:], True),
    ]
    prefix = b'peer_read_reconnect_progress='
    header = ('test '+EXACT_CASES['peerreconnect']+' ... ').encode('ascii')
    marker, window, invalid = 'unobserved', 'unobserved', None
    for location, fragment, omit_boundary in windows:
        lines = fragment.split(b'\n')[:-1]  # Exclude an unterminated last line.
        if omit_boundary:
            lines = lines[1:]  # Its original start may be outside the sample.
        for line in lines:
            if line.startswith(header):
                line = line[len(header):]
            if not line.startswith(prefix):
                continue
            try:
                value = line[len(prefix):].decode('ascii')
            except UnicodeDecodeError:
                value = None
            if value not in PEER_RECONNECT_PROGRESS:
                invalid = location
            else:
                marker, window = value, location
    if invalid is not None:
        marker, window = 'invalid', invalid
    return {'last_sampled_progress_marker': marker, 'progress_sample_window': window}


def gate_failure_diagnostic(kind, receipt, stdout_bytes, stderr_bytes):
    if kind not in COMMANDS:
        return None
    status = receipt.get('returncode')
    status = status if type(status) is int else None
    lifecycle = any(receipt.get(field) is not True for field in (
        'owned_child_reaped', 'owned_group_absent', 'pipes_eof', 'signal_decisions_finished',
    ))
    deadline = receipt.get('deadline_exceeded') is True
    unknown = bool(receipt.get('sticky_unknown'))
    changed = receipt.get('source_unchanged') is not True
    overflow = any(item.get('overflow') is True for item in receipt.get('logs', {}).values())
    if status == 0 and not (lifecycle or deadline or unknown or changed or overflow) and receipt.get('primary_failure') is None:
        return None

    # Each stream contributes at most its first and last 2048 raw bytes.
    # Keep a separator between disjoint samples rather than inventing markers.
    def sample(data):
        return data if len(data) <= 2048 else data[:2048]+b'\n'+data[-2048:]
    stdout = sample(stdout_bytes).decode('utf-8', errors='replace')
    observed = stdout+'\n'+sample(stderr_bytes).decode('utf-8', errors='replace')
    failure_class = 'unclassified'
    if 'failed to download' in observed and ('--offline' in observed or 'attempting to make an HTTP request' in observed):
        failure_class = 'offline_download'
    elif 'no matching package named' in observed and 'offline' in observed:
        failure_class = 'offline_resolution'
    elif 'could not compile' in observed:
        failure_class = 'compile_failed'
    else:
        selected = EXACT_CASES.get(kind)
        if (selected is not None
            and re.search(r'^running 1 test$', stdout, re.M)
            and re.search(r'^test '+re.escape(selected)+r' \.\.\. ', stdout, re.M)
            and re.search(r'^test result: FAILED\. 0 passed; 1 failed; 0 ignored;', stdout, re.M)):
            failure_class = 'exact_test_failed'
        elif (re.search(r'^running 0 tests$', stdout, re.M)
              and re.search(r'^test result: ok\. 0 passed; 0 failed; 0 ignored;', stdout, re.M)):
            failure_class = 'zero_tests'
    diagnostic = {
        'kind': kind, 'failure_class': failure_class, 'returncode': status,
        'deadline_exceeded': deadline, 'lifecycle_unsettled': lifecycle,
        'sticky_unknown': unknown, 'source_changed': changed, 'output_overflow': overflow,
    }
    if kind == 'peerreconnect':
        diagnostic.update(peer_reconnect_sampled_progress(stdout_bytes))
    return diagnostic


def gate_failure_log_sample(path):
    # Read no more than 4096 raw bytes per owned log, without scanning its middle.
    try:
        with path.open('rb') as log:
            first = log.read(2048)
            log.seek(0, os.SEEK_END)
            size = log.tell()
            if size <= 2048:
                return first
            log.seek(max(2048, size-2048))
            return first+log.read(2048)
    except OSError:
        return b''


def exact_case_passed(output, selected_test):
    # Nocapture output may separate the selected name and its trailing "ok".
    # The fixed --exact command plus one executed, nonignored passing case is
    # the qualification boundary; partial/zero/multi-case output is rejected.
    # Nested controllers can print earlier success. Every observed harness
    # start needs a result, and only a passing summary at stdout's end,
    # followed by optional blank LF lines, qualifies the outer result.
    return bool(
        re.search(r'^running 1 test$', output, re.M)
        and re.search(r'^test '+re.escape(selected_test)+r' \.\.\. ', output, re.M)
        and len(re.findall(r'^running [0-9]+ tests?$', output, re.M))
            == len(re.findall(r'^test result:', output, re.M))
        and re.search(r'^test result: ok\. 1 passed; 0 failed; 0 ignored;[^\n]*\n*\Z', output, re.M)
    )


def package_harness_passed(output, required_cases):
    # All package harnesses must finish, including feature-gated empty ones.
    # A terminal successful Cargo exit alone cannot establish executed tests.
    starts = re.findall(r'^running ([0-9]+) tests?$', output, re.M)
    summaries = re.findall(
        r'^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored; ([0-9]+) measured;[^\n]*\n',
        output, re.M,
    )
    if not starts or len(starts) != len(summaries):
        return False
    if len(re.findall(r'^test result:', output, re.M)) != len(summaries):
        return False
    if not re.search(r'^test result: ok\.[^\n]*\n+\Z', output, re.M):
        return False
    total_passed = 0
    for start, (status, passed, failed, ignored, measured) in zip(starts, summaries):
        if status != 'ok' or int(failed) != 0:
            return False
        if int(start) != sum(int(count) for count in (passed, failed, ignored, measured)):
            return False
        total_passed += int(passed)
    if total_passed == 0:
        return False
    for case in required_cases:
        records = re.findall(r'^test ' + re.escape(case) + r' \.\.\. ([^\n]*)$', output, re.M)
        if records != ['ok']:
            return False
    return True


def compact_sqlite_named_case_passed(output):
    selected_test = EXACT_CASES['wscompactloss']
    return bool(
        exact_case_passed(output, selected_test)
        and re.findall(r'^test ([^\s]+) \.\.\.', output, re.M) == [selected_test]
        and len(re.findall(r'^running [0-9]+ tests?$', output, re.M)) == 1
        and len(re.findall(r'^test result:', output, re.M)) == 1
    )


def held_reply_monitor_suite_passed(output):
    return bool(
        named_suite_passed(output, EXPECTED_SUITES['wsheldmonitor'], nocapture=True)
        and len(re.findall(r'^running [0-9]+ tests?$', output, re.M)) == 1
        and len(re.findall(r'^test result:', output, re.M)) == 1
    )


def terminal_eperm_eligible(platform, action, terminal, error_number):
    # XNU killpg filters SZOMB and returns EPERM for an existing zombie-only
    # group. Keep errno untouched; final absence, not signal delivery, decides
    # containment. https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c
    return platform == 'darwin' and action in {'postterminal_term', 'postterminal_kill'} and terminal is True and error_number == 1

def terminal_eperm_settled(reaped, group_absent, eof, deadline, lifecycle_unknown):
    return reaped is True and group_absent is True and eof is True and deadline is False and not lifecycle_unknown

def main():
    kind=sys.argv[1];command,limit,profile,trace=COMMANDS[kind]
    assert os.name=='posix' and hasattr(os,'waitid') and hasattr(os,'WNOWAIT'), 'Unix ownership observer required'
    FAULT_KINDS={'cachetests','redisfault','rediscleanup','wsloss','remotetests','faultclippy','fmt','cachemetrics','peermetrics','peerreconnect','peerport','quinnclose','quinnordinary','quinnruntimeclose','createprep','createpath','createguard','createunit','chunkedtests','chunkedclippy','cacheprofileoff','cacheprofileon','storagealloc','corealloc','coremetrics','cachemetricsclippy','cacheconsumers','cacheconsumerred','clidiagnostics','clicompact','napimetrics','consumerclippy','cachemetricstrace'}
    FAULT_KINDS.update({'clientsetupmetrics','discoverymetrics'})
    FAULT_KINDS.add('clientmetrics')
    FAULT_KINDS.add('filesystemmetrics')
    FAULT_KINDS.add('peeriometrics')
    FAULT_KINDS.add('peeriometricstrace')
    FAULT_KINDS.add('sqlitecache')
    FAULT_KINDS.update({'lazycli', 'lazyservice', 'targetlazy', 'targetlazyunit', 'balancedprobe'})
    FAULT_KINDS.add('wsclientmetrics')
    FAULT_KINDS.add('wsautoloss')
    FAULT_KINDS.update({'wscompactloss','wsheldmonitor'})
    FAULT_KINDS.update({'wsservicedefault','wsserviceon','wsserviceunit','wspackages','wspackagesprofiled','wsclippy','wsclippyprofiled'})
    FAULT_KINDS.update({'holdersdk', 'holdercli', 'holdercliprofiled', 'holderclippy', 'holderclippyprofiled'})
    FAULT_KINDS.update({'tidbcoldcompile', 'tidbcoldunit'})
    FAULT_KINDS.update({'objectstorered', 'objectstorecachered', 'rustfshttpred', 'objectstoreunit', 'objectstorecacheunit', 'rustfshttpunit', 'rustfsownedprefix', 'objectstorealloc', 'objectstorequalification', 'objectstoreclippy'})
    FAULT_KINDS.update(OBJECT_STORE_EXPORT_SUITES)
    FAULT_KINDS.update(COLD_HOLDER_CASES)
    assert kind in FAULT_KINDS, 'fixed fault qualification commands only'
    root=pathlib.Path(tempfile.mkdtemp(prefix='mount-rs-owned-fault-'+kind+'-',dir=os.environ.get('MOUNT_RS_FAILURE_EVIDENCE_ROOT',tempfile.gettempdir())));os.chmod(root,0o700)
    fixture_tmp=root/'fixtures';fixture_tmp.mkdir(mode=0o700)
    redis_pin=None
    if kind in {'redisfault','rediscleanup'}:
     redis_configured=os.environ.get('MOUNT_RS_CACHE_REDIS_SERVER')
     assert redis_configured and pathlib.Path(redis_configured).is_absolute(), 'explicit absolute Redis executable required'
     redis_path=pathlib.Path(redis_configured).resolve(strict=True)
     redis_stat=redis_path.stat();assert stat.S_ISREG(redis_stat.st_mode) and os.access(redis_path,os.X_OK)
     redis_pin={'path':str(redis_path),'size':redis_stat.st_size,'sha256':hashlib.sha256(redis_path.read_bytes()).hexdigest()}
     # Bind the selected deployment binary before launch and require it unchanged afterward.
    def write(name,value):
     data=(json.dumps(value,sort_keys=True,indent=2)+'\n').encode();fd=os.open(root/name,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600);assert os.write(fd,data)==len(data);os.close(fd);return hashlib.sha256(data).hexdigest()
    def frozen():
     if kind in {'node','processnode','diagnosticnode','cacheconsumers','cacheconsumerred'}:
      pending=[BASE/v for v in command if v.endswith('.mjs')];found=set()
      while pending:
       path=pending.pop().resolve(strict=True);assert path.is_relative_to(BASE) and path not in [BASE]
       if path in found:continue
       found.add(path)
       for spec in re.findall(r'(?:from\s+|import\s*\(?\s*)[\"\x27](\.[^\"\x27]+\.mjs)[\"\x27]',path.read_text()):pending.append(path.parent/spec)
      paths=sorted(found | {BASE/'benchmarks/storage/capture-native.cjs',BASE/'bindings/mount-rs-napi/index.js'})
      paths.extend([pathlib.Path(__file__).resolve(),BASE/'scripts/test-remote-failures-controls.py',BASE/'.github/workflows/ci.yml',BASE/'.github/workflows/remote-drives.yml',BASE/'crates/mount-rs-blob-cache/tests/support/stage_metrics.rs'])
      paths.extend(BASE/v for v in ['scripts/test-foundationdb.sh','tests/ozone/production-rollout-contract.json','scripts/test-tidb.sh','scripts/test-rustfs.sh'])
     else:
      tracked=subprocess.check_output(['git','ls-files','--cached','--others','--exclude-standard','-z'],cwd=BASE).decode().split('\0')
      paths=[BASE/v for v in tracked if v and (v.endswith(('.rs','.toml','.lock')) or v=='scripts/cargo-shared')]
      paths.append(BASE/'.github/workflows/ci.yml')
      paths.append(pathlib.Path(__file__).resolve())
      paths.append(BASE/'scripts/test-remote-failures-controls.py')
      paths.append(BASE/'.github/workflows/remote-drives.yml')
      paths.append(BASE/'crates/mount-rs-remote-client/tests/quic_mount_reply_loss/mod.rs')
      paths.append(BASE/'crates/mount-rs-blob-cache/tests/support/stage_metrics.rs')
     paths.append(BASE/'crates/mount-rs-blob-cache/tests/support/sqlite_cache.rs')
     if (BASE/'crates/mount-rs-remote-client/src/websocket_metrics_tests.rs').is_file():
      paths.append(BASE/'crates/mount-rs-remote-client/src/websocket_metrics_tests.rs')
     if (BASE/'crates/mount-rs-service/tests/websocket_diagnostics.rs').is_file():
      paths.append(BASE/'crates/mount-rs-service/tests/websocket_diagnostics.rs')
     paths.extend(BASE/v for v in ['filesystems/mount-rs-chunked/src/causal_metrics.rs','filesystems/mount-rs-chunked/tests/filesystem_causal_metrics.rs','tests/filesystem_causal_profile_allocations.rs','filesystems/mount-rs-chunked/tests/current_path_create_rebase.rs','filesystems/mount-rs-chunked/tests/compact_snapshot_revision.rs','filesystems/mount-rs-chunked/src/create_guard_metrics_tests.rs','filesystems/mount-rs-chunked/src/create_rebase.rs'] if (BASE/v).is_file())
     paths.append(BASE/'crates/mount-rs-blob-cache/tests/quinn_close.rs')
     paths.append(BASE/'crates/mount-rs-remote-client/src/connection_metrics_tests.rs')
     target_runtime=BASE/'crates/mount-rs-service/tests/support/production_target/lazy_runtime.rs'
     if kind=='targetlazyunit' or target_runtime.is_file():paths.append(target_runtime)
     paths.extend(BASE/v for v in (
      'apps/mount-rs-cli/src/remote_runtime.rs',
      'apps/mount-rs-cli/src/remote_runtime/lifecycle_tests.rs',
      'apps/mount-rs-cli/src/server_cache/cold_holder.rs',
      'apps/mount-rs-cli/src/server_cache/cold_holder_tests.rs',
      'apps/mount-rs-cli/tests/ten_process_cache_support/cold_retirement.rs',
      'apps/mount-rs-cli/tests/ten_process_cache_support/object_store_projection.rs',
      'apps/mount-rs-cli/tests/ten_process_cache_support/object_store_projection_tests.rs',
      'src/diagnostics/object_store.rs',
      'crates/mount-rs-service/src/object_store_diagnostics.rs',
      'crates/mount-rs-service/src/object_store_diagnostics_tests.rs',
      'crates/mount-rs-service/src/filesystem_runtime.rs',
      'crates/mount-rs-service/tests/filesystem_runtime.rs',
     ) if (BASE/v).is_file())
     if (BASE/'crates/mount-rs-blob-cache/tests/discovery_metrics.rs').is_file():paths.append(BASE/'crates/mount-rs-blob-cache/tests/discovery_metrics.rs')
     # Pin the complete scoped dependency, including upstream provenance and
     # licenses, even before new vendor files have entered the Git index.
     vendor=BASE/'vendor/quinn-proto-0.11.18'
     if vendor.is_dir():paths.extend(p for p in vendor.rglob('*') if p.is_file())
     paths.append(BASE/'scripts/cargo-shared-env.sh')
     return {str(p.relative_to(BASE)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(set(paths))}
    before=frozen();write('source-before.json',before)
    env=os.environ.copy();env.update({'MOUNT_RS_PROFILE_IO':str(profile),'MOUNT_RS_TRACE_STORAGE':str(trace),'MOUNT_RS_TRACE_REQUESTS':'0','CARGO_TARGET_DIR':os.environ.get('CARGO_TARGET_DIR',os.environ.get('MOUNT_RS_CARGO_TARGET_DIR',str(root/'cargo-target')))})
    if kind in {'wsloss','wsautoloss','wscompactloss','wsheldmonitor','wsclientmetrics','wsservicedefault','wsserviceon','wsserviceunit','wspackages','wspackagesprofiled','wsclippy','wsclippyprofiled','clidiagnostics','clicompact'}:env['MOUNT_RS_TRACE_SERVICE']='0'
    if kind in OBJECT_STORE_EXPORT_SUITES:env['MOUNT_RS_TRACE_SERVICE']='0'
    if kind in {'cacheconsumers','cacheconsumerred'}:
     # Pure suites forbid loading a real addon or latching native profiling.
     for key in ['MOUNT_RS_PROFILE_IO','MOUNT_RS_TRACE_STORAGE','MOUNT_RS_TRACE_REQUESTS','NAPI_RS_FORCE_WASI','NAPI_RS_WASI_FLAVOR','NODE_PATH']:
      env.pop(key,None)
     env['NAPI_RS_NATIVE_LIBRARY_PATH']=str(BASE/'benchmarks/storage/capture-native.cjs')
    env['TMPDIR']=str(fixture_tmp)
    env.pop('MOUNT_RS_CACHE_REDIS_SERVER',None)
    if redis_pin is not None:env['MOUNT_RS_CACHE_REDIS_SERVER']=redis_pin['path']
    if kind in {'wsloss','wsautoloss','wscompactloss','wsheldmonitor'}:env['MOUNT_RS_REMOTE_SQLITE_REPLY_LOSS']='1'
    # All owned files/selector setup precede child creation.
    selector=selectors.DefaultSelector();logs={};cap=4194304
    for name in ['stdout','stderr']:
     fd=os.open(root/(name+'.log'),os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
     logs[name]={'fd':fd,'received':0,'retained':0,'overflow':False}
    start=time.monotonic();end=start+limit;proc=None;owned=None
    unknown=[];signals=[];deadline=False;primary=None;code=None;eof=False;absent=None
    term_at=None;kill_at=None;terminal_observed=False;signal_decisions_finished=False

    def send_owned(action,sig):
     attempt={'action':action,'signal':int(sig),'errno':None,'outcome':'pending'}
     signals.append(attempt)
     try:
      os.killpg(owned,sig);attempt['outcome']='sent'
     except ProcessLookupError:
      attempt.update(outcome='already_absent',errno=3)
     except OSError as error:
      attempt.update(outcome='error',errno=error.errno)
      if terminal_eperm_eligible(sys.platform, action, terminal_observed, error.errno):
       attempt['terminal_darwin_eperm']='pending_final_observations'
      else:
       unknown.append(action+'_signal_error')

    def leader_terminal():
     # WNOWAIT preserves the original PID/PGID owner through every signal decision.
     result=os.waitid(os.P_PID,owned,os.WEXITED|os.WNOHANG|os.WNOWAIT)
     return result is not None

    def drain_once(seconds):
     for key,_ in selector.select(max(0,min(seconds,end-time.monotonic()))):
      data=os.read(key.fileobj.fileno(),65536);entry=logs[key.data]
      if not data:
       selector.unregister(key.fileobj);key.fileobj.close();continue
      entry['received']+=len(data);keep=data[:max(0,cap-entry['retained'])]
      if keep:
       view=memoryview(keep)
       while view:
        n=os.write(entry['fd'],view)
        if n<=0:raise OSError('output_write_no_progress')
        entry['retained']+=n;view=view[n:]
      if entry['received']>cap:
       entry['overflow']=True
       if 'output_overflow' not in unknown:unknown.append('output_overflow')

    try:
     proc=subprocess.Popen(command,cwd=BASE,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
     owned=proc.pid
     # start_new_session is the ownership contract; failure of its identity check
     # is sticky, and cleanup still addresses only the child-created session group.
     if os.getpgid(owned)!=owned:raise RuntimeError('initial_owned_group_mismatch')
     for name,pipe in [('stdout',proc.stdout),('stderr',proc.stderr)]:
      os.set_blocking(pipe.fileno(),False);selector.register(pipe,selectors.EVENT_READ,name)
     while True:
      terminal_observed=leader_terminal()
      now=time.monotonic()
      if now>=end-5:deadline=True
      if terminal_observed and term_at is None:
       term_at=now;send_owned('postterminal_term',signal.SIGTERM)
      if terminal_observed and not selector.get_map() and kill_at is not None:
       break
      if (now>=end-5 or unknown) and term_at is None:
       if now>=end-5:deadline=True
       term_at=now;send_owned('term',signal.SIGTERM)
      if term_at is not None and now>=term_at+2 and kill_at is None:
       kill_at=now;send_owned('postterminal_kill' if terminal_observed else 'kill',signal.SIGKILL)
      if now>=end:
       unknown.append('parent_settlement_deadline');break
      drain_once(.05)
    except BaseException as error:
     primary=error
     unknown.append('primary_'+type(error).__name__)
    finally:
     if proc is not None:
      # Every path settles its owned-group signal decisions while the leader is pinned.
      try:
       if term_at is None:
        term_at=time.monotonic();send_owned('final_term',signal.SIGTERM)
       if kill_at is None:
        while time.monotonic()<min(end,term_at+2):drain_once(.05)
      except BaseException as cleanup_error:
       unknown.append('cleanup_'+type(cleanup_error).__name__)
      finally:
       if kill_at is None:
        kill_at=time.monotonic()
        if kill_at>=end:
         deadline=True
         if 'parent_settlement_deadline' not in unknown:unknown.append('parent_settlement_deadline')
        try:send_owned('forced_deadline_kill' if kill_at>=end else 'final_kill',signal.SIGKILL)
        except BaseException as kill_error:unknown.append('final_kill_'+type(kill_error).__name__)
      # No group-directed signal may occur after this point.
      signal_decisions_finished=True
      try:code=proc.wait(timeout=max(0,end-time.monotonic()))
      except subprocess.TimeoutExpired:unknown.append('owned_child_unreaped')
      except BaseException as wait_error:unknown.append('wait_'+type(wait_error).__name__)
      # No further signals: allow kernel reaping of killed descendants to settle.
      probe_end=min(end,time.monotonic()+2)
      while True:
       try:os.killpg(owned,0);absent=False
       except ProcessLookupError:absent=True
       except OSError as probe_error:
        absent=None;unknown.append('final_group_unavailable')
        signals.append({'action':'group_absence_probe','signal':0,'outcome':'error','errno':probe_error.errno})
       if absent is not False or time.monotonic()>=probe_end:break
       drain_once(.05)
      if absent is False:unknown.append('owned_group_still_present')
      # Drain normal termination output within the original total deadline.
      if primary is None:
       try:
        while selector.get_map() and time.monotonic()<end:drain_once(.05)
       except BaseException as drain_error:
        primary=drain_error;unknown.append('drain_'+type(drain_error).__name__)
      eof=not selector.get_map()
      for pipe in [proc.stdout,proc.stderr]:
       if pipe is not None and not pipe.closed:pipe.close()
     try:selector.close()
     except BaseException as close_error:
      if primary is None:primary=close_error
      unknown.append('selector_close_error')
     for entry in logs.values():
      try:os.close(entry.pop('fd'))
      except BaseException as close_error:
       if primary is None:primary=close_error
       unknown.append('log_close_error')

    # Reconcile only Darwin postterminal EPERM, after the independent ownership
    # observations; retain original outcome/errno and never signal after reap.
    for attempt in signals:
     if attempt.get('terminal_darwin_eperm') == 'pending_final_observations':
      settled=terminal_eperm_settled(code is not None, absent, eof, deadline, unknown)
      attempt['terminal_darwin_eperm']='empty_group_after_reap' if settled else 'unresolved'
      if not settled:unknown.append(attempt['action']+'_signal_error')

    # Evidence writes occur after cleanup decisions/reap. They never gate cleanup.
    try:
     for name,item in logs.items():
      raw=(root/(name+'.log')).read_bytes();item.update({'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest(),'path':str(root/(name+'.log'))})
     selected_test=EXACT_CASES.get(kind)
     selected_test_pass=None
     if selected_test is not None:
      output=(root/'stdout.log').read_text(errors='replace')
      selected_test_pass=compact_sqlite_named_case_passed(output) if kind=='wscompactloss' else exact_case_passed(output, selected_test)
      if not selected_test_pass:unknown.append('exact_named_case_not_observed_passed')
     selected_suite=EXPECTED_SUITES.get(kind)
     selected_suite_pass=None
     if selected_suite is not None:
      output=(root/'stdout.log').read_text()
      selected_suite_pass=held_reply_monitor_suite_passed(output) if kind=='wsheldmonitor' else lazy_suite_passed(output,kind) if kind in {'lazycli','lazyservice'} else named_suite_passed(output,selected_suite,nocapture=kind in {'createpath','createunit','filesystemmetrics','targetlazyunit'} or kind in OBJECT_STORE_EXPORT_SUITES)
      if not selected_suite_pass:unknown.append('named_suite_not_observed_passed')
     required_package_cases=EXPECTED_PACKAGE_CASES.get(kind)
     package_harness_pass=None
     if required_package_cases is not None:
      package_harness_pass=package_harness_passed((root/'stdout.log').read_text(),required_package_cases)
      if not package_harness_pass:unknown.append('package_harness_not_observed_passed')
     cache_slow_records=None
     if kind=='cachemetricstrace':
      cache_slow_records=slow_logging_file_records(root/'stderr.log')
      if cache_slow_records is None:unknown.append('cache_slow_logging_not_observed_valid')
     if kind=='peeriometricstrace':
      cache_slow_records=slow_logging_file_records(root/'stderr.log', peer=True)
      if cache_slow_records is None:unknown.append('peer_slow_logging_not_observed_valid')
     sqlite_reply_loss=None
     if kind in {'wsloss','wsautoloss'}:
      sqlite_reply_loss=sqlite_reply_loss_gate_record(kind,(root/'stderr.log').read_bytes(),unknown)
     elif kind=='wscompactloss':
      sqlite_reply_loss=compact_sqlite_reply_loss_gate_record(kind,(root/'stderr.log').read_bytes(),unknown)
     after=frozen();write('source-after.json',after)
     redis_unchanged=None if redis_pin is None else redis_path.is_file() and redis_path.stat().st_size==redis_pin['size'] and hashlib.sha256(redis_path.read_bytes()).hexdigest()==redis_pin['sha256']
     if redis_unchanged is False:unknown.append('redis_executable_changed')
     fixture_children=sorted(p.name for p in fixture_tmp.iterdir())
     fixture_removed=False
     if code is not None and absent is True and eof:
      assert shutil.rmtree.avoids_symlink_attacks, 'require fd-based removal of owned fixture root'
      shutil.rmtree(fixture_tmp);fixture_removed=not fixture_tmp.exists()
     else:unknown.append('fixture_directory_retained_unknown_process_ownership')
     receipt={'schema':'mount-rs.causal-bounded-local-gate.v2','kind':kind,'command':command,'elapsed_seconds':time.monotonic()-start,'parent_limit_seconds':limit,'cleanup_reserved_seconds':5,'returncode':code,'owned_pid':owned,'new_session':True,'wnowait_owner_pin':True,'signal_decisions_finished':signal_decisions_finished,'owned_child_reaped':code is not None,'owned_group_absent':absent,'pipes_eof':eof,'deadline_exceeded':deadline,'signals':signals,'sticky_unknown':unknown,'primary_failure':None if primary is None else {'type':type(primary).__name__,'errno':getattr(primary,'errno',None)},'logs':logs,'source_count':len(before),'source_unchanged':before==after,'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=BASE).decode().strip(),'runner_sha256':hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),'profile':env.get('MOUNT_RS_PROFILE_IO'),'trace':env.get('MOUNT_RS_TRACE_STORAGE'),'native_capture_binding':str(BASE/'benchmarks/storage/capture-native.cjs') if kind in {'node','processnode','diagnosticnode','cacheconsumers','cacheconsumerred'} else None,'automatic_retry':False,'cache_slow_records':cache_slow_records,'selected_suite':selected_suite,'named_suite_observed_passed':selected_suite_pass,'selected_test':selected_test,'exact_named_case_observed_passed':selected_test_pass,'redis_executable':redis_pin,'redis_executable_unchanged':redis_unchanged,'fixture_tmpdir':str(fixture_tmp),'fixture_children_before_postprocess_removal':fixture_children,'fixture_tmpdir_removed_after_reap_group_absence_eof':fixture_removed,'postterminal_group_sweep':'containment_only; fixture_cleanup_requires_in_test_assertions'}
     receipt.update({'required_package_cases':required_package_cases,'package_harness_observed_passed':package_harness_pass})
     if kind in {'wsloss','wsautoloss','wscompactloss'}:receipt['sqlite_reply_loss']=sqlite_reply_loss
     diagnostic=gate_failure_diagnostic(kind, receipt, gate_failure_log_sample(root/'stdout.log'), gate_failure_log_sample(root/'stderr.log'))
     receipt['failure_diagnostic']=diagnostic
     sha=write('receipt.json',receipt);print(json.dumps({'path':str(root/'receipt.json'),'sha256':sha,'returncode':code,'elapsed_seconds':receipt['elapsed_seconds'],'source_unchanged':before==after,'unknown':unknown,'group_absent':absent,'pipes_eof':eof}))
     if diagnostic is not None and os.environ.get('GITHUB_ACTIONS') == 'true':
      fields=['kind='+diagnostic['kind'], 'class='+diagnostic['failure_class'], 'returncode_known='+str(int(diagnostic['returncode'] is not None))]
      if diagnostic['returncode'] is not None:fields.append('returncode='+str(diagnostic['returncode']))
      fields.extend(field+'='+str(int(diagnostic[field])) for field in ['deadline_exceeded','lifecycle_unsettled','sticky_unknown','source_changed','output_overflow'])
      if diagnostic['kind']=='peerreconnect':
       fields.extend(field+'='+diagnostic[field] for field in ['last_sampled_progress_marker','progress_sample_window'])
      try:print('::error title=Owned gate failure::'+' '.join(fields),file=sys.stderr)
      except OSError:pass
    except BaseException:
     if primary is not None:raise primary
     raise
    if primary is not None:raise primary
    raise SystemExit(0 if code==0 and absent is True and eof and not deadline and not unknown and before==after and not any(v['overflow'] for v in logs.values()) else 1)

if __name__=='__main__':
    main()
