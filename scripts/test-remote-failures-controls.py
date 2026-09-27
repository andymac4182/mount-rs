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
