#!/usr/bin/env python3
"""Result-gating controls; importing the parent must start no child processes."""
import ast
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

path = Path(__file__).with_name("test-remote-failures.py")
spec = importlib.util.spec_from_file_location("owned_remote_failures", path)
parent = importlib.util.module_from_spec(spec)
spec.loader.exec_module(parent)

NAME = "redis_directory_real_peer_failures_preserve_exact_backing"
SUMMARY = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.42s\n"


# Independent schema-v2 oracles; never derive these keys or values from the parser.
SQLITE_REPLY_LOSS_PREFIX = b"MOUNT_RS_SQLITE_REPLY_LOSS "
SQLITE_REPLY_LOSS_FIXTURES = {
    "wsloss": {
        "schema_version": 2,
        "connection_selection": "websocket",
        "initial_quic_datagrams": 0,
        "initial_quic_responses": 0,
        "websocket_contact_elapsed_us": 0,
        "initial_quic_settlement_quiet_ms": 100,
        "deferred_credential_issues": 0,
        "protocol_version": 2,
        "client_hellos": 1,
        "denied_partition_hellos": 1,
        "denied_untrusted_tls": 1,
        "drive_permission_denials": 2,
        "write_submissions": 1,
        "completed_write_replies": 1,
        "held_reply": 1,
        "preclose_metadata_checks": 1,
        "preclose_verified_bytes": 65673,
        "preclose_eof": 0,
        "suppressed_response_envelopes": 1,
        "suppressed_response_bytes": 32,
        "completed_write_count": 65673,
        "suppressed_response_messages": 2,
        "downstream_write_response_messages": 0,
        "uncertain_results": 1,
        "fail_closed_followups": 3,
        "replayed_write_submissions": 0,
        "reconnects": 0,
        "quic_datagrams": 0,
        "quic_observation_scope": "after_initial_settlement_through_post_loss_quiet",
        "no_contact_window_ms": 100,
        "oracle_metadata_checks": 1,
        "oracle_verified_bytes": 65673,
        "oracle_size_bytes": 65673,
        "oracle_eof_count": 0,
        "request_cleanup_completed": 1,
        "relay_cleanup_completed": 1,
        "oracle_cleanup_completed": 1,
        "server_close_wait_completed": 1,
        "process_cleanup_observed": 0,
        "directory_retained": 1,
        "directory_removed": 0,
    },
    "wsautoloss": {
        "schema_version": 2,
        "connection_selection": "auto",
        "initial_quic_datagrams": 1,
        "initial_quic_responses": 0,
        "websocket_contact_elapsed_us": 3000000,
        "initial_quic_settlement_quiet_ms": 100,
        "deferred_credential_issues": 1,
        "protocol_version": 2,
        "client_hellos": 1,
        "denied_partition_hellos": 1,
        "denied_untrusted_tls": 1,
        "drive_permission_denials": 2,
        "write_submissions": 1,
        "completed_write_replies": 1,
        "held_reply": 1,
        "preclose_metadata_checks": 1,
        "preclose_verified_bytes": 65673,
        "preclose_eof": 0,
        "suppressed_response_envelopes": 1,
        "suppressed_response_bytes": 32,
        "completed_write_count": 65673,
        "suppressed_response_messages": 2,
        "downstream_write_response_messages": 0,
        "uncertain_results": 1,
        "fail_closed_followups": 3,
        "replayed_write_submissions": 0,
        "reconnects": 0,
        "quic_datagrams": 0,
        "quic_observation_scope": "after_initial_settlement_through_post_loss_quiet",
        "no_contact_window_ms": 100,
        "oracle_metadata_checks": 1,
        "oracle_verified_bytes": 65673,
        "oracle_size_bytes": 65673,
        "oracle_eof_count": 0,
        "request_cleanup_completed": 1,
        "relay_cleanup_completed": 1,
        "oracle_cleanup_completed": 1,
        "server_close_wait_completed": 1,
        "process_cleanup_observed": 0,
        "directory_retained": 1,
        "directory_removed": 0,
    },
}


