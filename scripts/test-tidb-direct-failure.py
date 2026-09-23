#!/usr/bin/env python3
"""Check the real TiDB harness function without starting any services.

Calling a shell function in an ``if`` condition disables ``set -e`` within
that function. The direct provider gate must explicitly preserve a failed
legacy contract before running the concurrent consumers.
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile


def direct_provider_function(source: str) -> str:
    match = re.search(
        r"^run_direct_provider_test\(\) \{\n.*?^\}", source, re.MULTILINE | re.DOTALL
    )
    if match is None:
        raise AssertionError("TiDB harness direct-provider function was not found")
    return match.group(0)


def run_probe(
    function: str, root: Path, *, legacy_status: int, concurrent_status: int
) -> tuple[int, list[str]]:
    trace = root / "calls.log"
    trace.unlink(missing_ok=True)
    runner = "\n".join(
        [
            "set -eu",
            f"repo_dir={shlex.quote(str(root))}",
            "tidb_url='mysql://root@127.0.0.1:1/test'",
            "volume_key='owned-dry-failure-probe'",
            function,
            # Match the harness call site, including the if condition that
            # suppresses errexit throughout the called function.
            "if run_direct_provider_test 1; then",
            "  direct_status=0",
            "else",
            "  direct_status=$?",
            "fi",
            'exit "$direct_status"',
        ]
    )
    environment = os.environ.copy()
    environment.update(
        {
            "MOUNT_RS_PROBE_TRACE": str(trace),
            "MOUNT_RS_PROBE_LEGACY_STATUS": str(legacy_status),
            "MOUNT_RS_PROBE_CONCURRENT_STATUS": str(concurrent_status),
        }
    )
    completed = subprocess.run(
        ["sh", "-c", runner],
        env=environment,
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    assert not completed.stdout, f"unexpected probe stdout: {completed.stdout!r}"
    assert not completed.stderr, f"unexpected probe stderr: {completed.stderr!r}"
    calls = trace.read_text().splitlines() if trace.exists() else []
    return completed.returncode, calls


def main() -> None:
    repo = Path(__file__).resolve().parent.parent
    function = direct_provider_function((repo / "scripts/test-tidb.sh").read_text())
    guard = ' || return "$?"'
    assert function.count(guard) == 1, "expected one explicit legacy failure guard"
    broken_control = function.replace(guard, "", 1)
    with tempfile.TemporaryDirectory(prefix="mount-rs-tidb-direct-failure-") as path:
        root = Path(path)
        scripts = root / "scripts"
        scripts.mkdir()
        stub_header = "\n".join(
            [
                "#!/bin/sh",
                "set -eu",
                '[ "$MOUNT_RS_TIDB_URL" = "mysql://root@127.0.0.1:1/test" ]',
                '[ "$MOUNT_RS_TIDB_TEST_VOLUME_KEY" = "owned-dry-failure-probe" ]',
                '[ "$MOUNT_RS_TIDB_EXPECT_PERSISTED" = 1 ]',
            ]
        )
        legacy = scripts / "cargo-shared"
        legacy.write_text(
            stub_header
            + '\nprintf "legacy\\n" >> "$MOUNT_RS_PROBE_TRACE"\n'
            + 'exit "$MOUNT_RS_PROBE_LEGACY_STATUS"\n'
        )
        legacy.chmod(0o700)
        concurrent = scripts / "test-tidb-concurrent-consumers.sh"
        concurrent.write_text(
            stub_header
            + '\nprintf "concurrent\\n" >> "$MOUNT_RS_PROBE_TRACE"\n'
            + 'exit "$MOUNT_RS_PROBE_CONCURRENT_STATUS"\n'
        )

        # The former unguarded function falsely passes after a failed direct
        # contract. Proving that behavior makes this a regression check of
        # actual shell control flow instead of a source-text assertion alone.
        observed = run_probe(
            broken_control, root, legacy_status=7, concurrent_status=0
        )
        assert observed == (0, ["legacy", "concurrent"]), observed
        print("TIDB_DIRECT_FAILURE_RED_CONTROL hidden_legacy_status=7 observed_status=0")

        cases = [
            (7, 0, (7, ["legacy"])),
            (0, 9, (9, ["legacy", "concurrent"])),
            (0, 0, (0, ["legacy", "concurrent"])),
        ]
        for legacy_status, concurrent_status, expected in cases:
            observed = run_probe(
                function,
                root,
                legacy_status=legacy_status,
                concurrent_status=concurrent_status,
            )
            assert observed == expected, (legacy_status, concurrent_status, observed)
        print("TIDB_DIRECT_FAILURE_PASS cases=3 legacy_failure_stops_consumers=true")


if __name__ == "__main__":
    main()
