#!/usr/bin/env python3
"""Result-gating controls; importing the parent must start no child processes."""
import importlib.util
from pathlib import Path
import unittest

path = Path(__file__).with_name("test-remote-failures.py")
spec = importlib.util.spec_from_file_location("owned_remote_failures", path)
parent = importlib.util.module_from_spec(spec)
spec.loader.exec_module(parent)

NAME = "redis_directory_real_peer_failures_preserve_exact_backing"
SUMMARY = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.42s\n"


class ResultControls(unittest.TestCase):
    def test_single_line_pass(self):
        self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {NAME} ... ok\n\n{SUMMARY}", NAME))

    def test_nocapture_phase_output_between_name_and_ok(self):
        output = f"running 1 test\ntest {NAME} ... phase=healthy-directory backing_gets=0\nphase=peer-outage backing_gets=1\nok\n\n{SUMMARY}"
        self.assertTrue(parent.exact_case_passed(output, NAME))

    def test_wrong_selected_name(self):
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest other ... ok\n{SUMMARY}", NAME))

    def test_zero_cases(self):
        self.assertFalse(parent.exact_case_passed(f"running 0 tests\ntest {NAME} ... ok\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", NAME))

    def test_failed_case(self):
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest {NAME} ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n", NAME))

    def test_ignored_case(self):
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest {NAME} ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored;\n", NAME))

    def test_missing_running_count(self):
        self.assertFalse(parent.exact_case_passed(f"test {NAME} ... ok\n{SUMMARY}", NAME))

    def test_missing_selected_name(self):
        self.assertFalse(parent.exact_case_passed(f"running 1 test\n{SUMMARY}", NAME))

    def test_missing_terminal_summary(self):
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest {NAME} ... ok\n", NAME))

    def test_multi_case_run(self):
        self.assertFalse(parent.exact_case_passed(f"running 2 tests\ntest {NAME} ... ok\ntest other ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored;\n", NAME))

    def test_selected_name_is_literal(self):
        literal = "fixture::name.with.dot"
        self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {literal} ... ok\n{SUMMARY}", literal))
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest fixture::nameXwithXdot ... ok\n{SUMMARY}", literal))


class CacheStageSelectors(unittest.TestCase):
    def test_profiled_exact_cases_have_one_literal_selector(self):
        expected = {
            "cachemetrics": "cache_lookup_stage_metrics_preserve_bytes_and_cancellation",
            "peermetrics": "peer::tests::peer_connection_stage_metrics_preserve_bytes_and_cancellation",
            "storagealloc": "warmed_core_spans_record_without_added_allocations",
            "corealloc": "warmed_causal_profile_rows_record_without_added_allocations",
        }
        for kind, name in expected.items():
            command, limit, profile, trace = parent.COMMANDS[kind]
            self.assertEqual((limit, profile, trace), (180, 1, 0))
            self.assertEqual(parent.EXACT_CASES[kind], name)
            self.assertIn("--ignored", command)
            self.assertEqual(command[command.index("--exact") + 1], name)
            self.assertIn("--test-threads=1", command)
            self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {name} ... behavior_oracles=complete\nok\n{SUMMARY}", name))
            self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest other ... ok\n{SUMMARY}", name))

    def test_ready_future_baselines_are_separate_profile_processes(self):
        for kind, enabled in [("cacheprofileoff", 0), ("cacheprofileon", 1)]:
            command, limit, profile, trace = parent.COMMANDS[kind]
            self.assertEqual((limit, profile, trace), (180, enabled, 0))
            self.assertEqual(command, ["./scripts/cargo-shared", "run", "--locked", "--offline", "-p", "mount-rs-blob-cache", "--example", "cache_profile"])
            self.assertNotIn(kind, parent.EXACT_CASES)

    def test_cache_consumers_are_fixed_node24_pure_fixture_controls(self):
        command, limit, profile, trace = parent.COMMANDS["cacheconsumers"]
        self.assertEqual((limit, profile, trace), (180, 0, 0))
        self.assertEqual(command, ["fnm", "exec", "--using", "v24.18.0", "node", "--test", "benchmarks/storage/test.mjs", "benchmarks/storage/foundationdb-diagnostics.test.mjs", "benchmarks/storage/owned-layout-metrics.test.mjs", "benchmarks/storage/owned-backing-pilot.test.mjs", "scripts/verify-owned-backing-pilot.test.mjs"])
        self.assertNotIn("cacheconsumers", parent.EXACT_CASES)