class SqliteReplyLossRecordControls(unittest.TestCase):
    def fixture(self, kind):
        return SQLITE_REPLY_LOSS_FIXTURES[kind].copy()

    def wire(self, record):
        return SQLITE_REPLY_LOSS_PREFIX + json.dumps(record, separators=(",", ":")).encode() + b"\n"

    def parsed(self, raw, kind):
        parser = getattr(parent, "sqlite_reply_loss_record", lambda *args: None)
        return parser(raw, kind)

    def test_literal_schema_two_records_project_only_their_closed_fields(self):
        for kind in ["wsloss", "wsautoloss"]:
            with self.subTest(kind=kind):
                expected = self.fixture(kind)
                raw = b"Compiling private-package\nwarning: private build path\n" + self.wire(expected) + b"Finished test profile\n"
                self.assertEqual(self.parsed(raw, kind), expected)

    def test_missing_or_malformed_record_is_unavailable(self):
        for kind in ["wsloss", "wsautoloss"]:
            for raw in [b"", b"Compiling private-package\n", SQLITE_REPLY_LOSS_PREFIX + b"{}\n", SQLITE_REPLY_LOSS_PREFIX + b"{invalid}\n"]:
                with self.subTest(kind=kind, raw=raw):
                    self.assertIsNone(self.parsed(raw, kind))

    def test_duplicate_records_do_not_choose_a_favorable_candidate(self):
        for kind in ["wsloss", "wsautoloss"]:
            valid = self.wire(self.fixture(kind))
            malformed = SQLITE_REPLY_LOSS_PREFIX + b"{invalid}\n"
            for raw in [valid + valid, valid + malformed, malformed + valid]:
                with self.subTest(kind=kind, raw=raw):
                    self.assertIsNone(self.parsed(raw, kind))

    def test_private_extra_field_is_rejected(self):
        for kind in ["wsloss", "wsautoloss"]:
            record = self.fixture(kind)
            record["private_path"] = "/private/token.jwt"
            with self.subTest(kind=kind):
                self.assertIsNone(self.parsed(self.wire(record), kind))

    def test_every_required_key_must_be_present(self):
        for kind in ["wsloss", "wsautoloss"]:
            for key in self.fixture(kind):
                record = self.fixture(kind)
                del record[key]
                with self.subTest(kind=kind, omitted=key):
                    self.assertIsNone(self.parsed(self.wire(record), kind))

    def test_fixed_numeric_values_are_exact(self):
        variable = {"initial_quic_datagrams", "websocket_contact_elapsed_us"}
        for kind in ["wsloss", "wsautoloss"]:
            for key, value in self.fixture(kind).items():
                if type(value) is not int or key in variable:
                    continue
                record = self.fixture(kind)
                record[key] = value + 1
                with self.subTest(kind=kind, field=key):
                    self.assertIsNone(self.parsed(self.wire(record), kind))

    def test_numeric_fields_reject_boolean_float_nonfinite_and_nonnumeric_values(self):
        invalid = [True, False, 0.0, 1.5, float("nan"), float("inf"), -float("inf"), None, "1", [], {}]
        for kind in ["wsloss", "wsautoloss"]:
            for key, value in self.fixture(kind).items():
                if type(value) is not int:
                    continue
                for replacement in invalid:
                    record = self.fixture(kind)
                    record[key] = replacement
                    with self.subTest(kind=kind, field=key, replacement=replacement):
                        self.assertIsNone(self.parsed(self.wire(record), kind))

    def test_mode_and_observation_scope_are_exact_strings(self):
        for kind in ["wsloss", "wsautoloss"]:
            for key in ["connection_selection", "quic_observation_scope"]:
                for replacement in [None, True, 1, [], {}, "", "/private/token", self.fixture(kind)[key].upper()]:
                    record = self.fixture(kind)
                    record[key] = replacement
                    with self.subTest(kind=kind, field=key, replacement=replacement):
                        self.assertIsNone(self.parsed(self.wire(record), kind))
        self.assertIsNone(self.parsed(self.wire(self.fixture("wsloss")), "wsautoloss"))
        self.assertIsNone(self.parsed(self.wire(self.fixture("wsautoloss")), "wsloss"))

    def test_auto_probe_count_and_contact_duration_accept_both_closed_bounds(self):
        for datagrams in [1, 32]:
            for contact in [3000000, 30000000]:
                record = self.fixture("wsautoloss")
                record.update(initial_quic_datagrams=datagrams, websocket_contact_elapsed_us=contact)
                with self.subTest(datagrams=datagrams, contact=contact):
                    self.assertEqual(self.parsed(self.wire(record), "wsautoloss"), record)

    def test_auto_probe_count_and_contact_duration_reject_outside_bounds(self):
        for key, values in {
            "initial_quic_datagrams": [-1, 0, 33, 18446744073709551616],
            "websocket_contact_elapsed_us": [-1, 2999999, 30000001, 18446744073709551616],
        }.items():
            for value in values:
                record = self.fixture("wsautoloss")
                record[key] = value
                with self.subTest(field=key, value=value):
                    self.assertIsNone(self.parsed(self.wire(record), "wsautoloss"))

    def test_explicit_websocket_has_no_probe_and_its_own_contact_bounds(self):
        for contact in [0, 30000000]:
            record = self.fixture("wsloss")
            record["websocket_contact_elapsed_us"] = contact
            self.assertEqual(self.parsed(self.wire(record), "wsloss"), record)
        for key, values in {"initial_quic_datagrams": [-1, 1, 32], "websocket_contact_elapsed_us": [-1, 30000001]}.items():
            for value in values:
                record = self.fixture("wsloss")
                record[key] = value
                with self.subTest(field=key, value=value):
                    self.assertIsNone(self.parsed(self.wire(record), "wsloss"))

    def test_duplicate_json_keys_and_nonobject_json_are_rejected(self):
        for kind in ["wsloss", "wsautoloss"]:
            valid = self.wire(self.fixture(kind))
            duplicate = valid.replace(b'{"schema_version":2,', b'{"schema_version":2,"schema_version":2,', 1)
            self.assertNotEqual(duplicate, valid)
            for raw in [duplicate, self.wire([self.fixture(kind)]), self.wire(None), self.wire(True), self.wire(2), self.wire("private text")]:
                with self.subTest(kind=kind, raw=raw):
                    self.assertIsNone(self.parsed(raw, kind))

    def test_candidates_require_the_exact_prefix_and_complete_physical_lf_line(self):
        for kind in ["wsloss", "wsautoloss"]:
            valid = self.wire(self.fixture(kind))
            malformed = [
                valid[:-1], valid[:-1] + b"\r\n", valid[:-1] + b"\r", valid[:-1] + b"\v",
                valid[:-1] + b"\f", valid[:-1] + "\u2028".encode(), valid[:-1] + "\u2029".encode(),
                b"private prefix " + valid, b" " + valid, b"\x1b[31m" + valid,
                valid[:-1] + b" trailing private text\n", valid[:-1] + b"\xff\n",
                valid[:100] + b"\n" + valid[100:],
                valid + b"warning: MOUNT_RS_SQLITE_REPLY_LOSS was quoted\n",
                b"warning: MOUNT_RS_SQLITE_REPLY_LOSS was quoted\n" + valid,
                valid + SQLITE_REPLY_LOSS_PREFIX + b"{",
            ]
            for raw in malformed:
                with self.subTest(kind=kind, raw=raw):
                    self.assertIsNone(self.parsed(raw, kind))

    def test_oversized_physical_candidate_is_unavailable(self):
        for kind in ["wsloss", "wsautoloss"]:
            raw = self.wire(self.fixture(kind))[:-1] + b" " * 4194304 + b"\n"
            with self.subTest(kind=kind):
                self.assertIsNone(self.parsed(raw, kind))

    def test_unknown_selector_cannot_project_a_valid_record(self):
        valid = self.wire(self.fixture("wsloss"))
        for kind in ["unknown", "remotetests", "/private/token", "WSLOSS"]:
            with self.subTest(kind=kind):
                self.assertIsNone(self.parsed(valid, kind))


