#!/usr/bin/env python3
"""Retain the two expected policy failures before changing filesystem writeback."""

import hashlib
import json
import os
import pathlib
import re
import selectors
import signal
import subprocess
import time


SECONDS = 300
STREAM_BYTES = 64 * 1024 * 1024
CASES = (
    (
        "contract",
        ["--test", "contract"],
        "filesystem_writeback_acknowledgments_do_not_claim_stable_storage",
        "FS_WRITEBACK_DURABILITY_REGRESSION",
    ),
    (
        "stages",
        ["--lib"],
        "unix::tests::filesystem_writeback_puts_do_not_start_forced_sync_stages",
        "FS_WRITEBACK_SYNC_REGRESSION",
    ),
)
SOURCES = (
    "providers/mount-rs-filesystem-blocks/src/unix.rs",
    "providers/mount-rs-filesystem-blocks/src/unix/tests.rs",
    "providers/mount-rs-filesystem-blocks/tests/contract.rs",
    "scripts/check-filesystem-writeback-red.py",
    ".github/workflows/filesystem-writeback.yml",
)


def pins():
    return {name: hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest() for name in SOURCES}


def group_present(pid):
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return False
    return True


def signal_group(pid, sig):
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        pass


def run_case(root, case):
    label, selection, name, sentinel = case
    command = [
        "./scripts/cargo-shared", "test", "--locked", "-p", "mount-rs-filesystem-blocks",
        *selection, name, "--", "--exact", "--nocapture", "--color", "never",
    ]
    streams = {}
    counts = {"stdout": 0, "stderr": 0}
    start = time.monotonic()
    before = pins()
    process = None
    stop = None
    forced = False
    error = None
    eof = {"stdout": False, "stderr": False}
    selector = selectors.DefaultSelector()
    try:
        for stream in counts:
            fd = os.open(root / (label + "." + stream), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            streams[stream] = os.fdopen(fd, "wb")
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        for stream in counts:
            pipe = getattr(process, stream)
            os.set_blocking(pipe.fileno(), False)
            selector.register(pipe, selectors.EVENT_READ, stream)
        stopped_at = None
        while selector.get_map() or process.poll() is None:
            now = time.monotonic()
            if now - start >= SECONDS and stop is None:
                stop = "deadline"
            if stop is not None and stopped_at is None:
                stopped_at = now
                if group_present(process.pid):
                    signal_group(process.pid, signal.SIGTERM)
            if stopped_at is not None and now - stopped_at >= 2 and group_present(process.pid):
                forced = True
                signal_group(process.pid, signal.SIGKILL)
            if stopped_at is not None and now - stopped_at >= 7:
                raise RuntimeError("owned group or capture pipes did not settle")
            for key, _events in selector.select(0.1):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    eof[key.data] = True
                    selector.unregister(key.fileobj)
                    key.fileobj.close()
                    continue
                stream = key.data
                counts[stream] += len(data)
                if counts[stream] > STREAM_BYTES:
                    if stop is None:
                        stop = "stream_limit"
                else:
                    streams[stream].write(data)
        exit_code = process.wait(timeout=2)
        if group_present(process.pid):
            raise RuntimeError("owned group remains after compiler and test exit")
        for file in streams.values():
            file.close()
    except BaseException as failure:
        error = type(failure).__name__
    finally:
        if process is not None:
            try:
                if group_present(process.pid):
                    forced = True
                    signal_group(process.pid, signal.SIGKILL)
                if process.poll() is None:
                    process.wait(timeout=2)
                settlement_deadline = time.monotonic() + 2
                while group_present(process.pid) and time.monotonic() < settlement_deadline:
                    time.sleep(0.05)
            except BaseException as failure:
                error = error or "Cleanup" + type(failure).__name__
            for pipe in (process.stdout, process.stderr):
                try:
                    pipe.close()
                except BaseException as failure:
                    error = error or "PipeClose" + type(failure).__name__
        try:
            selector.close()
        except BaseException as failure:
            error = error or "SelectorClose" + type(failure).__name__
        for file in streams.values():
            try:
                file.close()
            except BaseException as failure:
                error = error or "CaptureClose" + type(failure).__name__
    group_absent = False
    try:
        group_absent = process is not None and not group_present(process.pid)
    except OSError as failure:
        error = error or "GroupCheck" + type(failure).__name__
    reaped = process is not None and process.returncode is not None
    if not group_absent or not reaped:
        error = error or "OwnedGroupSettlementUnconfirmed"
    raw = {}
    text = {}
    after = None
    try:
        for stream in counts:
            raw[stream] = (root / (label + "." + stream)).read_bytes()
            text[stream] = raw[stream].decode("utf-8", errors="strict")
        after = pins()
    except (OSError, UnicodeError) as failure:
        error = error or type(failure).__name__
    stdout = text.get("stdout", "")
    stderr = text.get("stderr", "")
    exit_code = process.returncode if process is not None else None
    expected = (
        error is None and stop is None and not forced and reaped and group_absent
        and all(eof.values()) and exit_code == 101 and before == after
        and re.search(r"(?m)^running 1 test$", stdout) is not None
        and re.search(r"(?m)^test " + re.escape(name) + r" \.\.\. FAILED$", stdout) is not None
        and re.search(r"(?m)^test result: FAILED\. 0 passed; 1 failed; 0 ignored;", stdout) is not None
        and sentinel in stdout + stderr
    )
    receipt = {
        "schema": "mount-rs.filesystem-writeback.expected-red.v1",
        "case": label, "command": command, "exit_code": exit_code,
        "checkout_sha": os.environ.get("GITHUB_SHA"),
        "elapsed_seconds": time.monotonic() - start, "stop": stop, "error": error,
        "forced": forced, "reaped": reaped, "group_absent": group_absent,
        "pipe_eof": eof, "source_before": before, "source_after": after,
        "stream_bytes": counts, "retained_bytes": {key: len(value) for key, value in raw.items()},
        "expected_policy_failure": expected,
        "stream_sha256": {key: hashlib.sha256(value).hexdigest() for key, value in raw.items()},
    }
    with (root / (label + ".json")).open("x") as file:
        json.dump(receipt, file, sort_keys=True)
        file.write("\n")
    print(json.dumps({"case": label, "expected_policy_failure": expected, "exit_code": exit_code}), flush=True)
    if not expected:
        raise RuntimeError("expected one executed policy assertion failure; inspect retained evidence")


def main():
    root = pathlib.Path(os.environ["MOUNT_RS_FILESYSTEM_WRITEBACK_EVIDENCE"])
    root.mkdir(mode=0o700)
    for case in CASES:
        run_case(root, case)


if __name__ == "__main__":
    main()
