#!/usr/bin/env python3
"""Run one Docker action with bounded process-group cleanup."""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import tempfile
import time

TIMEOUT_EXIT = 124
GROUP_CLEANUP_EXIT = 125
TERM_GRACE_SECONDS = 1.0
KILL_GRACE_SECONDS = 2.0


def group_exists(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
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


def stop_group(process: subprocess.Popen[object]) -> bool:
    pgid = process.pid
    signal_group(pgid, signal.SIGTERM)
    try:
        process.wait(timeout=TERM_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        pass

    group_gone = wait_group_gone(pgid, TERM_GRACE_SECONDS)
    if not group_gone:
        signal_group(pgid, signal.SIGKILL)
        try:
            process.wait(timeout=KILL_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            pass
        group_gone = wait_group_gone(pgid, KILL_GRACE_SECONDS)

    return process.poll() is not None and group_gone


def normalized_returncode(returncode: int) -> int:
    return returncode if returncode >= 0 else 128 + (-returncode)


def main(argv: list[str]) -> int:
    if len(argv) < 4:
        print(
            "usage: rustfs-bounded-docker.py TIMEOUT_SECONDS ACTION COMMAND [ARGS...]",
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

    action = argv[2]
    command = argv[3:]
    process: subprocess.Popen[object] | None = None

    # A regular file avoids waiting on an inherited pipe if a broken command
    # daemonizes. The group is still killed and the result remains bounded.
    with tempfile.TemporaryFile() as output:
        try:
            process = subprocess.Popen(
                command,
                stdout=output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        except OSError as error:
            output.write(f"{error}\n".encode())
            returncode = 127
        else:
            try:
                returncode = process.wait(timeout=timeout_seconds)
            except subprocess.TimeoutExpired:
                stopped = stop_group(process)
                output.flush()
                output.seek(0)
                sys.stdout.buffer.write(output.read())
                if not stopped:
                    print(
                        f"RUSTFS_DOCKER_GROUP_CLEANUP_FAIL action={action}",
                        file=sys.stderr,
                    )
                    return GROUP_CLEANUP_EXIT
                print(f"RUSTFS_DOCKER_TIMEOUT action={action}", file=sys.stderr)
                return TIMEOUT_EXIT

            if not wait_group_gone(process.pid, KILL_GRACE_SECONDS):
                stopped = stop_group(process)
                output.flush()
                output.seek(0)
                sys.stdout.buffer.write(output.read())
                if not stopped:
                    print(
                        f"RUSTFS_DOCKER_GROUP_CLEANUP_FAIL action={action}",
                        file=sys.stderr,
                    )
                    return GROUP_CLEANUP_EXIT
                print(f"RUSTFS_DOCKER_GROUP_LEAK action={action}", file=sys.stderr)
                return GROUP_CLEANUP_EXIT

        output.flush()
        output.seek(0)
        sys.stdout.buffer.write(output.read())
        return normalized_returncode(returncode)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
