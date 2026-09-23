#!/usr/bin/env python3
"""Dry probe: an exited Cargo-like leader must not leave its session child alive."""

from __future__ import annotations

import importlib.util
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from unittest import mock


RUNNER = Path(__file__).resolve().parents[2] / "scripts/test-native-pglite-rustfs-two-process.py"


def finder_mount_lines() -> list[str]:
    table = subprocess.run(["mount"], capture_output=True, text=True, check=True).stdout
    return [line for line in table.splitlines() if "mount-rs-finder-live" in line]


def process_live(pid: int, expected_session: int) -> bool:
    try:
        if os.getsid(pid) != expected_session:
            return False
    except ProcessLookupError:
        return False
    status = subprocess.run(
        ["ps", "-p", str(pid), "-o", "stat="],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    return bool(status) and not status.startswith("Z")


def outer_force_kill_probe() -> None:
    outer_code = """
import importlib.util,os,pathlib,sys,time
spec=importlib.util.spec_from_file_location('native_pglite_runner',sys.argv[1])
runner=importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
child=runner.spawn_supervised_child(
    [sys.executable,'-c','import time;time.sleep(30)'],
    cwd=pathlib.Path.cwd(),env=os.environ.copy())
print(child.pid,os.getsid(child.pid),flush=True)
time.sleep(30)
"""
    finder_before = finder_mount_lines()
    unrelated = subprocess.Popen(
        [sys.executable, "-c", "import time;time.sleep(30)"],
        start_new_session=True,
    )
    outer = subprocess.Popen(
        [sys.executable, "-c", outer_code, str(RUNNER)],
        stdout=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    child_pid, child_session = map(int, outer.stdout.readline().split())
    try:
        assert os.getsid(outer.pid) == outer.pid
        os.killpg(outer.pid, signal.SIGKILL)  # Only this dry outer command group.
        outer.wait(timeout=5)
        deadline = time.monotonic() + 5
        while process_live(child_pid, child_session) and time.monotonic() < deadline:
            time.sleep(0.05)
        assert not process_live(child_pid, child_session), (
            "outer force-kill left the supervised child running in a detached session"
        )
        assert unrelated.poll() is None, "outer cleanup signaled an unrelated session"
        assert finder_mount_lines() == finder_before, "outer cleanup changed Finder mount"
        print("NATIVE_PGLITE_RUNNER_OUTER_FORCE_KILL_PASS", flush=True)
    finally:
        if process_live(child_pid, child_session):
            os.kill(child_pid, signal.SIGTERM)  # Exact detached dry child in RED case.
        if outer.poll() is None:
            os.killpg(outer.pid, signal.SIGTERM)
        outer.wait(timeout=5)
        if unrelated.poll() is None:
            unrelated.terminate()
        unrelated.wait(timeout=5)


def symlinked_mountpoint_cleanup_probe() -> None:
    spec = importlib.util.spec_from_file_location("native_pglite_runner", RUNNER)
    assert spec and spec.loader
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    for redirected_name in ("mount-a", "mount-b"):
        with tempfile.TemporaryDirectory(prefix="mount-rs-pglite-symlink-dry-") as parent_name:
            parent = Path(parent_name)
            root = parent / "owned-native"
            root.mkdir(mode=0o700)
            (root / ".mount-rs-pglite-rustfs-owned").write_text(f"{os.getpid()}\n")
            scope = root / "mount-rs-cli-pglite-two-process-dry"
            scope.mkdir()
            for name in ("mount-a", "mount-b"):
                if name != redirected_name:
                    (scope / name).mkdir()
            unrelated = parent / "unrelated-mount-target"
            unrelated.mkdir()
            (scope / redirected_name).symlink_to(unrelated, target_is_directory=True)
            with (
                mock.patch.object(runner, "is_mounted") as mounted,
                mock.patch.object(runner.subprocess, "run") as run_command,
            ):
                try:
                    runner.cleanup_owned_native_root(root)
                except RuntimeError as error:
                    assert "symlink" in str(error), error
                else:
                    raise AssertionError("symlinked owned mountpoint was accepted for cleanup")
            assert root.exists(), "uncertain owned root must be preserved"
            assert unrelated.exists(), "unrelated target must remain untouched"
            mounted.assert_not_called()
            run_command.assert_not_called()
        print("NATIVE_PGLITE_RUNNER_SYMLINKED_MOUNTPOINT_REJECT_PASS", flush=True)


def run() -> None:
    spec = importlib.util.spec_from_file_location("native_pglite_runner", RUNNER)
    assert spec and spec.loader
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)

    child_code = """
import pathlib,signal,sys,time
marker=pathlib.Path(sys.argv[1])
def stop(_signal,_frame):
    marker.write_text('terminated\\n')
    sys.exit(0)
signal.signal(signal.SIGTERM,stop)
print('ready',flush=True)
time.sleep(30)
"""
    leader_code = """
import subprocess,sys
child=subprocess.Popen([sys.executable,'-c',sys.argv[2],sys.argv[1]],stdout=subprocess.PIPE,text=True)
assert child.stdout.readline().strip()=='ready'
print(child.pid,flush=True)
"""
    with tempfile.TemporaryDirectory(prefix="mount-rs-pglite-group-probe-") as temp:
        marker = Path(temp) / "child-stopped"
        unrelated = subprocess.Popen(
            [sys.executable, "-c", "import time;time.sleep(30)"],
            start_new_session=True,
        )
        leader = subprocess.Popen(
            [sys.executable, "-c", leader_code, str(marker), child_code],
            stdout=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        child_pid = int(leader.stdout.readline().strip())
        try:
            assert leader.wait(timeout=5) == 0
            assert os.getsid(child_pid) == leader.pid
            assert os.getpgid(child_pid) == leader.pid
            assert not marker.exists(), "child must remain live after leader exits"
            try:
                runner.stop_child_and_check(leader, leader.pid, leader.pid, set())
            except RuntimeError as error:
                assert "descendants remain" in str(error), error
            else:
                raise AssertionError("exited leader's live child was not detected")
            os.killpg(leader.pid, signal.SIGTERM)  # Simulate exact outer cleanup.
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            assert marker.exists(), "exited leader left its child alive"
            assert unrelated.poll() is None, "outer cleanup signaled an unrelated session"
            print("NATIVE_PGLITE_RUNNER_EXITED_LEADER_CHILD_CLEAN_PASS", flush=True)
        finally:
            if not marker.exists():
                try:
                    os.kill(child_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            if unrelated.poll() is None:
                unrelated.terminate()
            unrelated.wait(timeout=5)


if __name__ == "__main__":
    run()
    outer_force_kill_probe()
    symlinked_mountpoint_cleanup_probe()