class SqliteReplyLossGateControls(unittest.TestCase):
    fixture = SqliteReplyLossRecordControls.fixture
    wire = SqliteReplyLossRecordControls.wire
    parsed = SqliteReplyLossRecordControls.parsed
    parsed = SqliteReplyLossRecordControls.parsed

    def gated(self, kind, raw, unknown):
        gate = getattr(parent, "sqlite_reply_loss_gate_record", lambda *args: None)
        return gate(kind, raw, unknown)

    def test_invalid_record_adds_one_fixed_sticky_failure(self):
        for kind in ["wsloss", "wsautoloss"]:
            unknown = ["owned_group_unsettled"]
            self.assertIsNone(self.gated(kind, b"", unknown))
            self.assertIsNone(self.gated(kind, b"malformed output\n", unknown))
            self.assertEqual(unknown, ["owned_group_unsettled", "sqlite_reply_loss_not_observed_valid"])

    def test_bounded_deeply_nested_numeric_value_is_unavailable_and_sticky(self):
        nested = b"[" * 1100 + b"0" + b"]" * 1100
        for kind in ["wsloss", "wsautoloss"]:
            raw = self.wire(self.fixture(kind)).replace(b'"schema_version":2', b'"schema_version":' + nested, 1)
            with self.subTest(kind=kind):
                self.assertLessEqual(len(raw), 4096)
                self.assertIsNone(self.parsed(raw, kind))
                unknown = []
                self.assertIsNone(self.gated(kind, raw, unknown))
                self.assertEqual(unknown, ["sqlite_reply_loss_not_observed_valid"])

    def test_valid_record_preserves_prior_failure(self):
        for kind in ["wsloss", "wsautoloss"]:
            unknown = ["owned_group_unsettled"]
            expected = self.fixture(kind)
            self.assertEqual(self.gated(kind, self.wire(expected), unknown), expected)
            self.assertEqual(unknown, ["owned_group_unsettled"])

    def test_later_valid_record_never_clears_invalid_record_failure(self):
        for kind in ["wsloss", "wsautoloss"]:
            unknown = []
            self.assertIsNone(self.gated(kind, b"", unknown))
            expected = self.fixture(kind)
            self.assertEqual(self.gated(kind, self.wire(expected), unknown), expected)
            self.assertEqual(unknown, ["sqlite_reply_loss_not_observed_valid"])

    def test_unrelated_kind_skips_parser_and_preserves_unknown(self):
        for kind in ["remotetests", "unknown", "/private/token"]:
            unknown = ["owned_group_unsettled"]
            with patch.object(parent, "sqlite_reply_loss_record", create=True, side_effect=AssertionError("unrelated kind must not parse")):
                self.assertIsNone(self.gated(kind, self.wire(self.fixture("wsloss")), unknown))
            self.assertEqual(unknown, ["owned_group_unsettled"])


