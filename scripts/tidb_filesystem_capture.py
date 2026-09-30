"""Bounded SDK stdout capture and pure admission for the owned TiDB+FS selector.

Imports perform no filesystem, subprocess or network operations.
"""
import os
from pathlib import Path
import re
import sys

from native_backing_pair.artifacts import read_artifact
from native_backing_pair.contracts import ContractError

CAPTURE_CAP = 65536
SDK_NAME = "actual_tidb_filesystem_durable_peer_writes_reopen_and_root_authority"
PHASE_MARKERS = {
    "0": "TIDB_FILESYSTEM_SEED_PASS full_bytes=verified peer_contexts=verified fresh_reopen=verified root_authority=verified sql_blocks=0",
    "1": "TIDB_FILESYSTEM_REOPEN_PASS retained_backing=verified pre_constructor_sql=verified full_bytes=verified fresh_reopen=verified sql_blocks=0",
}
_SUMMARY = re.compile(
    r"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
    r"finished in (?:0|[1-9][0-9]*)(?:\.[0-9]+)?s\Z"
)


class CaptureError(ContractError):
    """The selected SDK invocation lacks complete passing evidence."""


def require(condition, label):
    if not condition:
        raise CaptureError(label)


def verify_sdk_capture(raw, phase, exit_code):
    require(type(phase) is str and phase in PHASE_MARKERS, "invalid_phase")
    require(type(exit_code) is int and exit_code == 0, "cargo_failed")
    require(type(raw) is bytes and 0 < len(raw) <= CAPTURE_CAP, "invalid_stdout")
    try:
        text = raw.decode("utf-8")
    except UnicodeError as error:
        raise CaptureError("invalid_stdout_utf8") from error
    require(not any(ch in text for ch in ("\x00", "\x1b", "\r")), "stdout_control_rewrite")
    lines = text.split("\n")
    running = [line for line in lines if line.startswith("running ")]
    require(running == ["running 1 test"], "selected_test_count")
    summaries = [line for line in lines if line.startswith("test result:")]
    require(len(summaries) == 1 and _SUMMARY.fullmatch(summaries[0]), "passing_inventory")
    results = [(index, line) for index, line in enumerate(lines)
               if line.startswith("test ") and not line.startswith("test result:")]
    require(len(results) == 1, "named_result_count")
    index, result = results[0]
    prefix = f"test {SDK_NAME} ... "
    require(result.startswith(prefix), "named_result")
    suffix = result[len(prefix):]
    marker = PHASE_MARKERS[phase]
    # --nocapture can print the SDK marker after libtest's un-terminated prefix.
    if suffix == marker:
        require(index + 1 < len(lines) and lines[index + 1] == "ok", "named_pass")
        markers = [suffix]
    else:
        require(suffix == "ok", "named_pass")
        markers = [line for line in lines if line == marker]
    require(markers == [marker], "phase_marker")
    for candidate_phase in PHASE_MARKERS:
        token = PHASE_MARKERS[candidate_phase].split(" ", 1)[0]
        require(text.count(token) == (1 if candidate_phase == phase else 0), "phase_marker_count")


def bounded_capture(source, sink):
    remaining = CAPTURE_CAP
    overflow = False
    while True:
        chunk = source.read(65536)
        if not chunk:
            return overflow
        if len(chunk) > remaining:
            overflow = True
        kept = chunk[:remaining]
        if kept:
            sink.write(kept)
            remaining -= len(kept)
        # Drain excess bytes without retaining/writing them; the existing job
        # deadline still owns runtime and subprocess containment.


def main(argv):
    require(len(argv) == 3, "capture_arguments")
    phase, capture_name, status_name = argv
    require(phase in PHASE_MARKERS, "invalid_phase")
    capture = Path(capture_name)
    status = Path(status_name)
    require(capture.is_absolute() and ".." not in capture.parts
            and status.parent == capture.parent
            and capture.name == f"filesystem-sdk-{phase}.stdout"
            and status.name == f"filesystem-sdk-{phase}.status", "owned_capture_paths")
    fd = os.open(capture, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    with os.fdopen(fd, "wb") as output:
        overflow = bounded_capture(sys.stdin.buffer, output)
        output.flush()
        os.fsync(output.fileno())
    captured = read_artifact(capture, cap=CAPTURE_CAP)
    status_read = read_artifact(status, cap=8)
    require(re.fullmatch(rb"(?:0|[1-9][0-9]{0,2})\n", status_read.data), "cargo_status_shape")
    exit_code = int(status_read.data)
    require(exit_code <= 255, "cargo_status_shape")
    # Replay only the descriptor-validated bounded bytes into the CI log.
    sys.stdout.buffer.write(captured.data)
    sys.stdout.buffer.flush()
    require(not overflow, "stdout_capture_cap")
    verify_sdk_capture(captured.data, phase, exit_code)
    print(f"TIDB_FILESYSTEM_SDK_CAPTURE_PASS phase={phase} test={SDK_NAME} sha256={captured.pin.sha256} bytes={captured.pin.size}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (ContractError, OSError, ValueError) as error:
        print(f"test-tidb filesystem capture rejected: {error}", file=sys.stderr)
        sys.exit(1)
