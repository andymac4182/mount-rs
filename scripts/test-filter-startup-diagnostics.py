#!/usr/bin/env python3
"""Offline controls for the bounded public startup/progress log filter."""

import importlib.util
import io
import json
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import tempfile
import unittest


HELPER = Path(__file__).with_name("filter-startup-diagnostics.py")
SPEC = importlib.util.spec_from_file_location("startup_log_filter", HELPER)
FILTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FILTER)
STAGES = (
    "configuration", "catalog_open", "catalog_load", "catalog_validate",
    "tls_material", "cache_start", "drive_config", "drive_open", "backing_receipt",
    "drive_register", "listener_bind", "ready", "cleanup",
)
SECRET = "private-secret-value-/private/owned-fixture"
REVISION = "a" * 40
EXPECTED = dict(
    source_revision=REVISION, mode="full", provider="sqlite", servers=10,
    clients=10000, drives=10000, partitions=5000, files_per_drive=1000,
    phase_seconds=30,
)


def startup():
    return dict(
        schema="mount-rs.startup.v1", pid=1234, worker=0, generation=1,
        observed_unix_ms=1000, current_stage="configuration", terminal_outcome="running",
        cleanup_outcome=None, configured_partitions=None, planned_drives=None,
        open_started=0, open_success=0, open_error=0, open_cancelled=0,
        open_in_flight=0, registered_drives=0, elapsed_ns=1,
        current_stage_elapsed_ns=1, accounting_complete=True, banks_captured=False,
        stages=[dict(stage=name, started=0, success=0, error=0, cancelled=0,
                     in_flight=0, elapsed_ns=0, max_ns=0) for name in STAGES],
    )


def target(event="controller_start"):
    verified = event != "controller_start"
    return dict(
        schema="mount-rs.target-progress.v1", event=event, pid=3456, elapsed_ns=1,
        mode="full", provider="sqlite", servers=10, clients=10000, drives=10000,
        partitions=5000, files_per_drive=1000, phase_seconds=30, phase="preflight",
        source_revision=REVISION if verified else None, source_verified=verified,
        host_free_bytes=None, initialized_drives=0, connected_clients=0,
        outcome="success" if event == "terminal" else "running", accounting_complete=True,
    )


def ready_startup(complete=True):
    record = startup()
    record.update(current_stage="ready", terminal_outcome="ready", planned_drives=2,
                  configured_partitions=1, open_started=2, open_success=2, registered_drives=2,
                  accounting_complete=complete)
    record["stages"][7].update(started=2, success=2)
    return record


def line(record, prefix="startup_diagnostics "):
    return prefix.encode() + json.dumps(record, separators=(",", ":")).encode() + b"\n"


def target_line(record):
    return line(record, "target_progress ")


class FragmentedInput(io.BytesIO):
    def read(self, size=-1):
        return super().read(min(size, 3))

    def read1(self, size=-1):
        return self.read(size)


class BrokenOutput:
    def write(self, unused):
        raise OSError(SECRET)

    def flush(self):
        raise OSError(SECRET)