class SqliteReplyLossSelectors(unittest.TestCase):
    def test_each_loss_selector_executes_one_serial_ignored_unprofiled_case(self):
        expected = {
            "wsloss": "websocket_sqlite_commit_survives_lost_wire_reply_without_replay",
            "wsautoloss": "automatic_fallback_sqlite_commit_survives_lost_wire_reply_without_replay",
        }
        for kind, name in expected.items():
            with self.subTest(kind=kind):
                command, limit, profile, trace = parent.COMMANDS[kind]
                self.assertEqual((limit, profile, trace), (180, 0, 0))
                self.assertEqual(command, ["./scripts/cargo-shared", "test", "-p", "mount-rs-remote-client", "--test", "quic_mount", "--locked", "--offline", "--", "--ignored", "--exact", name, "--test-threads=1", "--nocapture"])
                self.assertEqual(parent.EXACT_CASES[kind], name)
                valid = f"running 1 test\ntest {name} ... receipt observed\nok\n{SUMMARY}"
                self.assertTrue(parent.exact_case_passed(valid, name))
                other = expected["wsautoloss" if kind == "wsloss" else "wsloss"]
                self.assertFalse(parent.exact_case_passed(valid.replace(name, other), name))
                self.assertFalse(parent.exact_case_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", name))
                self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest {name} ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored;\n", name))

    def test_reply_loss_environment_gate_is_static_and_scoped_to_both_selectors(self):
        tree = ast.parse(path.read_text())
        gates = []
        service_trace_gates = []
        for node in ast.walk(tree):
            if not isinstance(node, ast.If) or not isinstance(node.test, ast.Compare):
                continue
            comparison = node.test
            if not (isinstance(comparison.left, ast.Name) and comparison.left.id == "kind"
                    and len(comparison.ops) == 1 and isinstance(comparison.ops[0], ast.In)
                    and len(comparison.comparators) == 1 and isinstance(comparison.comparators[0], ast.Set)):
                continue
            kinds = {entry.value for entry in comparison.comparators[0].elts if isinstance(entry, ast.Constant)}
            for assignment in node.body:
                if not isinstance(assignment, ast.Assign) or len(assignment.targets) != 1:
                    continue
                target = assignment.targets[0]
                if (isinstance(target, ast.Subscript) and isinstance(target.value, ast.Name)
                        and target.value.id == "env" and isinstance(target.slice, ast.Constant)
                        and target.slice.value == "MOUNT_RS_REMOTE_SQLITE_REPLY_LOSS"):
                    gates.append((kinds, ast.literal_eval(assignment.value)))
                if (isinstance(target, ast.Subscript) and isinstance(target.value, ast.Name)
                        and target.value.id == "env" and isinstance(target.slice, ast.Constant)
                        and target.slice.value == "MOUNT_RS_TRACE_SERVICE"):
                    service_trace_gates.append((kinds, ast.literal_eval(assignment.value)))
        self.assertEqual(gates, [({"wsloss", "wsautoloss"}, "1")])
        self.assertTrue(any({"wsloss", "wsautoloss"} <= kinds and value == "0" for kinds, value in service_trace_gates))


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
    def test_websocket_general_suite_keeps_process_recorder_off_in_both_builds(self):
        for kind in ['wspackages', 'wspackagesprofiled']:
            command, limit, profile, trace = parent.COMMANDS[kind]
            self.assertEqual((limit, profile, trace), (180, 0, 0))
            self.assertIn('--all-targets', command)
            self.assertNotIn('--ignored', command)
        self.assertNotIn('--features', parent.COMMANDS['wspackages'][0])
        self.assertEqual(parent.COMMANDS['wspackagesprofiled'][0][parent.COMMANDS['wspackagesprofiled'][0].index('--features') + 1], 'mount-rs-service/io-profiling,mount-rs-cli/io-profiling')

    def test_websocket_service_suites_reject_partial_or_zero_runs(self):
        for kind, profile, count in [('wsservicedefault', 0, 2), ('wsserviceon', 1, 8)]:
            command, limit, observed_profile, trace = parent.COMMANDS[kind]
            self.assertEqual((limit, observed_profile, trace), (180, profile, 0))
            self.assertEqual(command[command.index('--test') + 1], 'websocket_diagnostics')
            self.assertIn('--test-threads=1', command)
            self.assertNotIn('--ignored', command)
            names = parent.EXPECTED_SUITES[kind]
            self.assertEqual(len(names), count)
            output = f'running {count} tests\n' + ''.join(f'test {name} ... ok\n' for name in names) + f'test result: ok. {count} passed; 0 failed; 0 ignored;\n'
            self.assertTrue(parent.named_suite_passed(output, names))
            self.assertFalse(parent.named_suite_passed(output.replace(names[0], 'unrelated'), names))
            self.assertFalse(parent.named_suite_passed('running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n', names))

    def test_lazy_runtime_literal_selectors_require_every_named_case(self):
        for kind, count in [('lazycli', 12), ('lazyservice', 23)]:
            with self.subTest(kind=kind):
                self.assertIn(kind, parent.COMMANDS)
                command, limit, profile, trace = parent.COMMANDS[kind]
                self.assertEqual((limit, profile, trace), (180, 1, 0))
                self.assertIn('--locked', command)
                self.assertIn('--offline', command)
                self.assertIn('--test-threads=1', command)
                self.assertNotIn('--ignored', command)
                names = parent.EXPECTED_SUITES[kind]
                self.assertEqual(len(names), count)
                if kind == 'lazycli':
                    self.assertIn('::lazy_', command)
                    groups = [names]
                else:
                    self.assertEqual(command.count('--test'), 3)
                    groups = [names[:8], names[8:17], names[17:]]
                output = ''.join(f'running {len(group)} tests\n' + ''.join(f'test {name} ... controlled output\nok\n' for name in group) + f'test result: ok. {len(group)} passed; 0 failed; 0 ignored;\n' for group in groups)
                self.assertTrue(parent.lazy_suite_passed(output, kind))
                self.assertFalse(parent.lazy_suite_passed(output.replace(names[0], 'unrelated'), kind))
                self.assertFalse(parent.lazy_suite_passed(output.replace('0 failed', '1 failed', 1), kind))
                self.assertFalse(parent.lazy_suite_passed(output+f'test {names[0]} ... ok\n', kind))
                self.assertFalse(parent.lazy_suite_passed(output.replace('running 8 tests', 'running 7 tests'), kind) if kind == 'lazyservice' else parent.lazy_suite_passed('running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n', kind))

    def test_websocket_client_metrics_requires_one_profiled_case(self):
        name = "websocket::metrics_tests::websocket_stages_preserve_serialized_transactions"
        command, limit, profile, trace = parent.COMMANDS["wsclientmetrics"]
        self.assertEqual(parent.EXACT_CASES["wsclientmetrics"], name)
        self.assertEqual(command[command.index("--exact") + 1], name)
        self.assertIn("--ignored", command)
        self.assertIn("--test-threads=1", command)
        self.assertEqual((limit, profile, trace), (180, 1, 0))

    def test_persistent_sqlite_cache_has_one_exact_profiled_case(self):
        name = "sqlite_backed_authenticated_cache_failures_preserve_exact_backing_savings"
        command, limit, profile, trace = parent.COMMANDS["sqlitecache"]
        self.assertEqual(parent.EXACT_CASES["sqlitecache"], name)
        self.assertEqual(command[command.index("--exact") + 1], name)
        self.assertIn("--ignored", command)
        self.assertIn("--test-threads=1", command)
        self.assertEqual(command[command.index("--test") + 1], "distributed_failure")
        self.assertEqual((limit, profile, trace), (180, 1, 0))

    def test_peer_request_stages_require_one_profiled_executed_case(self):
        name = "peer::tests::peer_request_stage_metrics_preserve_bytes_outcomes_and_cancellation"
        command, limit, profile, trace = parent.COMMANDS["peeriometrics"]
        self.assertEqual((limit, profile, trace), (180, 1, 0))
        self.assertEqual(command, ["./scripts/cargo-shared", "test", "-p", "mount-rs-blob-cache", "--lib", "--locked", "--offline", "--", "--ignored", "--exact", name, "--test-threads=1", "--nocapture"])
        self.assertEqual(parent.EXACT_CASES["peeriometrics"], name)
        self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {name} ... behavior_oracles=complete\nok\n{SUMMARY}", name))
        self.assertFalse(parent.exact_case_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", name))

    def test_filesystem_metrics_require_the_complete_profiled_ci_suite(self):
        command, limit, profile, trace = parent.COMMANDS["filesystemmetrics"]
        self.assertEqual((limit, profile, trace), (180, 1, 0))
        self.assertEqual(command, ["./scripts/cargo-shared", "test", "-p", "mount-rs-chunked", "--test", "filesystem_causal_metrics", "--locked", "--offline", "--", "--ignored", "--test-threads=1", "--nocapture"])
        names = parent.EXPECTED_SUITES["filesystemmetrics"]
        self.assertEqual(len(names), 12)
        self.assertEqual(len(set(names)), 12)
        output = "running 12 tests\n" + "".join(f"test {name} ... phase=observed\nok\n" for name in names) + "test result: ok. 12 passed; 0 failed; 0 ignored;\n"
        self.assertTrue(parent.named_suite_passed(output, names, nocapture=True))
        self.assertFalse(parent.named_suite_passed(output.replace(names[0], "other"), names, nocapture=True))
        self.assertFalse(parent.named_suite_passed(output.replace("12 passed; 0 failed", "11 passed; 1 failed"), names, nocapture=True))
        self.assertFalse(parent.named_suite_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", names, nocapture=True))

    def test_client_stream_metrics_require_one_profiled_executed_case(self):
        name = "connection::metrics_tests::quic_stream_acquisition_outcomes_preserve_transactions"
        command, limit, profile, trace = parent.COMMANDS["clientmetrics"]
        self.assertEqual((limit, profile, trace), (180, 1, 0))
        self.assertEqual(command, ["./scripts/cargo-shared", "test", "-p", "mount-rs-remote-client", "--lib", "--locked", "--offline", "--", "--ignored", "--exact", name, "--test-threads=1", "--nocapture"])
        self.assertEqual(parent.EXACT_CASES["clientmetrics"], name)
        self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {name} ... behavior_oracles=complete\nok\n{SUMMARY}", name))
        self.assertFalse(parent.exact_case_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", name))
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest {name} ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored;\n", name))

    def test_create_qualification_requires_complete_executed_controls(self):
        for kind, count in [("createpath", 5), ("createunit", 4)]:
            names = parent.EXPECTED_SUITES[kind]
            self.assertEqual(len(names), count)
            valid = f"running {count} tests\n"+"".join(f"test {name} ... ok\n" for name in names)+f"test result: ok. {count} passed; 0 failed; 0 ignored;\n"
            self.assertTrue(parent.named_suite_passed(valid, names))
            self.assertFalse(parent.named_suite_passed(valid.replace(names[0], "unrelated"), names))
            self.assertFalse(parent.named_suite_passed(valid.replace(f"{count} passed", f"{count-1} passed"), names))
            split = valid.replace(f"test {names[0]} ... ok", f"test {names[0]} ... FIXED_STAGE\nok")
            self.assertFalse(parent.named_suite_passed(split, names))
            self.assertTrue(parent.named_suite_passed(split, names, nocapture=True))
            self.assertFalse(parent.named_suite_passed(split.replace(names[0], "unrelated"), names, nocapture=True))
            self.assertFalse(parent.named_suite_passed(split+f"test {names[0]} ... ok\n", names, nocapture=True))
            self.assertFalse(parent.named_suite_passed(split.replace(f"{count} passed", f"{count-1} passed"), names, nocapture=True))
        for kind, name in [
            ("createprep", "missing_compact_create_preparation_avoids_full_scan_with_128_siblings"),
            ("createguard", "unchanged_compact_refresh_preserves_prepared_create"),
        ]:
            command, limit, profile, trace = parent.COMMANDS[kind]
            self.assertEqual((limit, profile, trace), (180, 1, 0))
            self.assertEqual(parent.EXACT_CASES[kind], name)
            self.assertEqual(command[command.index("--exact")+1], name)

    def test_setup_and_discovery_commands_are_fixed_exact_enabled_and_bounded(self):
        for kind, case, target in [
            ("clientsetupmetrics", "connection::metrics_tests::quic_connection_setup_metrics_preserve_outcomes", "--lib"),
            ("discoverymetrics", "discovery_locate_metrics_preserve_bytes_outcomes_and_cancellation", "discovery_metrics"),
        ]:
            command, limit, profile, trace = parent.COMMANDS[kind]
            self.assertEqual((limit, profile, trace), (180, 1, 0))
            self.assertIn(target, command)
            self.assertEqual(parent.EXACT_CASES[kind], case)
            self.assertIn("--ignored", command)
            self.assertIn("--exact", command)
            self.assertIn("--locked", command)
            self.assertIn("--offline", command)
            self.assertIn(case, command)
            self.assertFalse(parent.exact_case_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", case))

    def test_quinn_close_regressions_require_the_selected_executed_case(self):
        expected = {
            "quinnordinary": ("tests::ordinary_initial_then_close_drains_without_waiting_for_idle_timeout", False),
            "quinnclose": ("tests::first_close_initial_drains_without_waiting_for_idle_timeout", False),
            "quinnruntimeclose": ("tests::first_close_initial_releases_real_endpoint_within_close_grace", True),
        }
        for kind, (name, ignored) in expected.items():
            command, limit, profile, trace = parent.COMMANDS[kind]
            self.assertEqual((limit, profile, trace), (180, 0, 0))
            self.assertEqual(command[command.index("--test") + 1], "quinn_close")
            self.assertEqual(command[command.index("--exact") + 1], name)
            self.assertEqual("--ignored" in command, ignored)
            self.assertEqual(parent.EXACT_CASES[kind], name)
            self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {name} ... ok\n{SUMMARY}", name))
            self.assertFalse(parent.exact_case_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", name))
            self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest {name} ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n", name))

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

    def test_peer_reconnect_is_one_profiled_serial_integration_case(self):
        name = "peer_read_reconnect_bypasses_pending_replica_handshake"
        command, limit, profile, trace = parent.COMMANDS["peerreconnect"]
        self.assertEqual((limit, profile, trace), (180, 1, 0))
        self.assertEqual(command, ["./scripts/cargo-shared", "test", "-p", "mount-rs-blob-cache", "--test", "distributed_failure", "--locked", "--offline", "--", "--ignored", "--exact", name, "--test-threads=1", "--nocapture"])
        self.assertEqual(parent.EXACT_CASES["peerreconnect"], name)
        self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {name} ... behavior_oracles=complete\nok\n{SUMMARY}", name))
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest other ... ok\n{SUMMARY}", name))
        self.assertFalse(parent.exact_case_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", name))

    def test_udp_release_requires_one_unprofiled_real_socket_case(self):
        name = "stopped_peer_udp_rebind_waits_for_real_driver_release"
        command, limit, profile, trace = parent.COMMANDS["peerport"]
        self.assertEqual((limit, profile, trace), (180, 0, 0))
        self.assertEqual(command, ["./scripts/cargo-shared", "test", "-p", "mount-rs-blob-cache", "--test", "distributed_failure", "--locked", "--offline", "--", "--ignored", "--exact", name, "--test-threads=1", "--nocapture"])
        self.assertEqual(parent.EXACT_CASES["peerport"], name)
        self.assertTrue(parent.exact_case_passed(f"running 1 test\ntest {name} ... real_udp_oracles=complete\nok\n{SUMMARY}", name))
        self.assertFalse(parent.exact_case_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n", name))
        self.assertFalse(parent.exact_case_passed(f"running 1 test\ntest {name} ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n", name))

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
        self.assertEqual(len(names), 8)
        output = "running 8 tests\n" + "".join(f"test {name} ... ok\n" for name in names) + "test result: ok. 8 passed; 0 failed; 0 ignored;\n"
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

    def test_new_peer_stage_fixed_labels_and_outcomes_are_observed(self):
        names = ['blob_cache.peer.request_byte_admission_wait', 'blob_cache.peer.open_bi',
                 'blob_cache.peer.request_send', 'blob_cache.peer.response_receive',
                 'blob_cache.peer.get', 'blob_cache.peer.get_miss']
        for name in names:
            for outcome in ['success', 'error', 'cancelled']:
                line = f'MOUNT_RS_STORAGE_SLOW operation={name} outcome={outcome} elapsed_us=2000000\n'
                self.assertEqual(parent.cache_slow_logging_records(line),
                                 [{'operation':name, 'outcome':outcome, 'elapsed_us':2000000}])

    def test_mixed_old_and_new_labels_preserve_the_shared_budget(self):
        peer = self.LINE.replace('blob_cache.disk.lookup', 'blob_cache.peer.get')
        self.assertEqual(len(parent.cache_slow_logging_records((self.LINE + peer) * 8)), 16)
        self.assertIsNone(parent.cache_slow_logging_records((self.LINE + peer) * 8 + peer))

    def test_similar_private_peer_label_is_rejected(self):
        self.assertIsNone(parent.cache_slow_logging_records(self.LINE.replace(
            'blob_cache.disk.lookup', 'blob_cache.peer.get.private-drive')))

    def test_real_peer_trace_requires_quota_cancellation_and_inclusive_error(self):
        quota = self.LINE.replace('blob_cache.disk.lookup', 'blob_cache.peer.request_byte_admission_wait').replace('outcome=error', 'outcome=cancelled')
        get = self.LINE.replace('blob_cache.disk.lookup', 'blob_cache.peer.get')
        self.assertEqual(len(parent.peer_slow_logging_records(quota + get)), 2)
        self.assertIsNone(parent.peer_slow_logging_records(quota))
        self.assertIsNone(parent.peer_slow_logging_records(get))
        self.assertIsNone(parent.peer_slow_logging_records(quota + get.replace('outcome=error', 'outcome=success')))

    def test_peer_trace_selector_preserves_one_serial_real_case(self):
        off, off_limit, off_profile, off_trace = parent.COMMANDS['peeriometrics']
        on, on_limit, on_profile, on_trace = parent.COMMANDS['peeriometricstrace']
        self.assertEqual(on, off)
        self.assertEqual((off_limit, off_profile, off_trace), (180, 1, 0))
        self.assertEqual((on_limit, on_profile, on_trace), (180, 1, 1))
        self.assertEqual(parent.EXACT_CASES['peeriometricstrace'], parent.EXACT_CASES['peeriometrics'])

    def test_unterminated_fixed_record_cannot_qualify_logging(self):
        self.assertIsNone(parent.cache_slow_logging_records(self.LINE.rstrip('\n')))
        self.assertIsNone(parent.cache_slow_logging_records(self.LINE + self.LINE.rstrip('\n')))

    def test_alternate_separators_cannot_complete_a_fixed_record(self):
        for separator in ['\r', '\r\n', '\v', '\f', '\x85', '\u2028', '\u2029']:
            malformed = self.LINE.rstrip('\n') + separator
            self.assertIsNone(parent.cache_slow_logging_records(malformed))
            self.assertIsNone(parent.cache_slow_logging_records(self.LINE + malformed))

    def test_raw_cache_file_preserves_physical_record_terminators(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'stderr.log'
            path.write_bytes(self.LINE.encode())
            self.assertEqual(len(parent.slow_logging_file_records(path)), 1)
            for separator in [b'', b'\r', b'\r\n', b'\v', b'\f', b'\xc2\x85', b'\xe2\x80\xa8', b'\xe2\x80\xa9']:
                path.write_bytes(self.LINE.rstrip('\n').encode() + separator)
                self.assertIsNone(parent.slow_logging_file_records(path))

    def test_raw_peer_file_preserves_both_required_record_terminators(self):
        import tempfile
        from pathlib import Path
        quota = self.LINE.replace('blob_cache.disk.lookup', 'blob_cache.peer.request_byte_admission_wait').replace('outcome=error', 'outcome=cancelled').encode()
        get = self.LINE.replace('blob_cache.disk.lookup', 'blob_cache.peer.get').encode()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'stderr.log'
            path.write_bytes(quota + get)
            self.assertEqual(len(parent.slow_logging_file_records(path, peer=True)), 2)
            for separator in [b'', b'\r', b'\r\n', b'\v', b'\f', b'\xc2\x85', b'\xe2\x80\xa8', b'\xe2\x80\xa9']:
                path.write_bytes(quota + get.rstrip(b'\n') + separator)
                self.assertIsNone(parent.slow_logging_file_records(path, peer=True))
                path.write_bytes(quota.rstrip(b'\n') + separator + get)
                self.assertIsNone(parent.slow_logging_file_records(path, peer=True))

    def test_raw_file_invalid_utf8_cannot_supply_a_recognized_frame(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'stderr.log'
            path.write_bytes(self.LINE.rstrip('\n').encode() + b'\xff\n')
            self.assertIsNone(parent.slow_logging_file_records(path))

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


class GateFailureDiagnosticControls(unittest.TestCase):
    def receipt(self, **changes):
        receipt = {
            "returncode": 101, "owned_child_reaped": True,
            "owned_group_absent": True, "pipes_eof": True,
            "signal_decisions_finished": True, "deadline_exceeded": False,
            "sticky_unknown": [], "source_unchanged": True,
            "primary_failure": None,
            "logs": {"stdout": {"overflow": False}, "stderr": {"overflow": False}},
        }
        receipt.update(changes)
        return receipt

    def expected(self, failure_class="unclassified", **changes):
        diagnostic = {
            "kind": "storagealloc", "failure_class": failure_class,
            "returncode": 101, "deadline_exceeded": False,
            "lifecycle_unsettled": False, "sticky_unknown": False,
            "source_changed": False, "output_overflow": False,
        }
        diagnostic.update(changes)
        return diagnostic

    def diagnostic(self, stdout=b"", stderr=b"", receipt=None, kind="storagealloc"):
        classifier = getattr(parent, "gate_failure_diagnostic", lambda *args: None)
        return classifier(kind, self.receipt() if receipt is None else receipt, stdout, stderr)

    def test_offline_download_requires_both_observed_markers(self):
        for reason in [b"--offline was specified", b"attempting to make an HTTP request"]:
            with self.subTest(reason=reason):
                self.assertEqual(self.diagnostic(stderr=b"error: failed to download private-package\n"+reason), self.expected("offline_download"))
        for text in [b"failed to download private-package", b"--offline was specified", b"attempting to make an HTTP request"]:
            with self.subTest(text=text):
                self.assertEqual(self.diagnostic(stderr=text), self.expected())

    def test_offline_resolution_requires_both_observed_markers(self):
        self.assertEqual(self.diagnostic(stderr=b"error: no matching package named private-package found\noffline mode"), self.expected("offline_resolution"))
        self.assertEqual(self.diagnostic(stderr=b"no matching package named private-package"), self.expected())
        self.assertEqual(self.diagnostic(stderr=b"offline mode"), self.expected())

    def test_compile_failure_uses_the_known_cargo_marker(self):
        self.assertEqual(self.diagnostic(stderr=b"error: could not compile private-crate due to 1 previous error"), self.expected("compile_failed"))

    def test_exact_failed_case_requires_one_selected_case_and_failed_summary(self):
        name = "warmed_core_spans_record_without_added_allocations"
        output = f"running 1 test\ntest {name} ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n".encode()
        self.assertEqual(self.diagnostic(stdout=output), self.expected("exact_test_failed"))
        self.assertEqual(self.diagnostic(stdout=output.replace(name.encode(), b"other_case")), self.expected())
        self.assertEqual(self.diagnostic(stdout=output.replace(b"running 1 test\n", b"")), self.expected())
        self.assertEqual(self.diagnostic(stdout=output.split(b"test result:")[0]), self.expected())

    def test_zero_cases_requires_the_observed_run_and_summary(self):
        output = b"running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n"
        self.assertEqual(self.diagnostic(stdout=output), self.expected("zero_tests"))
        self.assertEqual(self.diagnostic(stdout=b"running 0 tests\n"), self.expected())

    def test_class_precedence_is_fixed(self):
        stderr = b"could not compile private-crate\nno matching package named private-package\noffline\nfailed to download private-package\n--offline"
        self.assertEqual(self.diagnostic(stderr=stderr), self.expected("offline_download"))

    def test_lifecycle_deadline_and_unknown_are_separate_receipt_flags(self):
        receipt = self.receipt(returncode=None, owned_child_reaped=False, owned_group_absent=False, pipes_eof=False, signal_decisions_finished=False, deadline_exceeded=True, sticky_unknown=["/private/token-path"], source_unchanged=False, logs={"stdout": {"overflow": True}, "stderr": {"overflow": False}})
        self.assertEqual(self.diagnostic(receipt=receipt), self.expected(returncode=None, deadline_exceeded=True, lifecycle_unsettled=True, sticky_unknown=True, source_changed=True, output_overflow=True))

    def test_unfinished_signal_decisions_are_lifecycle_unsettled(self):
        self.assertEqual(self.diagnostic(receipt=self.receipt(returncode=0, signal_decisions_finished=False)), self.expected(returncode=0, lifecycle_unsettled=True))

    def test_primary_parent_failure_emits_without_copying_exception_fields(self):
        receipt = self.receipt(returncode=0, primary_failure={"type": "/private/secret-exception", "errno": 13})
        self.assertEqual(self.diagnostic(receipt=receipt), self.expected(returncode=0))

    def test_zero_child_status_does_not_hide_changed_source_or_overflow(self):
        receipt = self.receipt(returncode=0, source_unchanged=False, logs={"stdout": {"overflow": False}, "stderr": {"overflow": True}})
        self.assertEqual(self.diagnostic(receipt=receipt), self.expected(returncode=0, source_changed=True, output_overflow=True))

    def test_numeric_signal_status_is_retained_and_non_numeric_status_is_unknown(self):
        self.assertEqual(self.diagnostic(receipt=self.receipt(returncode=-9)), self.expected(returncode=-9))
        for value in [None, "private-status", False]:
            with self.subTest(value=value):
                self.assertEqual(self.diagnostic(receipt=self.receipt(returncode=value)), self.expected(returncode=None))

    def test_first_and_last_samples_exclude_middle_only_markers(self):
        marker = b"error: failed to download private-package\n--offline was specified\n"
        middle = b"x"*4096+marker+b"y"*4096
        self.assertEqual(self.diagnostic(stderr=middle), self.expected())
        self.assertEqual(self.diagnostic(stderr=marker+b"x"*8192), self.expected("offline_download"))
        self.assertEqual(self.diagnostic(stderr=b"x"*8192+marker), self.expected("offline_download"))

    def test_secret_and_path_markers_project_only_closed_fields(self):
        stdout = b"/private/secret-file eyJ.private-token.secret https://private-host/path\n"
        stderr = b"error: failed to download /private/secret-package\n--offline eyJ.private-token.secret"
        self.assertEqual(self.diagnostic(stdout=stdout, stderr=stderr), self.expected("offline_download"))

    def test_successful_gate_has_no_diagnostic(self):
        self.assertIsNone(self.diagnostic(stderr=b"could not compile historical-message", receipt=self.receipt(returncode=0)))

    def test_unknown_kind_cannot_be_copied_to_a_diagnostic(self):
        self.assertIsNone(self.diagnostic(stderr=b"failed to download private-package --offline", kind="/private/token-kind"))


class PeerProgressDiagnosticControls(unittest.TestCase):
    receipt = GateFailureDiagnosticControls.receipt
    expected = GateFailureDiagnosticControls.expected
    diagnostic = GateFailureDiagnosticControls.diagnostic

    def peer(self, stdout=b"", stderr=b"", receipt=None):
        return self.diagnostic(stdout, stderr, receipt, kind="peerreconnect")

    def expected_peer(self, marker="unobserved", window="unobserved", **changes):
        return self.expected(kind="peerreconnect", last_sampled_progress_marker=marker,
                             progress_sample_window=window, **changes)

    def test_complete_fixed_markers_retain_stream_order(self):
        output = b"peer_read_reconnect_progress=after_requester_shutdown\npeer_read_reconnect_progress=after_server_shutdown\n"
        self.assertEqual(self.peer(output), self.expected_peer("after_server_shutdown", "short_input"))

    def test_first_marker_can_follow_the_exact_libtest_header(self):
        output = b"test peer_read_reconnect_bypasses_pending_replica_handshake ... peer_read_reconnect_progress=before_stop_a\n"
        self.assertEqual(self.peer(output), self.expected_peer("before_stop_a", "short_input"))
        self.assertEqual(self.peer(output.replace(b"bypasses_pending_replica_handshake", b"unrelated")), self.expected_peer())

    def test_only_complete_newline_delimited_markers_are_observed(self):
        for output in [b"peer_read_reconnect_progress=before_stop_a", b"private-prefix peer_read_reconnect_progress=before_stop_a\n"]:
            self.assertEqual(self.peer(output), self.expected_peer())

    def test_malformed_marker_has_a_fixed_invalid_state(self):
        for marker in [b"/private/token", b"before_stop_a private-data", b"before_stop_a\x1b[31m", b"before_stop_a\xff"]:
            output=b"peer_read_reconnect_progress=before_stop_a\npeer_read_reconnect_progress="+marker+b"\n"
            self.assertEqual(self.peer(output), self.expected_peer("invalid", "short_input"))
            self.assertEqual(self.peer(output+b"peer_read_reconnect_progress=after_stop_a\n"), self.expected_peer("invalid", "short_input"))

    def test_samples_cannot_join_or_complete_boundary_markers(self):
        head=b"x"*(2047-len(b"peer_read_reconnect_progress=before_stop_a"))+b"\npeer_read_reconnect_progress=before_stop_a"
        tail=b"\npeer_read_reconnect_progress=after_stop_a\n"+b"y"*3000
        self.assertEqual(self.peer(head+tail), self.expected_peer())
        # The valid-looking suffix starts at an unknown tail boundary and is
        # excluded; no synthetic separator establishes its original line start.
        head=b"x"*2048
        tail=b"peer_read_reconnect_progress=after_stop_a\n"+b"y"*(2048-len(b"peer_read_reconnect_progress=after_stop_a\n"))
        self.assertEqual(self.peer(head+b"omitted"+tail), self.expected_peer())

    def test_complete_suffix_marker_supersedes_an_observed_prefix(self):
        head=b"peer_read_reconnect_progress=before_stop_a\n"+b"x"*3000
        tail=b"\npeer_read_reconnect_progress=before_amplification_assertions\n"
        self.assertEqual(self.peer(head+tail), self.expected_peer("before_amplification_assertions", "last_window"))
        self.assertEqual(self.peer(b"peer_read_reconnect_progress=before_stop_a\n"+b"x"*8192), self.expected_peer("before_stop_a", "first_window"))

    def test_middle_only_or_stderr_markers_are_unobserved(self):
        marker=b"peer_read_reconnect_progress=after_cleanup_receipt\n"
        self.assertEqual(self.peer(b"x"*4096+marker+b"y"*4096), self.expected_peer())
        self.assertEqual(self.peer(stderr=marker), self.expected_peer())

    def test_progress_does_not_change_failure_or_lifecycle_classification(self):
        marker=b"peer_read_reconnect_progress=before_blackhole_bind\n"
        self.assertEqual(self.peer(marker, b"could not compile private-package"), self.expected_peer("before_blackhole_bind", "short_input", failure_class="compile_failed"))
        self.assertEqual(self.peer(marker, receipt=self.receipt(returncode=0, owned_group_absent=False)), self.expected_peer("before_blackhole_bind", "short_input", returncode=0, lifecycle_unsettled=True))
        self.assertIsNone(self.peer(marker, receipt=self.receipt(returncode=0)))


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
