#!/usr/bin/env python3
"""Run two native PGlite-metadata/RustFS-block CLIs in the disposable RustFS harness."""

from __future__ import annotations

import os
import shutil
import signal
import stat
import subprocess
import tempfile
import time
from pathlib import Path
from urllib.parse import urlsplit


REPO = Path(__file__).resolve().parent.parent
TEST_NAME = "cli_two_process_pglite_metadata_rustfs_blocks_stays_coherent_under_load_and_reopens"


def required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"run from scripts/test-rustfs.sh with nonempty {name}")
    return value


def spawn_supervised_child(
    argv: list[str], *, cwd: Path, env: dict[str, str]
) -> subprocess.Popen[bytes]:
    # Inherit rustfs-combo-runner's private session and process group. Its
    # forced group cleanup must reach Cargo, the native test, CLI, and Node.
    return subprocess.Popen(argv, cwd=cwd, env=env)


def verify_harness() -> None:
    run_dir = Path(required("RUSTFS_RUN_DIR"))
    container = required("RUSTFS_HARNESS_CONTAINER")
    marker = run_dir / ".mount-rs-rustfs-owned"
    if run_dir.is_symlink() or marker.is_symlink() or not marker.is_file():
        raise RuntimeError("RustFS harness ownership marker is unavailable")
    if marker.read_text().strip() != container:
        raise RuntimeError("RustFS harness ownership marker does not match container")

    endpoint = urlsplit(required("RUSTFS_ENDPOINT"))
    if (
        endpoint.scheme != "http"
        or endpoint.hostname != "127.0.0.1"
        or not endpoint.port
        or endpoint.path not in ("", "/")
        or endpoint.query
        or endpoint.fragment
        or endpoint.username
        or endpoint.password
    ):
        raise RuntimeError("disposable RustFS endpoint must be loopback HTTP")
    for name in (
        "RUSTFS_BUCKET",
        "RUSTFS_REGION",
        "RUSTFS_ACCESS_KEY_ID",
        "RUSTFS_SECRET_ACCESS_KEY",
        "RUSTFS_TEST_PREFIX",
    ):
        required(name)


def live_supervised_members(group_id: int, session: int) -> set[int]:
    """Find children in the exact outer combo group and fail on moved members."""
    table = subprocess.run(
        ["ps", "-A", "-o", "pid=,pgid=,stat="],
        capture_output=True,
        text=True,
        check=True,
        timeout=10,
    ).stdout
    members: set[int] = set()
    for line in table.splitlines():
        fields = line.split()
        if len(fields) != 3:
            raise RuntimeError("cannot parse macOS process-session snapshot")
        pid, process_group = map(int, fields[:2])
        state = fields[2]
        try:
            owner_session = os.getsid(pid)
        except ProcessLookupError:
            continue  # It exited between ps and getsid.
        if process_group == group_id and owner_session != session:
            raise RuntimeError("combo process-group ID belongs to another session")
        if owner_session == session and process_group != group_id and not state.startswith("Z"):
            raise RuntimeError("combo child moved outside the supervised process group")
        if owner_session == session and not state.startswith("Z"):
            members.add(pid)
    return members


def stop_child_and_check(
    process: subprocess.Popen[bytes], group: int, session: int, exempt: set[int]
) -> None:
    # The Python runner must never signal its own outer group. It can stop
    # only the exact Cargo leader; the outer watchdog owns group termination.
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)
    deadline = time.monotonic() + 5
    while True:
        orphans = live_supervised_members(group, session) - exempt
        if not orphans:
            return
        if time.monotonic() >= deadline:
            raise RuntimeError(f"combo descendants remain for outer cleanup: {orphans}")
        time.sleep(0.1)


def is_mounted(target: Path) -> bool:
    original_identity = require_plain_mountpoint(target)
    table = subprocess.run(
        ["mount"], capture_output=True, text=True, check=True, timeout=10
    ).stdout
    if require_plain_mountpoint(target) != original_identity:
        raise RuntimeError(f"preserving changed native mountpoint: {target}")
    canonical = target.resolve()
    for line in table.splitlines():
        _, separator, mounted = line.partition(" on ")
        if separator and " (" in mounted:
            entry = Path(mounted.rsplit(" (", 1)[0])
            if entry.resolve() == canonical:
                return True
    return False


