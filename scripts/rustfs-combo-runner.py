#!/usr/bin/env python3
"""Run one provider-composition command in a bounded process group."""

from __future__ import annotations

import os
from pathlib import Path
import signal
import subprocess
import sys
import time

TIMEOUT_EXIT = 124
GROUP_CLEANUP_EXIT = 125
DEFAULT_GROUP_STOP_SECONDS = 5.0


def group_exists(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # macOS can report EPERM for a process group that has just lost its
        # leader. Confirm membership through the portable ps interface rather
        # than treating that transient result as a live descendant.
        try:
            result = subprocess.run(
                ["ps", "-eo", "pgid="],
                capture_output=True,
                text=True,
                check=False,
                timeout=1.0,
            )
        except (OSError, subprocess.TimeoutExpired):
            # Failure to prove that the group is gone is live-state evidence.
            return True
        if result.returncode != 0:
            return True
        return any(line.strip() == str(pgid) for line in result.stdout.splitlines())
    return True


def signal_group(pgid: int, signum: signal.Signals) -> None:
    try:
        os.killpg(pgid, signum)
    except (PermissionError, ProcessLookupError):
        pass


def wait_group_gone(pgid: int, seconds: float) -> bool:
    deadline = time.monotonic() + seconds
    while group_exists(pgid):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)
    return True


def stop_group(process: subprocess.Popen[object], group_stop_seconds: float) -> bool:
    pgid = process.pid
    signal_group(pgid, signal.SIGTERM)
    try:
        process.wait(timeout=group_stop_seconds)
    except subprocess.TimeoutExpired:
        pass
    group_gone = wait_group_gone(pgid, group_stop_seconds)
    if not group_gone:
        signal_group(pgid, signal.SIGKILL)
        try:
            process.wait(timeout=group_stop_seconds)
        except subprocess.TimeoutExpired:
            pass
        group_gone = wait_group_gone(pgid, group_stop_seconds)
    if process.poll() is None:
        return False
    return group_gone


def normalized_returncode(returncode: int) -> int:
    return returncode if returncode >= 0 else 128 + (-returncode)


def main(argv: list[str]) -> int:
    if len(argv) != 6:
        print(
            "usage: rustfs-combo-runner.py TIMEOUT_SECONDS REPO_DIR PID_FILE NAME COMMAND",
            file=sys.stderr,
        )
        return 2

    try:
        timeout_seconds = int(argv[1])
    except ValueError:
        print("TIMEOUT_SECONDS must be an integer", file=sys.stderr)
        return 2
    if timeout_seconds < 0:
        print("TIMEOUT_SECONDS must be non-negative", file=sys.stderr)
        return 2

    repo_dir = argv[2]
    pid_file = Path(argv[3])
    name = argv[4]
    command = argv[5]
    try:
        group_stop_seconds = float(
            os.environ.get(
                "RUSTFS_COMBO_GROUP_STOP_SECONDS",
                str(DEFAULT_GROUP_STOP_SECONDS),
            )
        )
    except ValueError:
        print("RUSTFS_COMBO_GROUP_STOP_SECONDS must be a positive number", file=sys.stderr)
        return 2
    if group_stop_seconds <= 0:
        print("RUSTFS_COMBO_GROUP_STOP_SECONDS must be a positive number", file=sys.stderr)
        return 2
    process: subprocess.Popen[object] | None = None

    def interrupted(signum: int, _frame: object) -> None:
        if process is not None:
            stopped = stop_group(process, group_stop_seconds)
            if not stopped:
                print(f"RUSTFS_COMBO_GROUP_CLEANUP_FAIL name={name}", file=sys.stderr)
                raise SystemExit(GROUP_CLEANUP_EXIT)
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)

    try:
        process = subprocess.Popen(
            ["/bin/sh", "-c", command],
            cwd=repo_dir,
            env=os.environ.copy(),
            start_new_session=True,
        )
        pid_file.write_text(f"{process.pid}\n", encoding="ascii")
        try:
            returncode = process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            stopped = stop_group(process, group_stop_seconds)
            if not stopped:
                print(f"RUSTFS_COMBO_GROUP_CLEANUP_FAIL name={name}", file=sys.stderr)
                return GROUP_CLEANUP_EXIT
            print(f"RUSTFS_COMBO_TIMEOUT name={name}", file=sys.stderr)
            return TIMEOUT_EXIT

        if not wait_group_gone(process.pid, group_stop_seconds):
            stopped = stop_group(process, group_stop_seconds)
            if not stopped:
                print(f"RUSTFS_COMBO_GROUP_CLEANUP_FAIL name={name}", file=sys.stderr)
                return GROUP_CLEANUP_EXIT
            print(f"RUSTFS_COMBO_GROUP_LEAK name={name}", file=sys.stderr)
            return GROUP_CLEANUP_EXIT

        normalized = normalized_returncode(returncode)
        if normalized == 0:
            print(f"RUSTFS_COMBO_PASS name={name}")
        else:
            print(f"RUSTFS_COMBO_FAIL name={name} status={normalized}", file=sys.stderr)
        return normalized
    finally:
        try:
            keep_pid_file = process is not None and group_exists(process.pid)
            if not keep_pid_file and pid_file.read_text(encoding="ascii").strip() == str(
                process.pid if process else ""
            ):
                pid_file.unlink()
        except (FileNotFoundError, OSError):
            pass


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
