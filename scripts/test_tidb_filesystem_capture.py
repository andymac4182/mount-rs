"""Pure output-admission controls; no TiDB, filesystem provider or subprocess runs."""

import io
import unittest

from tidb_filesystem_capture import (
    CAPTURE_CAP, SDK_NAME, PHASE_MARKERS, CaptureError,
    bounded_capture, verify_sdk_capture,
)


def valid(phase="0", *, interleaved=True):
    marker = PHASE_MARKERS[phase]
    if interleaved:
        result = f"test {SDK_NAME} ... {marker}\nok\n"
    else:
        result = f"{marker}\ntest {SDK_NAME} ... ok\n"
    return ("\nrunning 1 test\n" + result + "\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
            "0 filtered out; finished in 0.03s\n\n").encode("utf-8")


class CaptureTests(unittest.TestCase):
    def refuse(self, raw, phase="0", exit_code=0):
        with self.assertRaises(CaptureError):
            verify_sdk_capture(raw, phase, exit_code)

    def test_both_phases_and_real_libtest_marker_positions_are_admitted(self):
        for phase in ("0", "1"):
            for interleaved in (False, True):
                with self.subTest(phase=phase, interleaved=interleaved):
                    self.assertIsNone(verify_sdk_capture(valid(phase, interleaved=interleaved), phase, 0))

    def test_cargo_exit_zero_with_zero_tests_cannot_qualify_retained_phase_one(self):
        self.refuse(b"running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n", "1")

    def test_empty_or_missing_output_is_not_passing_evidence(self):
        self.refuse(b"")
        self.refuse(b"cargo finished successfully\n")

    def test_filtered_count_is_rejected_despite_one_pass(self):
        self.refuse(valid().replace(b"0 filtered out", b"1 filtered out"))

    def test_ignored_test_is_rejected_despite_exit_zero(self):
        self.refuse(valid().replace(b"\nok\n", b"\nignored\n").replace(b"1 passed", b"0 passed").replace(b"0 ignored", b"1 ignored"))

    def test_nonzero_failed_or_measured_counts_are_rejected(self):
        for field in (b"failed", b"measured"):
            with self.subTest(field=field):
                self.refuse(valid().replace(b"0 " + field, b"1 " + field))

    def test_missing_or_wrong_named_test_is_rejected(self):
        self.refuse(valid().replace(SDK_NAME.encode(), b"other_test"))
        self.refuse(valid().replace(f"test {SDK_NAME} ... ".encode(), b""))

    def test_duplicate_named_result_is_rejected(self):
        self.refuse(valid() + f"test {SDK_NAME} ... ok\n".encode())

    def test_extra_other_test_is_rejected(self):
        self.refuse(valid() + b"test other_test ... ok\n")

    def test_duplicate_or_wrong_running_count_is_rejected(self):
        self.refuse(valid() + b"running 1 test\n")
        self.refuse(valid().replace(b"running 1 test", b"running 2 tests"))

    def test_duplicate_or_missing_result_summary_is_rejected(self):
        summary = valid().split(b"test result: ")[1].strip()
        self.refuse(valid() + b"test result: " + summary + b"\n")
        self.refuse(valid().split(b"test result: ")[0])

    def test_missing_marker_is_rejected(self):
        self.refuse(valid(interleaved=False).replace(PHASE_MARKERS["0"].encode() + b"\n", b""))

    def test_wrong_phase_marker_is_rejected(self):
        self.refuse(valid("0"), "1")
        self.refuse(valid("1"), "0")

    def test_duplicate_marker_is_rejected(self):
        self.refuse(valid() + PHASE_MARKERS["0"].encode() + b"\n")

    def test_both_phase_markers_are_rejected(self):
        self.refuse(valid() + PHASE_MARKERS["1"].encode() + b"\n")

    def test_incomplete_or_modified_marker_is_rejected(self):
        self.refuse(valid().replace(b"sql_blocks=0", b"sql_blocks=1"))
        self.refuse(valid().replace(b"root_authority=verified ", b""))

    def test_failed_named_result_is_rejected(self):
        self.refuse(valid().replace(b"\nok\n", b"\nFAILED\n"))

    def test_nonzero_cargo_exit_is_rejected_even_with_complete_evidence(self):
        self.refuse(valid(), exit_code=1)

    def test_status_and_phase_require_exact_closed_types(self):
        for status in (True, False, "0", None, -1):
            with self.subTest(status=status):
                self.refuse(valid(), exit_code=status)
        for phase in (0, 1, True, "seed", "", None):
            with self.subTest(phase=phase):
                self.refuse(valid(), phase=phase)

    def test_stdout_is_exact_bytes_bounded_utf8_without_control_rewrites(self):
        for raw in (valid().decode(), None, b"\xff", valid() + b"\x00", valid() + b"\x1b[2K", valid().replace(b"\n", b"\r\n"), b"x" * (CAPTURE_CAP + 1)):
            with self.subTest(type=type(raw).__name__):
                self.refuse(raw)

    def test_bounded_capture_retains_small_exact_output(self):
        source = io.BytesIO(valid())
        sink = io.BytesIO()
        self.assertFalse(bounded_capture(source, sink))
        self.assertEqual(sink.getvalue(), valid())
        self.assertEqual(source.read(), b"")

    def test_bounded_capture_caps_writes_and_drains_overflow_to_eof(self):
        raw = b"x" * (CAPTURE_CAP * 3 + 17)
        source = io.BytesIO(raw)
        sink = io.BytesIO()
        self.assertTrue(bounded_capture(source, sink))
        self.assertEqual(len(sink.getvalue()), CAPTURE_CAP)
        self.assertEqual(sink.getvalue(), raw[:CAPTURE_CAP])
        self.assertEqual(source.read(), b"")


if __name__ == "__main__":
    unittest.main()
