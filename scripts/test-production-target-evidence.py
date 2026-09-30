#!/usr/bin/env python3
"""Offline synthetic controls for production-target receipt verification.

These temporary JSON/gzip fixtures are verifier test inputs, never live client,
storage, durability, throughput, or production-capacity evidence. No provider,
server, Cargo command, or retained historical capture is used.
"""

import argparse
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


RUNNER = Path(__file__).with_name("bench-remote-production-target.sh")
SHARED_VERIFIER = Path(__file__).with_name("verify-production-target-evidence.py")
STORED_LIMIT = 16 * 1024 * 1024
DECODED_LIMIT = 64 * 1024 * 1024
SENTINEL = "FULL_TARGET_GEOMETRY_REGRESSION"
GEOMETRY_FAILURE = (
    SENTINEL + ": relabeled 10-Drive/F2/1-second control was accepted as full"
)


def current_inline_program():
    """Execute the current verifier verbatim, without invoking the shell runner."""
    source = RUNNER.read_text(encoding="utf-8")
    marker = "python3 - <<'PY'\n"
    if source.count(marker) != 1:
        raise RuntimeError("expected exactly one current inline verifier")
    program, end, suffix = source.split(marker, 1)[1].partition("\nPY\n")
    if not end or suffix.strip():
        raise RuntimeError("current inline verifier boundary changed")
    return program


def verify_fixture(root, mode):
    """Adapter for today's inline verifier and the later shared raising API."""
    if SHARED_VERIFIER.is_file():
        spec = importlib.util.spec_from_file_location(
            "production_target_evidence", SHARED_VERIFIER
        )
        if spec is None or spec.loader is None:
            raise RuntimeError("shared verifier import unavailable")
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        try:
            verifier.verify_terminal(root, mode)
        except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
            return subprocess.CompletedProcess(
                [str(SHARED_VERIFIER)], 2, "", type(error).__name__
            )
        return subprocess.CompletedProcess([str(SHARED_VERIFIER)], 0, "", "")

    environment = os.environ.copy()
    environment["MOUNT_RS_TARGET_OUTPUT"] = str(root)
    environment["MOUNT_RS_TARGET_MODE"] = mode
    return subprocess.run(
        [sys.executable, "-c", current_inline_program()],
        env=environment,
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )


class SyntheticControl:
    """Ten synthetic Drive ledgers joined to two passes and 22 metric frames."""

    DRIVES = 10
    FILES = 2
    SERVERS = 10
    CONTROLLER_PID = 2000

    def __init__(self, root):
        self.root = root
        self.receipt_names = []
        self.worker_metric_names = []
        self.total_stored_bytes = 0
        workers = [
            dict(server=server, pid=1000 + server, reap_confirmed=True, exit_code=0)
            for server in range(self.SERVERS)
        ]
        self.terminal = dict(
            schema="mount-rs-production-target-v1",
            scope="synthetic offline verifier fixture; no live capacity proof",
            phase="terminal",
            outcome="success",
            full_target=False,
            configuration=dict(
                full_target=False,
                drives=self.DRIVES,
                files=self.FILES,
                seconds=1,
                population_seconds=600,
                provider="sqlite",
            ),
            workers=workers,
            cleanup_errors=[],
            verified_passes=2,
            fresh_oracle_complete=True,
            fresh_oracle_settled=True,
            expected_state_observation=dict(complete=True),
            expected_state_receipts=[],
            verified_files=self.DRIVES * self.FILES,
            verified_bytes=self.DRIVES * self.FILES * 4096,
            namespace_files=self.DRIVES * self.FILES,
            population_bytes=self.DRIVES * self.FILES * 4096,
            fresh_oracle_passes=[],
            phase_metrics=dict(boundaries=[]),
            metrics_required_for_outcome=True,
            controller_resources=dict(pid=self.CONTROLLER_PID),
            source=dict(digest="a" * 64, binary_sha256="b" * 64),
        )
        for drive in range(self.DRIVES):
            ledger = dict(
                drive=drive,
                files={
                    f"mixed-{file}": dict(identity=file, length=4096, changed={})
                    for file in range(self.FILES)
                },
                generations=[0] * self.FILES,
                oracle="tuple-seeded4096-byte blocks; initial generation0",
            )
            receipt = self.receipt(f"expected/drive-{drive}.json", ledger)
            receipt["drive"] = drive
            self.terminal["expected_state_receipts"].append(receipt)

        for sequence, label in enumerate(("initial", "final")):
            summary = dict(
                slot_limit=8,
                expected_drives=self.DRIVES,
                started_drives=self.DRIVES,
                completed_drives=self.DRIVES,
                live_slots=0,
                expected_files=self.DRIVES * self.FILES,
                completed_files=self.DRIVES * self.FILES,
                checked_files=self.DRIVES * self.FILES,
                expected_bytes=self.DRIVES * self.FILES * 4096,
                completed_bytes=self.DRIVES * self.FILES * 4096,
                compared_bytes=self.DRIVES * self.FILES * 4096,
                complete=True,
                settled=True,
            )
            summary["pass"] = label
            raw = dict(summary, completed_drive_ids=list(range(self.DRIVES)))
            receipt = self.receipt(f"oracle-receipts/{label}.json", raw)
            summary.update(after_boundary_complete=True, receipt=receipt)
            self.terminal["fresh_oracle_passes"].append(summary)

            common = dict(
                controller_pid=self.CONTROLLER_PID,
                generation=0,
                sequence=sequence,
                phase=f"{label}_fresh_oracle",
                boundary="after",
                source_digest=self.terminal["source"]["digest"],
                binary_digest=self.terminal["source"]["binary_sha256"],
                catalog_digest="c" * 64,
                backend_prefix="synthetic-control-only",
            )
            frame = dict(
                identity=dict(common, pid=self.CONTROLLER_PID, server=None, role="controller"),
                capture_complete=True,
                metrics_complete=True,
            )
            controller = self.receipt(f"metrics/g0-s{sequence}.json.gz", frame)
            measured = []
            for worker in workers:
                server, pid = worker["server"], worker["pid"]
                frame = dict(
                    identity=dict(common, pid=pid, server=server, role="worker"),
                    capture_complete=True,
                    metrics_complete=True,
                )
                name = f"worker-{server}/metrics/g0-s{sequence}.json.gz"
                self.worker_metric_names.append(name)
                receipt = self.receipt(name, frame)
                receipt.update(server=server, pid=pid)
                measured.append(receipt)
            self.terminal["phase_metrics"]["boundaries"].append(dict(
                phase=f"{label}_fresh_oracle",
                boundary="after",
                generation=0,
                sequence=sequence,
                complete=True,
                metrics_complete=True,
                controller=controller,
                workers=measured,
            ))
        self.publish_terminal()

    def write(self, name, value):
        plain = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
        if len(plain) > DECODED_LIMIT:
            raise ValueError("synthetic fixture exceeds inherited decoded limit")
        data = gzip.compress(plain, mtime=0) if name.endswith(".gz") else plain
        if len(data) > STORED_LIMIT:
            raise ValueError("synthetic fixture exceeds inherited stored limit")
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        previous = path.stat().st_size if path.exists() else 0
        path.write_bytes(data)
        self.total_stored_bytes += len(data) - previous
        return data

    def receipt(self, name, value):
        data = self.write(name, value)
        if name not in self.receipt_names:
            self.receipt_names.append(name)
        return dict(file=name, sha256=hashlib.sha256(data).hexdigest())

    def publish_terminal(self):
        self.write("terminal.json", self.terminal)