class FilterTests(unittest.TestCase):
    def run_filter(self, payload, *, expected=None, source_class=io.BytesIO,
                   private=None, public=None, **limits):
        source = source_class(payload)
        private = private if private is not None else io.BytesIO()
        public = public if public is not None else io.StringIO()
        result = FILTER.filter_stream(source, private, public, expected_target=expected, **limits)
        self.assertEqual(source.read(), b"", "all input must be drained")
        self.assertNotIn(SECRET, json.dumps(result))
        return result, private, public

    def assert_rejected(self, payload, **kwargs):
        result, private, public = self.run_filter(payload, **kwargs)
        self.assertEqual(result["status"], "failed")
        self.assertNotIn(SECRET, public.getvalue())
        return result, private, public

    def test_valid_startup_is_forwarded_and_raw_stream_retained(self):
        payload = b"ordinary build output\n" + line(startup())
        result, private, public = self.run_filter(payload)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["startup_records"], 1)
        self.assertEqual(private.getvalue(), payload)
        self.assertEqual(public.getvalue().encode(), line(startup()))
        self.assertFalse(result["identity_verified"])

    def test_unrecognized_audit_bank_and_private_lines_are_never_echoed(self):
        payload = (f"remote_access {SECRET}\nstartup_bank_diagnostics {{\"private\":\"{SECRET}\"}}\n"
                   f"unrecognized {{\"schema\":\"mount-rs.startup.v1\",\"private\":\"{SECRET}\"}}\n").encode()
        result, private, public = self.run_filter(payload)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(public.getvalue(), "")
        self.assertEqual(private.getvalue(), payload)

    def test_empty_standalone_stream_is_healthy_but_is_not_identity_proof(self):
        result, _, public = self.run_filter(b"")
        self.assertEqual(result["status"], "ok")
        self.assertFalse(result["identity_verified"])
        self.assertEqual(public.getvalue(), "")

    def test_unknown_private_field_rejects_whole_record_and_drains_later_record(self):
        invalid = startup()
        invalid["secret"] = SECRET
        payload = line(invalid) + line(startup())
        result, private, public = self.assert_rejected(payload)
        self.assertEqual(result["startup_records"], 1)
        self.assertEqual(public.getvalue().encode(), line(startup()))
        self.assertEqual(private.getvalue(), payload)

    def test_duplicate_top_level_and_stage_keys_fail_closed(self):
        valid = line(startup())
        for payload in (valid.replace(b'"pid":1234', b'"pid":1234,"pid":4321'),
                        valid.replace(b'"started":0', b'"started":0,"started":0', 1)):
            with self.subTest(payload=payload[:40]):
                self.assert_rejected(payload)

    def test_recognized_invalid_json_utf8_and_constants_fail_without_exception_data(self):
        for payload in (b"startup_diagnostics {\"schema\":\"mount-rs.startup.v1\",\n",
                        b"startup_diagnostics \xff\n",
                        line(startup()).replace(b'"pid":1234', b'"pid":NaN'),
                        line(startup()).replace(b'"pid":1234', b'"pid":Infinity')):
            with self.subTest(payload=payload[:40]):
                self.assert_rejected(payload + line(startup()))

    def test_scalar_types_ranges_and_exact_enums_are_closed(self):
        cases = [("pid", True), ("pid", 0), ("pid", 1 << 32), ("worker", 10),
                 ("generation", -1), ("generation", 1 << 64), ("generation", 1.0),
                 ("generation", "1"), ("accounting_complete", 1), ("banks_captured", True),
                 ("current_stage", SECRET), ("terminal_outcome", "qualified"),
                 ("cleanup_outcome", "ready")]
        for field, value in cases:
            with self.subTest(field=field, value=value):
                record = startup()
                record[field] = value
                self.assert_rejected(line(record))

    def test_stage_order_fields_and_accounting_are_checked(self):
        mutations = []
        record = startup()
        record["stages"].reverse()
        mutations.append(record)
        record = startup()
        record["stages"][0]["secret"] = SECRET
        mutations.append(record)
        record = startup()
        record["stages"][0]["started"] = 1
        mutations.append(record)
        record = startup()
        record["open_success"] = 1
        mutations.append(record)
        for record in mutations:
            self.assert_rejected(line(record))

    def test_ready_record_requires_successful_planned_registration(self):
        record = ready_startup()
        result, _, _ = self.run_filter(line(record))
        self.assertEqual(result["status"], "ok")
        record["registered_drives"] = 1
        self.assert_rejected(line(record))

    def test_running_incomplete_accounting_is_printable_but_filter_fails(self):
        record = startup()
        record["accounting_complete"] = False
        result, _, public = self.run_filter(line(record))
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["first_issue"], "diagnostic_accounting_incomplete")
        self.assertEqual(public.getvalue().encode(), line(record))
        self.assertFalse(result["identity_verified"])

    def test_ready_with_incomplete_accounting_preserves_ready_and_fails_filter(self):
        record = ready_startup(False)
        result, _, public = self.run_filter(line(record))
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["first_issue"], "diagnostic_accounting_incomplete")
        self.assertEqual(public.getvalue().encode(), line(record))
        emitted = json.loads(public.getvalue().split(" ", 1)[1])
        self.assertEqual(emitted["terminal_outcome"], "ready")
        self.assertEqual(emitted["open_success"], 2)
        self.assertEqual(emitted["registered_drives"], 2)

    def test_top_open_counters_must_mirror_drive_open_row_even_when_incomplete(self):
        for complete in (True, False):
            for ending in ("success", "error", "cancelled", "in_flight"):
                with self.subTest(complete=complete, ending=ending):
                    record = startup()
                    record["accounting_complete"] = complete
                    record["open_started"] = 1
                    record["open_" + ending] = 1
                    result, _, public = self.assert_rejected(line(record))
                    self.assertEqual(result["first_issue"], "invalid_accounting")
                    self.assertEqual(public.getvalue(), "")
            record = startup()
            record["accounting_complete"] = complete
            record["stages"][7].update(started=1, in_flight=1)
            with self.subTest(complete=complete, direction="row_to_top"):
                self.assert_rejected(line(record))

    def test_fragmented_reads_preserve_exact_records(self):
        payload = line(startup())
        result, private, public = self.run_filter(payload, source_class=FragmentedInput)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(private.getvalue(), payload)
        self.assertEqual(public.getvalue().encode(), payload)

    def test_partial_eof_is_rejected_without_echoing_partial_json(self):
        for payload in (line(startup())[:-1], f"private unterminated {SECRET}".encode()):
            result, private, public = self.assert_rejected(payload)
            self.assertEqual(result["first_issue"], "truncated_line")
            self.assertEqual(private.getvalue(), payload)
            self.assertEqual(public.getvalue(), "")

    def test_oversized_line_is_discarded_but_next_valid_record_is_drained(self):
        valid = line(startup())
        limit = len(valid) + 10
        payload = b"startup_diagnostics " + b"x" * limit + b"\n" + valid
        result, private, public = self.assert_rejected(payload, max_line_bytes=limit)
        self.assertEqual(result["first_issue"], "line_limit_exceeded")
        self.assertEqual(private.getvalue(), payload)
        self.assertEqual(public.getvalue().encode(), valid)

    def test_whole_log_is_bounded_and_excess_is_drained_without_forwarding(self):
        valid = line(startup())
        payload = valid + f"private {SECRET}\n".encode() + valid
        limit = len(valid) + 10
        result, private, public = self.assert_rejected(payload, max_log_bytes=limit)
        self.assertEqual(result["first_issue"], "log_limit_exceeded")
        self.assertEqual(len(private.getvalue()), limit)
        self.assertEqual(result["input_bytes"], len(payload))
        self.assertEqual(public.getvalue().encode(), valid)

    def test_private_or_public_write_error_is_sanitized_and_drain_continues(self):
        for destination in ("private", "public"):
            with self.subTest(destination=destination):
                result, _, _ = self.run_filter(line(startup()), **{destination: BrokenOutput()})
                self.assertEqual(result["status"], "failed")

    def test_target_header_source_and_terminal_bind_expected_identity(self):
        payload = b"".join(target_line(target(event)) for event in
                           ("controller_start", "source_verified", "capacity", "terminal"))
        result, private, public = self.run_filter(payload, expected=EXPECTED)
        self.assertEqual(result["status"], "ok")
        self.assertTrue(result["identity_verified"])
        self.assertEqual(result["target_records"], 4)
        self.assertEqual(private.getvalue(), payload)
        self.assertEqual(public.getvalue().encode(), payload)

    def test_required_identity_rejects_missing_headers_or_terminal(self):
        for payload in (b"", line(startup()), target_line(target()),
                        target_line(target()) + target_line(target("source_verified"))):
            self.assert_rejected(payload, expected=EXPECTED)

    def test_required_identity_rejects_dimensions_revision_pid_and_verified_drift(self):
        for field, value in (("drives", 100), ("source_revision", "b" * 40),
                             ("pid", 4321), ("source_verified", False)):
            with self.subTest(field=field):
                changed = target("capacity")
                changed[field] = value
                payload = target_line(target()) + target_line(target("source_verified"))
                self.assert_rejected(payload + target_line(changed) +
                                     target_line(target("terminal")), expected=EXPECTED)

    def test_target_order_duplicate_headers_and_duplicate_terminal_fail_closed(self):
        records = [target(event) for event in ("controller_start", "source_verified", "terminal")]
        for selection in ((1, 0, 2), (0, 0, 1, 2), (0, 1, 1, 2), (0, 1, 2, 2)):
            self.assert_rejected(b"".join(target_line(records[index]) for index in selection),
                                 expected=EXPECTED)

    def test_target_fields_enums_counts_and_success_accounting_are_closed(self):
        for field, value in (("phase", SECRET), ("provider", "other"), ("host_free_bytes", False),
                             ("connected_clients", 10001), ("initialized_drives", 10001),
                             ("accounting_complete", False), ("secret", SECRET)):
            with self.subTest(field=field):
                record = target("terminal")
                record[field] = value
                self.assert_rejected(target_line(record))

    def test_target_error_terminal_is_printable_but_not_product_success(self):
        final = target("terminal")
        final.update(outcome="error", accounting_complete=False)
        payload = target_line(target()) + target_line(target("source_verified")) + target_line(final)
        result, _, _ = self.run_filter(payload, expected=EXPECTED)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["first_issue"], "diagnostic_accounting_incomplete")
        self.assertFalse(result["identity_verified"])

    def test_target_running_incomplete_record_is_forwarded_and_filter_fails(self):
        record = target("progress")
        record["accounting_complete"] = False
        result, _, public = self.run_filter(target_line(record))
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["first_issue"], "diagnostic_accounting_incomplete")
        self.assertEqual(public.getvalue().encode(), target_line(record))

    def test_startup_and_target_record_limits_are_independent_of_raw_bank_limit(self):
        bank = b"startup_bank_diagnostics " + b"x" * (256 * 1024) + b"\n"
        result, _, public = self.run_filter(bank + line(startup()))
        self.assertEqual(result["status"], "ok")
        self.assertNotIn("startup_bank_diagnostics", public.getvalue())
        invalid = startup()
        invalid["secret"] = "x" * (16 * 1024)
        self.assert_rejected(line(invalid))

    def test_cli_creates_private_file_and_emits_only_safe_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            private = Path(directory) / "private.log"
            payload = f"private {SECRET}\n".encode() + line(startup())
            result = subprocess.run([sys.executable, str(HELPER), "--private-log", str(private)],
                                    input=payload, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(private.read_bytes(), payload)
            self.assertEqual(stat.S_IMODE(private.stat().st_mode), 0o600)
            self.assertNotIn(SECRET.encode(), result.stdout + result.stderr)
            self.assertEqual(result.stdout, line(startup()))
            self.assertEqual(json.loads(result.stderr)["status"], "ok")

    def test_cli_forwards_small_flushed_record_before_input_eof(self):
        with tempfile.TemporaryDirectory() as directory:
            private = Path(directory) / "live.log"
            process = subprocess.Popen(
                [sys.executable, str(HELPER), "--private-log", str(private)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            try:
                payload = line(startup())
                self.assertLess(len(payload), 8192)
                process.stdin.write(payload)
                process.stdin.flush()
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(timeout=1),
                                    "a complete flushed record must be public before producer EOF")
                self.assertEqual(process.stdout.readline(), payload)
                self.assertIsNone(process.poll(), "producer stdin is intentionally still open")
                self.assertEqual(private.read_bytes(), payload,
                                 "the publicly forwarded frame must already be retained privately")
                process.stdin.close()
                process.wait(timeout=2)
                self.assertEqual(process.returncode, 0)
                self.assertEqual(json.loads(process.stderr.read())["status"], "ok")
                self.assertEqual(private.read_bytes(), payload)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=2)
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()

    def test_cli_existing_file_symlink_or_invalid_args_fail_without_private_data(self):
        with tempfile.TemporaryDirectory() as directory:
            existing = Path(directory) / "existing.log"
            existing.write_text(SECRET)
            symlink = Path(directory) / "symlink.log"
            symlink.symlink_to(existing)
            cases = (["--private-log", str(existing)], ["--private-log", str(symlink)],
                     ["--private-log", str(Path(directory) / "new.log"), "--unknown", SECRET])
            for arguments in cases:
                result = subprocess.run([sys.executable, str(HELPER), *arguments],
                                        input=line(startup()), capture_output=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, b"")
                self.assertNotIn(SECRET.encode(), result.stderr)
                self.assertNotIn(directory.encode(), result.stderr)
            self.assertEqual(existing.read_text(), SECRET)

    def test_cli_required_identity_accepts_exact_bindings_and_rejects_bad_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            common = ["--require-target-identity"]
            for field, value in EXPECTED.items():
                common += ["--expected-" + field.replace("_", "-"), str(value)]
            payload = b"".join(target_line(target(event)) for event in
                               ("controller_start", "source_verified", "terminal"))
            result = subprocess.run([sys.executable, str(HELPER), "--private-log",
                                     str(Path(directory) / "healthy.log"), *common],
                                    input=payload, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0)
            self.assertTrue(json.loads(result.stderr)["identity_verified"])
            revision_index = common.index("--expected-source-revision") + 1
            common[revision_index] = "b" * 40
            result = subprocess.run([sys.executable, str(HELPER), "--private-log",
                                     str(Path(directory) / "mismatch.log"), *common],
                                    input=payload, capture_output=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(json.loads(result.stderr)["identity_verified"])

    def test_cli_malformed_record_drains_to_private_log_and_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as directory:
            private = Path(directory) / "malformed.log"
            payload = f"startup_diagnostics {{\"secret\":\"{SECRET}\"\n".encode() + line(startup())
            result = subprocess.run([sys.executable, str(HELPER), "--private-log", str(private)],
                                    input=payload, capture_output=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(private.read_bytes(), payload)
            self.assertEqual(result.stdout, line(startup()))
            self.assertNotIn(SECRET.encode(), result.stdout + result.stderr)
            self.assertEqual(json.loads(result.stderr)["first_issue"], "invalid_json")


if __name__ == "__main__":
    unittest.main()