def require_plain_mountpoint(target: Path) -> tuple[int, int]:
    try:
        metadata = target.lstat()
    except OSError as error:
        raise RuntimeError(f"preserving missing or unreadable native mountpoint: {target}") from error
    if not stat.S_ISDIR(metadata.st_mode):
        kind = "symlink" if stat.S_ISLNK(metadata.st_mode) else "non-directory"
        raise RuntimeError(f"preserving {kind} native mountpoint: {target}")
    return metadata.st_dev, metadata.st_ino


def cleanup_owned_native_root(root: Path) -> None:
    marker = root / ".mount-rs-pglite-rustfs-owned"
    if root.is_symlink() or marker.is_symlink() or marker.read_text().strip() != str(os.getpid()):
        raise RuntimeError(f"preserving native root without matching ownership: {root}")
    mountpoints: list[tuple[Path, tuple[int, int]]] = []
    for scope in root.glob("mount-rs-cli-pglite-two-process-*"):
        if scope.is_symlink() or not scope.is_dir():
            raise RuntimeError(f"preserving unexpected native scope: {scope}")
        for name in ("mount-a", "mount-b"):
            mountpoint = scope / name
            mountpoints.append((mountpoint, require_plain_mountpoint(mountpoint)))
    # Validate both run-owned endpoints before looking up or unmounting either.
    for mountpoint, original_identity in mountpoints:
        mounted = is_mounted(mountpoint)
        if require_plain_mountpoint(mountpoint) != original_identity:
            raise RuntimeError(f"preserving changed native mountpoint: {mountpoint}")
        if mounted:
            subprocess.run(
                ["umount", "-f", str(mountpoint)],
                capture_output=True,
                text=True,
                check=False,
                timeout=15,
            )
        if is_mounted(mountpoint):
            raise RuntimeError(f"preserving native root with attached mount: {mountpoint}")
    shutil.rmtree(root)


def main() -> None:
    def interrupted(_signum: int, _frame: object) -> None:
        raise KeyboardInterrupt("native PGlite/RustFS runner interrupted")

    signal.signal(signal.SIGTERM, interrupted)
    verify_harness()
    outer_group = os.getpgrp()
    outer_session = os.getsid(0)
    if outer_group != outer_session:
        raise RuntimeError("run inside rustfs-combo-runner's supervised process group")
    exempt = {os.getpid(), outer_group}  # Python and its optional /bin/sh leader.
    native_root = Path(tempfile.mkdtemp(prefix="mount-rs-native-pglite-rustfs-"))
    native_root.chmod(0o700)
    (native_root / ".mount-rs-pglite-rustfs-owned").write_text(f"{os.getpid()}\n")
    env = os.environ.copy()
    env["MOUNT_RS_CLI_NATIVE_NFS"] = "1"
    env["MOUNT_RS_CLI_NATIVE_PGLITE_TWO_PROCESS"] = "1"
    env["MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE"] = "1"
    env["CARGO_TARGET_DIR"] = "/private/tmp/mount-rs-pglite-rustfs-target"
    env["TMPDIR"] = str(native_root)
    cargo: subprocess.Popen[bytes] | None = None
    passed = False
    try:
        cargo = spawn_supervised_child(
            [
                str(REPO / "scripts/cargo-shared"),
                "test",
                "--locked",
                "-p",
                "mount-rs-cli",
                "--test",
                "native_two_process_pglite",
                "--",
                TEST_NAME,
                "--exact",
                "--ignored",
                "--nocapture",
            ],
            cwd=REPO,
            env=env,
        )
        try:
            result = cargo.wait(timeout=600)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("native two-CLI PGlite/RustFS test exceeded 600 seconds") from error
        if result != 0:
            raise RuntimeError(f"native two-CLI PGlite/RustFS test exited {result}")
        passed = True
    finally:
        if cargo is not None:
            try:
                stop_child_and_check(cargo, outer_group, outer_session, exempt)
            except BaseException:
                print(f"NATIVE_PGLITE_RUSTFS_PRESERVED_ROOT {native_root}", flush=True)
                raise
        cleanup_owned_native_root(native_root)
    if passed:
        print("NATIVE_PGLITE_RUSTFS_TWO_PROCESS_PASS", flush=True)
        print("NATIVE_PGLITE_RUSTFS_OWNED_NFS_AND_TEMP_CLEAN", flush=True)


if __name__ == "__main__":
    main()