class FullTargetGeometryRegression(AssertionError):
    """The one intended RED: reduced geometry accepted under the full label."""


class ProductionTargetEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="mount-rs-evidence-test-")
        self.addCleanup(self.directory.cleanup)
        self.fixture = SyntheticControl(Path(self.directory.name))

    def assert_rejected(self, mode="control"):
        self.fixture.publish_terminal()
        result = verify_fixture(self.fixture.root, mode)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)

    def test_valid_synthetic_control_is_accepted(self):
        self.assertEqual(len(self.fixture.terminal["expected_state_receipts"]), 10)
        self.assertEqual(len(self.fixture.terminal["fresh_oracle_passes"]), 2)
        self.assertEqual(len(self.fixture.worker_metric_names), 20)
        self.assertEqual(len(self.fixture.receipt_names), 34)
        result = verify_fixture(self.fixture.root, "control")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_control_relabelled_full_is_rejected(self):
        self.fixture.terminal["full_target"] = True
        self.fixture.terminal["configuration"]["full_target"] = True
        self.fixture.publish_terminal()
        result = verify_fixture(self.fixture.root, "full")
        if result.returncode == 0:
            raise FullTargetGeometryRegression(GEOMETRY_FAILURE)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)

    def test_changed_receipt_digest_is_rejected(self):
        self.fixture.terminal["expected_state_receipts"][0]["sha256"] = "0" * 64
        self.assert_rejected()

    def test_missing_oracle_drive_roster_is_rejected(self):
        summary = self.fixture.terminal["fresh_oracle_passes"][0]
        path = self.fixture.root / summary["receipt"]["file"]
        raw = json.loads(path.read_bytes())
        raw["completed_drive_ids"].pop()
        summary["receipt"] = self.fixture.receipt(summary["receipt"]["file"], raw)
        self.assert_rejected()

    def test_unclean_worker_is_rejected(self):
        self.fixture.terminal["workers"][0]["reap_confirmed"] = False
        self.assert_rejected()


class RecordingResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.successes = set()

    def addSuccess(self, test):
        self.successes.add(test.id())
        super().addSuccess(test)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--expect-geometry-red", action="store_true",
        help="succeed only for the exact intended geometry failure and four passing controls",
    )
    arguments = parser.parse_args()
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromTestCase(ProductionTargetEvidenceTests)
    required = {
        ProductionTargetEvidenceTests(name).id()
        for name in loader.getTestCaseNames(ProductionTargetEvidenceTests)
    }
    geometry = ProductionTargetEvidenceTests("test_control_relabelled_full_is_rejected").id()
    result = unittest.TextTestRunner(verbosity=2, resultclass=RecordingResult).run(suite)
    if not arguments.expect_geometry_red:
        return 0 if result.wasSuccessful() else 1
    intended_failure = (
        len(result.failures) == 1
        and result.failures[0][0].id() == geometry
        and result.failures[0][1].rstrip().endswith(
            "FullTargetGeometryRegression: " + GEOMETRY_FAILURE
        )
    )
    qualified_red = (
        result.testsRun == len(required) == 5
        and result.successes == required - {geometry}
        and intended_failure
        and not result.errors
        and not result.skipped
        and not result.expectedFailures
        and not result.unexpectedSuccesses
    )
    if qualified_red:
        print(SENTINEL + ": exact geometry RED with four passing synthetic controls")
        return 0
    print("Expected exactly the geometry RED and four passing synthetic controls", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
