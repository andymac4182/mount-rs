#!/usr/bin/env python3
"""Validate counter classification and fail-closed stage arithmetic offline."""
from pathlib import Path
import runpy
import unittest

ROOT = Path(__file__).resolve().parent
MAC = runpy.run_path(str(ROOT / 'observe-macos-stage.py'))
TIDB = runpy.run_path(str(ROOT / 'observe-tidb-stage.py'))
FDB = runpy.run_path(str(ROOT / 'foundationdb-counter-deltas.py'))

class CounterTests(unittest.TestCase):
    def test_session_counts_are_gauges_and_invalid_samples_fail(self):
        raw = '''# TYPE tidb_server_connections gauge
tidb_server_connections{resource_group="default"} 2000
# TYPE tidb_server_internal_sessions gauge
tidb_server_internal_sessions 12
'''
        gauges = TIDB['parse_session_gauges'](raw)
        self.assertEqual(gauges['tidb_server_connections{resource_group="default"}'], 2000)
        self.assertEqual(gauges['tidb_server_internal_sessions'], 12)
        self.assertNotIn('tidb_server_connections{resource_group="default"}', TIDB['parse_metrics'](raw)[0])
        with self.assertRaises(ValueError):
            TIDB['parse_session_gauges'](raw.replace('2000', 'NaN'))

    def test_memory_gauges_remain_gauges(self):
        raw = '''# TYPE process_resident_memory_bytes gauge
process_resident_memory_bytes 4096
# TYPE go_memstats_heap_alloc_bytes gauge
go_memstats_heap_alloc_bytes 2048
# TYPE process_cpu_seconds_total counter
process_cpu_seconds_total 2
'''
        gauges = TIDB['parse_memory_gauges'](raw)
        self.assertEqual(gauges['process_resident_memory_bytes'], 4096)
        self.assertEqual(gauges['go_memstats_heap_alloc_bytes'], 2048)
        self.assertNotIn('process_cpu_seconds_total', gauges)
    def test_host_delta_and_invalid_boundaries(self):
        keys = MAC['KEYS']
        before = {'captured_monotonic_ns': 1, 'devices': {'drive': dict.fromkeys(keys, 10)}}
        after = {'captured_monotonic_ns': 2_000_000_001, 'devices': {'drive': dict.fromkeys(keys, 20)}}
        self.assertEqual(MAC['reduce'](before, after)['rates_per_second'][keys[0]], 5)
        for invalid in (before, {'captured_monotonic_ns': 3, 'devices': {}},
                        {'captured_monotonic_ns': 3, 'devices': {'drive': dict.fromkeys(keys, 9)}}):
            with self.assertRaises(ValueError):
                MAC['reduce'](before, invalid)

    def test_prometheus_gauge_suffix_is_not_a_counter(self):
        raw = '''# TYPE tikv_futurepool_pending_task_total gauge
 tikv_futurepool_pending_task_total 7
# TYPE tidb_executor_statement_total counter
tidb_executor_statement_total{type="Select"} 12
# TYPE tikv_grpc_msg_duration_seconds histogram
tikv_grpc_msg_duration_seconds_count{type="kv_get"} 9
# TYPE unexplained_count gauge
unexplained_count 4
'''.replace('\n ', '\n')
        measured, _, available = TIDB['parse_metrics'](raw)
        self.assertNotIn('tikv_futurepool_pending_task_total', measured)
        self.assertIn('tikv_futurepool_pending_task_total', available)
        self.assertNotIn('unexplained_count', measured)
        self.assertEqual(measured['tidb_executor_statement_total{type="Select"}'], 12)
        self.assertEqual(measured['tikv_grpc_msg_duration_seconds_count{type="kv_get"}'], 9)

    def test_tidb_restart_and_reset_are_not_valid_deltas(self):
        def source(pid, value, timestamp=10):
            return {'process_pid': pid, 'process_started_at': 'fixed', 'captured_at_unix_ns': timestamp,
                    'metrics': {'sql': value}, 'proc_io': {}, 'cgroup_io': {}}
        before = {'sources': {'owned': source(1, 10)}}
        with self.assertRaises(RuntimeError):
            TIDB['diff'](before, {'sources': {'owned': source(2, 20, 20)}})
        _, resets = TIDB['diff'](before, {'sources': {'owned': source(1, 9, 20)}})
        self.assertTrue(resets)
        with self.assertRaises(RuntimeError):
            TIDB['diff'](before, {'sources': {'owned': source(1, 20, 9)}})

    def test_fdb_gauges_and_counter_resets(self):
        self.assertEqual(FDB['counters']({'reads': {'counter': 10, 'hz': 900}, 'queue': 500}), {'reads': 10})
        sample = FDB['deltas']({'reads': 10}, {'reads': 9}, 2)['reads']
        self.assertTrue(sample['counter_reset'])
        self.assertIsNone(sample['per_second'])
        self.assertIsNone(sample['delta'])

if __name__ == '__main__':
    unittest.main()
