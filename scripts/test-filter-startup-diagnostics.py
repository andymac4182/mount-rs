#!/usr/bin/env python3
"""Offline controls for the bounded public startup/progress log filter."""

import importlib.util
import io
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import tempfile
import time
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


def lazy_startup(ready=False, complete=True):
    record = startup()
    record.update(schema="mount-rs.startup.v2", construction_mode="lazy",
                  max_active_drives=None, construction_plans=0,
                  accounting_complete=complete)
    if ready:
        record.update(current_stage="ready", terminal_outcome="ready", planned_drives=3,
                      configured_partitions=2, max_active_drives=2048,
                      construction_plans=3, registered_drives=3)
        record["stages"][6].update(started=3, success=3)
        record["stages"][9].update(started=3, success=3)
    return record


def line(record, prefix="startup_diagnostics "):
    return prefix.encode() + json.dumps(record, separators=(",", ":")).encode() + b"\n"


def target_line(record):
    return line(record, "target_progress ")


def resource(role="controller"):
    return dict(
        schema="mount-rs.resource-progress.v1", controller_pid=3456,
        source_revision=REVISION, source_digest="b" * 64, binary_sha256="c" * 64,
        role=role, pid=3456 if role == "controller" else 1234,
        worker=None if role == "controller" else 0,
        generation_context=None if role == "controller" else 1, phase="preflight",
        observed_unix_ms=1000, published_unix_ms=1010, samples=1,
        sample_interval_ms=100, terminal_sample=None, available=True, reason=None,
        counter_scope="sampler_baseline_cumulative_process", cpu_user_us=5,
        cpu_system_us=2, rss_current_bytes=100, rss_lifetime_peak_bytes=120,
        rss_peak_bytes=120, minimum_host_free_bytes=1 << 36,
        block_inputs=1, block_outputs=2, process_disk_read_bytes=None,
        process_disk_write_bytes=None, process_disk_bytes_reason="not_captured_by_sampler",
    )


def resource_line(record):
    return line(record, "resource_progress ")