class CacheDeliverySelectors(unittest.TestCase):
    def test_signed_cli_selector_executes_one_nonignored_case(self):
        command, limit, profile, trace = parent.COMMANDS["clicompact"]
        self.assertEqual((limit, profile, trace), (180, 1, 0))
        self.assertEqual(command[command.index("--features") + 1], "local-oidc-fixture,io-profiling")
        self.assertEqual(command[command.index("--exact") + 1], parent.EXACT_CASES["clicompact"])
        self.assertNotIn("--ignored", command)

    def test_trace_is_separate_from_allocation_and_stage_measurements(self):
        off, limit, profile, trace = parent.COMMANDS["cachemetrics"]
        on, on_limit, on_profile, on_trace = parent.COMMANDS["cachemetricstrace"]
        self.assertEqual(off, on)
        self.assertEqual((limit, profile, trace), (180, 1, 0))
        self.assertEqual((on_limit, on_profile, on_trace), (180, 1, 1))
        self.assertEqual(parent.EXACT_CASES["cachemetrics"], parent.EXACT_CASES["cachemetricstrace"])


class NamedSuiteControls(unittest.TestCase):
    def test_current_cli_suite_requires_every_named_case(self):
        names = parent.EXPECTED_SUITES["clidiagnostics"]
        output = "running 7 tests\n" + "".join(f"test {name} ... ok\n" for name in names) + "test result: ok. 7 passed; 0 failed; 0 ignored;\n"
        self.assertTrue(parent.named_suite_passed(output, names))
        self.assertFalse(parent.named_suite_passed(output.replace(names[0], "other"), names))

    def test_successful_zero_test_run_cannot_qualify_cli_suite(self):
        self.assertFalse(parent.named_suite_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", parent.EXPECTED_SUITES["clidiagnostics"]))

    def test_cli_suite_selector_matches_nested_module(self):
        self.assertEqual(parent.COMMANDS["clidiagnostics"][0][-1], "remote::diagnostics::tests::")
        self.assertIn("--lib", parent.COMMANDS["clidiagnostics"][0])
        self.assertNotIn("--bin", parent.COMMANDS["clidiagnostics"][0])


class CacheSlowLogControls(unittest.TestCase):
    LINE = "MOUNT_RS_STORAGE_SLOW operation=blob_cache.disk.lookup outcome=error elapsed_us=600000\n"

    def test_fixed_disk_deadline_record_is_observed(self):
        self.assertEqual(parent.cache_slow_logging_records("compile status\n"+self.LINE), [{"operation":"blob_cache.disk.lookup", "outcome":"error", "elapsed_us":600000}])

    def test_absent_record_cannot_qualify_logging(self):
        self.assertIsNone(parent.cache_slow_logging_records("test passed\n"))

    def test_private_or_unknown_label_is_rejected(self):
        self.assertIsNone(parent.cache_slow_logging_records(self.LINE+self.LINE.replace("blob_cache.disk.lookup", "private-drive")))

    def test_earlier_slow_flight_burst_can_consume_the_shared_budget(self):
        line = self.LINE.replace("blob_cache.disk.lookup", "blob_cache.miss.singleflight_wait").replace("outcome=error", "outcome=success")
        self.assertEqual(len(parent.cache_slow_logging_records(line*16)), 16)

    def test_budget_and_threshold_are_enforced(self):
        self.assertIsNone(parent.cache_slow_logging_records(self.LINE*17))
        self.assertIsNone(parent.cache_slow_logging_records(self.LINE.replace("600000", "99999")))


class DarwinSignalControls(unittest.TestCase):
    # Missing implementation models the previous sticky-error behavior, so
    # positive controls fail semantically before the portability correction.
    def eligible(self, platform="darwin", action="postterminal_term", terminal=True, errno=1):
        return getattr(parent, "terminal_eperm_eligible", lambda *args: False)(platform, action, terminal, errno)

    def settled(self, reaped=True, absent=True, eof=True, deadline=False, unknown=()):
        return getattr(parent, "terminal_eperm_settled", lambda *args: False)(reaped, absent, eof, deadline, unknown)

    def test_darwin_terminal_term_is_provisional(self):
        self.assertTrue(self.eligible())

    def test_darwin_terminal_kill_is_provisional(self):
        self.assertTrue(self.eligible(action="postterminal_kill"))

    def test_linux_permission_error_stays_failure(self):
        self.assertFalse(self.eligible(platform="linux"))

    def test_live_leader_permission_error_stays_failure(self):
        self.assertFalse(self.eligible(terminal=False))

    def test_preterminal_signal_stays_failure(self):
        self.assertFalse(self.eligible(action="term"))

    def test_other_errno_stays_failure(self):
        self.assertFalse(self.eligible(errno=5))

    def test_all_final_observations_reconcile_empty_group(self):
        self.assertTrue(self.settled())

    def test_missing_reap_stays_failure(self):
        self.assertFalse(self.settled(reaped=False))

    def test_present_group_stays_failure(self):
        self.assertFalse(self.settled(absent=False))

    def test_ambiguous_group_stays_failure(self):
        self.assertFalse(self.settled(absent=None))

    def test_missing_eof_stays_failure(self):
        self.assertFalse(self.settled(eof=False))

    def test_deadline_stays_failure(self):
        self.assertFalse(self.settled(deadline=True))

    def test_other_lifecycle_error_stays_failure(self):
        self.assertFalse(self.settled(unknown=("initial_owned_group_mismatch",)))


if __name__ == "__main__":
    unittest.main()
