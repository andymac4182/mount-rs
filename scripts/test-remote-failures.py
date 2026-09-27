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
EXACT_CASES = {
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
EXPECTED_SUITES = {
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
        'missing_preparation_waits_for_full_publication_and_releases_gate_before_blocks',
        'canceled_missing_preparation_retains_no_pending_full_capture',
        'occupied_preparation_uses_full_current_selected_body',
        'damaged_traversed_or_unrelated_guard_refuses_create_without_publication',
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
        'full_current_banks_with_maximum_u64_fit_existing_record_limit',
        'oversized_serializable_value_emits_only_a_bounded_incomplete_record',
        'serialization_failure_uses_a_fixed_incomplete_record',
    )),
}
def named_suite_passed(output, names, nocapture=False):
    count=len(names)
    return bool(
        re.search(r'^running '+str(count)+r' tests$', output, re.M)
        and re.search(r'^test result: ok\. '+str(count)+r' passed; 0 failed; 0 ignored;', output, re.M)
        and (
            (lambda observed: len(observed) == count and set(observed) == set(names))(
                re.findall(r'^test ([^\s]+) \.\.\.', output, re.M)
            ) if nocapture else
            all(re.search(r'^test '+re.escape(name)+r' \.\.\. ok$', output, re.M) for name in names)
        )
    )


def cache_slow_logging_records(output):
    names = {
        'blob_cache.miss.admission_wait', 'blob_cache.miss.singleflight_wait',
        'blob_cache.ram.lookup', 'blob_cache.disk.lookup',
        'blob_cache.peer.connection_lock_wait', 'blob_cache.peer.connection_establish',
    }
    records = []
    for line in output.splitlines():
        if not line.startswith('MOUNT_RS_STORAGE_SLOW'):
            continue
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
    return bool(
        re.search(r'^running 1 test$', output, re.M)
        and re.search(r'^test '+re.escape(selected_test)+r' \.\.\. ', output, re.M)
        and re.search(r'^test result: ok\. 1 passed; 0 failed; 0 ignored;', output, re.M)
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
    FAULT_KINDS.add('clientmetrics')
    FAULT_KINDS.add('filesystemmetrics')
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
      tracked=subprocess.check_output(['git','ls-files','-z'],cwd=BASE).decode().split('\0')
      paths=[BASE/v for v in tracked if v and (v.endswith(('.rs','.toml','.lock')) or v=='scripts/cargo-shared')]
      paths.append(BASE/'.github/workflows/ci.yml')
      paths.append(pathlib.Path(__file__).resolve())
      paths.append(BASE/'scripts/test-remote-failures-controls.py')
      paths.append(BASE/'.github/workflows/remote-drives.yml')
      paths.append(BASE/'crates/mount-rs-remote-client/tests/quic_mount_reply_loss/mod.rs')
      paths.append(BASE/'crates/mount-rs-blob-cache/tests/support/stage_metrics.rs')
      paths.extend(BASE/v for v in ['filesystems/mount-rs-chunked/src/causal_metrics.rs','filesystems/mount-rs-chunked/tests/filesystem_causal_metrics.rs','tests/filesystem_causal_profile_allocations.rs','filesystems/mount-rs-chunked/tests/compact_snapshot_revision.rs','filesystems/mount-rs-chunked/src/create_guard_metrics_tests.rs','filesystems/mount-rs-chunked/src/create_rebase.rs','filesystems/mount-rs-chunked/tests/current_path_create_rebase.rs'] if (BASE/v).is_file())
     paths.append(BASE/'crates/mount-rs-blob-cache/tests/quinn_close.rs')
     paths.append(BASE/'crates/mount-rs-remote-client/src/connection_metrics_tests.rs')
     # Pin the complete scoped dependency, including upstream provenance and
     # licenses, even before new vendor files have entered the Git index.
     vendor=BASE/'vendor/quinn-proto-0.11.18'
     if vendor.is_dir():paths.extend(p for p in vendor.rglob('*') if p.is_file())
     paths.append(BASE/'scripts/cargo-shared-env.sh')
     return {str(p.relative_to(BASE)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(set(paths))}
    before=frozen();write('source-before.json',before)
    env=os.environ.copy();env.update({'MOUNT_RS_PROFILE_IO':str(profile),'MOUNT_RS_TRACE_STORAGE':str(trace),'MOUNT_RS_TRACE_REQUESTS':'0','CARGO_TARGET_DIR':os.environ.get('CARGO_TARGET_DIR',os.environ.get('MOUNT_RS_CARGO_TARGET_DIR',str(root/'cargo-target')))})
    if kind in {'cacheconsumers','cacheconsumerred'}:
     # Pure suites forbid loading a real addon or latching native profiling.
     for key in ['MOUNT_RS_PROFILE_IO','MOUNT_RS_TRACE_STORAGE','MOUNT_RS_TRACE_REQUESTS','NAPI_RS_FORCE_WASI','NAPI_RS_WASI_FLAVOR','NODE_PATH']:
      env.pop(key,None)
     env['NAPI_RS_NATIVE_LIBRARY_PATH']=str(BASE/'benchmarks/storage/capture-native.cjs')
    env['TMPDIR']=str(fixture_tmp)
    env.pop('MOUNT_RS_CACHE_REDIS_SERVER',None)
    if redis_pin is not None:env['MOUNT_RS_CACHE_REDIS_SERVER']=redis_pin['path']
    if kind=='wsloss':env['MOUNT_RS_REMOTE_SQLITE_REPLY_LOSS']='1'
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
      selected_test_pass=exact_case_passed(output, selected_test)
      if not selected_test_pass:unknown.append('exact_named_case_not_observed_passed')
     selected_suite=EXPECTED_SUITES.get(kind)
     selected_suite_pass=None
     if selected_suite is not None:
      selected_suite_pass=named_suite_passed((root/'stdout.log').read_text(),selected_suite,nocapture=kind in {'createpath','createunit','filesystemmetrics'})
      if not selected_suite_pass:unknown.append('named_suite_not_observed_passed')
     cache_slow_records=None
     if kind=='cachemetricstrace':
      cache_slow_records=cache_slow_logging_records((root/'stderr.log').read_text(errors='replace'))
      if cache_slow_records is None:unknown.append('cache_slow_logging_not_observed_valid')
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