def resource_stream(*records, worker=False):
    header = target_line(target()) + target_line(target("source_verified"))
    if worker:
        header += line(startup())
    return header + b"".join(resource_line(record) for record in records) + target_line(target("terminal"))


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

    def test_lazy_v2_registered_plans_are_ready_without_startup_opens(self):
        record = lazy_startup(ready=True)
        payload = line(record)
        result, private, public = self.run_filter(payload)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["startup_records"], 1)
        self.assertEqual(private.getvalue(), payload)
        self.assertEqual(public.getvalue().encode(), payload)
        emitted = json.loads(public.getvalue().split(" ", 1)[1])
        self.assertEqual(emitted["registered_drives"], 3)
        self.assertEqual((emitted["open_started"], emitted["open_success"]), (0, 0))

    def test_lazy_v2_requires_closed_fields_and_explicit_nullable_capacity(self):
        record = lazy_startup()
        result, _, public = self.run_filter(line(record))
        self.assertEqual(result["status"], "ok")
        self.assertEqual(public.getvalue().encode(), line(record))
        for field in ("construction_mode", "max_active_drives", "construction_plans"):
            with self.subTest(missing=field):
                changed = lazy_startup()
                del changed[field]
                self.assert_rejected(line(changed))
        for field, value in (("construction_mode", "eager"),
                             ("construction_mode", SECRET),
                             ("max_active_drives", 0), ("max_active_drives", True),
                             ("max_active_drives", 1.0), ("max_active_drives", 1 << 64),
                             ("construction_plans", None), ("construction_plans", True),
                             ("construction_plans", -1), ("construction_plans", 1 << 64),
                             ("private_path", SECRET)):
            with self.subTest(field=field, value=value):
                changed = lazy_startup()
                changed[field] = value
                self.assert_rejected(line(changed))

    def test_lazy_v2_rejects_false_readiness_and_nonzero_startup_opens(self):
        result, _, _ = self.run_filter(line(lazy_startup(ready=True)))
        self.assertEqual(result["status"], "ok")
        for complete in (True, False):
            for field, value in (("max_active_drives", None), ("construction_plans", 2),
                                 ("registered_drives", 2), ("planned_drives", None)):
                with self.subTest(field=field, complete=complete):
                    record = lazy_startup(ready=True, complete=complete)
                    record[field] = value
                    self.assert_rejected(line(record))
            with self.subTest(complete=complete):
                record = lazy_startup(ready=True, complete=complete)
                record.update(open_started=1, open_success=1)
                record["stages"][7].update(started=1, success=1)
                self.assert_rejected(line(record))

    def test_lazy_v2_incomplete_ready_remains_printable_but_fails_filter(self):
        record = lazy_startup(ready=True, complete=False)
        result, _, public = self.run_filter(line(record))
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["first_issue"], "diagnostic_accounting_incomplete")
        self.assertEqual(public.getvalue().encode(), line(record))

    def test_eager_v1_shape_stays_closed_to_lazy_fields(self):
        record = startup()
        self.assertEqual(len(record), 21)
        result, _, public = self.run_filter(line(record))
        self.assertEqual(result["status"], "ok")
        self.assertEqual(public.getvalue().encode(), line(record))
        for field in ("construction_mode", "max_active_drives", "construction_plans"):
            with self.subTest(field=field):
                changed = startup()
                changed[field] = None
                self.assert_rejected(line(changed))

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

    def test_resource_records_bind_header_and_are_forwarded_losslessly(self):
        record = resource()
        record.update(cpu_user_us=FILTER.U64_MAX, block_outputs=FILTER.U64_MAX,
                      observed_unix_ms=FILTER.U64_MAX - 1, published_unix_ms=FILTER.U64_MAX)
        payload = resource_stream(record)
        result, private, public = self.run_filter(payload, expected=EXPECTED,
                                                source_class=FragmentedInput)
        self.assertEqual(result.get("resource_records", 0), 1,
                         "the resource observation must be forwarded, not silently ignored")
        self.assertEqual(result["schema"], "mount-rs.startup-log-filter.v2")
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["resource_records"], 1)
        self.assertTrue(result["identity_verified"])
        self.assertEqual(private.getvalue(), payload)
        self.assertEqual(public.getvalue().encode(), payload)

    def test_resource_requires_verified_header_even_without_expected_arguments(self):
        for payload in (resource_line(resource()),
                        target_line(target()) + resource_line(resource()),
                        target_line(target("source_verified")) + resource_line(resource())):
            result, _, public = self.assert_rejected(payload)
            self.assertEqual(result["first_issue"], "resource_identity_mismatch")
            self.assertNotIn("resource_progress", public.getvalue())

    def test_worker_generation_requires_startup_pid_and_generation_context(self):
        record = resource("worker")
        result, _, _ = self.run_filter(resource_stream(record, worker=True), expected=EXPECTED)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["resource_records"], 1)
        self.assert_rejected(resource_stream(record), expected=EXPECTED)
        record["generation_context"] = None
        result, _, _ = self.run_filter(resource_stream(record), expected=EXPECTED)
        self.assertEqual(result["status"], "ok")
        record["generation_context"] = 2
        self.assert_rejected(resource_stream(record, worker=True), expected=EXPECTED)

    def test_resource_identity_source_binary_pid_role_and_phase_drift_fail_closed(self):
        for field, value in (("controller_pid", 9000), ("source_revision", "d" * 40),
                             ("source_digest", "d" * 64), ("binary_sha256", "d" * 64),
                             ("pid", 9999), ("phase", "worker_setup")):
            with self.subTest(field=field):
                changed = resource()
                changed[field] = value
                result, _, public = self.assert_rejected(resource_stream(resource(), changed),
                                                        expected=EXPECTED)
                self.assertEqual(result["resource_records"], 1)
                self.assertNotIn(SECRET, public.getvalue())

    def test_resource_worker_pid_roster_and_startup_drift_are_bounded(self):
        valid = resource("worker")
        invalid = resource("worker")
        invalid.update(pid=9999, generation_context=None)
        self.assert_rejected(resource_stream(valid, invalid, worker=True), expected=EXPECTED)
        changed = startup()
        changed["pid"] = 9999
        header = target_line(target()) + target_line(target("source_verified")) + line(startup())
        self.assert_rejected(header + resource_line(valid) + line(changed) +
                             resource_line(valid) + target_line(target("terminal")), expected=EXPECTED)

    def test_resource_first_binding_cannot_replace_an_earlier_startup_identity(self):
        header = target_line(target()) + target_line(target("source_verified")) + line(startup())
        changed_startup = startup()
        changed_startup.update(pid=9999, generation=2)
        changed = resource("worker")
        changed.update(pid=9999, generation_context=2)
        self.assert_rejected(header + line(changed_startup) + resource_line(changed) +
                             target_line(target("terminal")), expected=EXPECTED)
        changed_startup.update(pid=1234, generation=0)
        changed.update(pid=1234, generation_context=0)
        self.assert_rejected(header + line(changed_startup) + resource_line(changed) +
                             target_line(target("terminal")), expected=EXPECTED)
        changed.update(pid=9999, generation_context=None)
        self.assert_rejected(header + resource_line(changed) + target_line(target("terminal")),
                             expected=EXPECTED)

    def test_resource_os_lifetime_peak_cannot_regress_behind_a_retained_sampled_peak(self):
        first = resource()
        first.update(samples=2, terminal_sample=False)
        changed = dict(first)
        changed.update(samples=3, rss_lifetime_peak_bytes=119)
        result, _, _ = self.assert_rejected(resource_stream(first, changed), expected=EXPECTED)
        self.assertEqual(result["first_issue"], "resource_counter_regressed")

    def test_resource_closed_fields_enums_types_and_unavailable_disk_are_private(self):
        changes = (("secret", SECRET), ("reason", SECRET), ("role", SECRET),
                   ("counter_scope", SECRET), ("process_disk_bytes_reason", SECRET),
                   ("worker", 0), ("generation_context", 0), ("cpu_user_us", True),
                   ("cpu_system_us", -1), ("block_inputs", 1 << 64), ("block_outputs", "2"),
                   ("samples", 0), ("sample_interval_ms", 1), ("terminal_sample", 1),
                   ("available", 1), ("process_disk_read_bytes", 0),
                   ("process_disk_write_bytes", 0), ("published_unix_ms", 0),
                   ("observed_unix_ms", 1011), ("published_unix_ms", 11001),
                   ("rss_peak_bytes", 99), ("source_digest", "B" * 64))
        for field, value in changes:
            with self.subTest(field=field):
                record = resource()
                record[field] = value
                self.assert_rejected(resource_stream(record), expected=EXPECTED)
        record = resource()
        record["samples"] = 2
        self.assert_rejected(resource_stream(record), expected=EXPECTED)

    def test_resource_unavailable_is_printable_but_invalidates_filter(self):
        fields = ("observed_unix_ms", "samples", "terminal_sample", "cpu_user_us", "cpu_system_us",
                  "rss_current_bytes", "rss_lifetime_peak_bytes", "rss_peak_bytes",
                  "minimum_host_free_bytes", "block_inputs", "block_outputs")
        for reason in ("missing_sample", "invalid_sample", "stale_sample", "foreign_pid",
                       "resource_validation_failed"):
            with self.subTest(reason=reason):
                record = resource()
                record.update(available=False, reason=reason, **dict.fromkeys(fields))
                result, _, public = self.assert_rejected(resource_stream(record), expected=EXPECTED)
                self.assertEqual(result["first_issue"], "resource_observation_unavailable")
                self.assertEqual(result["resource_records"], 1)
                self.assertIn(resource_line(record).decode(), public.getvalue())
                record["rss_current_bytes"] = 0
                result, _, public = self.assert_rejected(resource_stream(record), expected=EXPECTED)
                self.assertEqual(result["resource_records"], 0)

    def test_resource_duplicate_key_oversize_and_partial_eof_fail_and_keep_draining(self):
        valid = resource_line(resource())
        cases = (valid.replace(b'"samples":1', b'"samples":1,"samples":1'),
                 valid[:-1] + b" " * 2048 + b"\n", valid[:-1])
        for payload in cases:
            with self.subTest(size=len(payload)):
                header = target_line(target()) + target_line(target("source_verified"))
                self.assert_rejected(header + payload)
        record = resource()
        record["secret"] = SECRET
        payload = resource_stream(record, resource())
        result, private, public = self.assert_rejected(payload, expected=EXPECTED)
        self.assertEqual(result["resource_records"], 1)
        self.assertEqual(private.getvalue(), payload)

    def test_resource_limit_counts_prefix_and_newline_at_the_exact_boundary(self):
        original = resource_line(resource())
        exact = original[:-1] + b" " * (2048 - len(original)) + b"\n"
        self.assertEqual(len(exact), 2048)
        header = target_line(target()) + target_line(target("source_verified"))
        result, _, public = self.run_filter(header + exact + target_line(target("terminal")),
                                            expected=EXPECTED)
        self.assertEqual(result["status"], "ok")
        self.assertIn(original.decode(), public.getvalue())
        over = exact[:-1] + b" \n"
        self.assertEqual(len(over), 2049)
        result, _, public = self.assert_rejected(header + over + target_line(target("terminal")),
                                                expected=EXPECTED)
        self.assertEqual(result["first_issue"], "record_limit_exceeded")
        self.assertEqual(result["resource_records"], 0)

    def test_resource_cumulative_counter_regressions_do_not_reset_on_generation_change(self):
        first = resource("worker")
        first.update(samples=2, terminal_sample=False)
        for field in ("samples", "cpu_user_us", "cpu_system_us", "block_inputs", "block_outputs",
                      "rss_peak_bytes"):
            with self.subTest(field=field):
                changed = dict(first)
                changed[field] -= 1
                self.assert_rejected(resource_stream(first, changed, worker=True), expected=EXPECTED)
        changed = dict(first)
        changed["minimum_host_free_bytes"] += 1
        self.assert_rejected(resource_stream(first, changed, worker=True), expected=EXPECTED)
        changed = dict(first)
        changed.update(generation_context=2, samples=3, cpu_user_us=4)
        next_startup = startup()
        next_startup["generation"] = 2
        header = target_line(target()) + target_line(target("source_verified")) + line(startup())
        self.assert_rejected(header + resource_line(first) + line(next_startup) +
                             resource_line(changed) + target_line(target("terminal")), expected=EXPECTED)

    def test_resource_writer_failure_keeps_raw_stream_private_and_drains(self):
        payload = resource_stream(resource())
        result, private, _ = self.run_filter(payload, expected=EXPECTED, public=BrokenOutput())
        self.assertEqual(result["status"], "failed")
        self.assertEqual(private.getvalue(), payload)

    def test_resource_context_can_advance_without_resetting_the_sampler(self):
        first = resource("worker")
        changed = dict(first)
        changed.update(generation_context=2, samples=2, terminal_sample=False,
                       cpu_user_us=6, rss_current_bytes=90)
        next_startup = startup()
        next_startup["generation"] = 2
        header = target_line(target()) + target_line(target("source_verified")) + line(startup())
        payload = header + resource_line(first) + line(next_startup) + resource_line(changed)
        payload += target_line(target("terminal"))
        result, _, public = self.run_filter(payload, expected=EXPECTED)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["resource_records"], 2)
        self.assertEqual(public.getvalue().encode(), payload)

    def test_resource_cli_forwarding_is_visible_before_producer_eof(self):
        with tempfile.TemporaryDirectory() as directory:
            private = Path(directory) / "resource.log"
            process = subprocess.Popen(
                [sys.executable, str(HELPER), "--private-log", str(private)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                bufsize=0,
            )
            try:
                payload = target_line(target()) + target_line(target("source_verified"))
                payload += resource_line(resource())
                process.stdin.write(payload)
                process.stdin.flush()
                emitted = bytearray()
                os.set_blocking(process.stdout.fileno(), False)
                deadline = time.monotonic() + 1
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    # A partial-frame regression cannot block while producer stdin is open.
                    while emitted.count(b"\n") < 3:
                        remaining = deadline - time.monotonic()
                        self.assertGreater(remaining, 0)
                        self.assertTrue(selector.select(timeout=remaining))
                        try:
                            chunk = os.read(process.stdout.fileno(), 8192)
                        except BlockingIOError:
                            continue
                        self.assertTrue(chunk)
                        emitted.extend(chunk)
                        self.assertLessEqual(len(emitted), len(payload))
                os.set_blocking(process.stdout.fileno(), True)
                self.assertEqual(bytes(emitted), payload)
                self.assertIsNone(process.poll())
                self.assertEqual(private.read_bytes(), payload)
                final = target_line(target("terminal"))
                process.stdin.write(final)
                process.stdin.close()
                process.wait(timeout=2)
                self.assertEqual(process.returncode, 0)
                self.assertEqual(process.stdout.read(), final)
                summary = json.loads(process.stderr.read())
                self.assertEqual(summary["resource_records"], 1)
                self.assertEqual(summary["schema"], "mount-rs.startup-log-filter.v2")
                self.assertEqual(stat.S_IMODE(private.stat().st_mode), 0o600)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=2)
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()


if __name__ == "__main__":
    unittest.main()
