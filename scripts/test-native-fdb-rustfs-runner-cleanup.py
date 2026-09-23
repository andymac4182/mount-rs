#!/usr/bin/env python3
"""Dry failure-path probes for the native FDB/RustFS runner's owned cleanup."""

from __future__ import annotations

import importlib.util
import os
import plistlib
import shlex
import signal
import subprocess
import tempfile
from pathlib import Path
from unittest import mock


RUNNER = Path(__file__).with_name("test-native-fdb-rustfs-two-process.py")
spec = importlib.util.spec_from_file_location("native_fdb_rustfs_runner", RUNNER)
assert spec and spec.loader
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def marked_native_root(parent: Path) -> tuple[Path, Path]:
    root = parent / "native"
    root.mkdir(mode=0o700)
    (root / ".mount-rs-native-fdb-rustfs-owned").write_text(f"{os.getpid()}\n")
    scope = root / "mount-rs-cli-two-process-dry-probe"
    (scope / "mount-a").mkdir(parents=True)
    (scope / "mount-b").mkdir()
    return root, scope


def probe_exact_unmount_and_preservation() -> None:
    with tempfile.TemporaryDirectory(prefix="mount-rs-native-fdb-cleanup-dry-") as parent_name:
        parent = Path(parent_name)
        root, scope = marked_native_root(parent)
        with (
            mock.patch.object(runner, "is_mounted", side_effect=[True, False, False, False]),
            mock.patch.object(runner.subprocess, "run") as run,
        ):
            runner.cleanup_owned_native_root(root)
        assert not root.exists()
        run.assert_called_once()
        assert run.call_args.args[0] == ["umount", "-f", str(scope / "mount-a")]

        root, scope = marked_native_root(parent)
        with (
            mock.patch.object(runner, "is_mounted", return_value=True),
            mock.patch.object(runner.subprocess, "run") as run,
        ):
            try:
                runner.cleanup_owned_native_root(root)
            except RuntimeError as error:
                assert "attached mount" in str(error)
            else:
                raise AssertionError("attached owned mount was removed")
        assert root.exists(), "attached mount's owned root must be preserved"
        run.assert_called_once()
        assert run.call_args.args[0] == ["umount", "-f", str(scope / "mount-a")]


def probe_symlink_mountpoint_preserves_owned_root_and_foreign_mount() -> None:
    for replaced in ("mount-a", "mount-b"):
        with tempfile.TemporaryDirectory(prefix="mount-rs-native-fdb-symlink-dry-") as parent_name:
            parent = Path(parent_name)
            root, scope = marked_native_root(parent)
            unrelated_mount = parent / "preexisting-finder-mount"
            unrelated_mount.mkdir()
            (scope / replaced).rmdir()
            (scope / replaced).symlink_to(unrelated_mount, target_is_directory=True)

            with (
                mock.patch.object(runner, "is_mounted", return_value=True) as mounted,
                mock.patch.object(runner.subprocess, "run") as run,
            ):
                try:
                    runner.cleanup_owned_native_root(root)
                except RuntimeError as error:
                    assert "symlink" in str(error)
                else:
                    raise AssertionError(f"symlinked {replaced} allowed owned cleanup")
            assert root.exists(), "ambiguous owned root must be preserved"
            assert unrelated_mount.is_dir(), "unrelated Finder mount path must remain"
            mounted.assert_not_called()
            run.assert_not_called()


def probe_exited_leader_with_live_owned_child() -> None:
    with tempfile.TemporaryDirectory(prefix="mount-rs-native-fdb-process-dry-") as parent_name:
        run_dir = Path(parent_name)
        executable = (
            run_dir / "target" / "debug" / "deps" / "native_two_process_foundationdb-dry-probe"
        )
        executable.parent.mkdir(parents=True)
        executable.symlink_to("/bin/sleep")
        leader = subprocess.Popen(
            ["/bin/sh", "-c", f"{shlex.quote(str(executable))} 30 &"],
            start_new_session=True,
        )
        try:
            leader.wait(timeout=5)
            found = runner.owned_process_pids(run_dir, Path("/bin/fdbserver"))
            assert found, "an exited group leader must not hide its owned child"
            runner.stop_owned_processes(run_dir, Path("/bin/fdbserver"))
            assert not runner.owned_process_pids(run_dir, Path("/bin/fdbserver"))
        finally:
            try:
                os.killpg(leader.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def probe_foundationdb_cluster_id_grammar() -> None:
    assert runner.cluster_connection_string("vs06sczd", 4500) == (
        "mount_rs:vs06sczd@127.0.0.1:4500\n"
    )
    for run_id in ("mount_rs_vs06sczd", "bad-id", ""):
        try:
            runner.cluster_connection_string(run_id, 4500)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid FoundationDB cluster ID: {run_id}")


def probe_bounded_native_test_timeout() -> None:
    assert runner.native_test_timeout_seconds(None) == 600
    for value in ("1", "1200", "1800"):
        assert runner.native_test_timeout_seconds(value) == int(value)
    for value in ("0", "1801", "-1", "", "many", "1.5"):
        try:
            runner.native_test_timeout_seconds(value)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid native test timeout: {value}")


def probe_owned_apfs_detach_and_unrelated_finder_preservation() -> None:
    for fail_detach in (False, True):
        with tempfile.TemporaryDirectory(prefix="mount-rs-native-fdb-rustfs-") as parent_name:
            run_dir = Path(parent_name)
            (run_dir / ".mount-rs-native-fdb-rustfs-owned").write_text(f"{os.getpid()}\n")
            data_dir = run_dir / "data"
            data_dir.mkdir()
            native_root = run_dir / "native"
            image = run_dir / "fdb-data.sparseimage"
            image.touch()
            attached = True
            detach_devices: list[str] = []
            unrelated = {
                "image-path": "/private/tmp/preexisting-finder-demo.sparseimage",
                "owner-uid": os.getuid(),
                "image-type": "sparse disk image",
                "system-entities": [
                    {"dev-entry": "/dev/disk3"},
                    {"dev-entry": "/dev/disk4s1", "mount-point": "/Volumes/FinderDemo"},
                ],
            }
            owned = {
                "image-path": str(image),
                "owner-uid": os.getuid(),
                "image-type": "sparse disk image",
                "system-entities": [
                    {"dev-entry": "/dev/disk99"},
                    {"dev-entry": "/dev/disk100s1", "mount-point": str(data_dir.resolve())},
                ],
            }

            def hdiutil(args: list[str], **_kwargs: object) -> subprocess.CompletedProcess[bytes]:
                nonlocal attached
                if args[:3] == ["hdiutil", "info", "-plist"]:
                    images = [unrelated, owned] if attached else [unrelated]
                    return subprocess.CompletedProcess(args, 0, plistlib.dumps({"images": images}))
                if args[:2] == ["hdiutil", "detach"]:
                    detach_devices.append(args[2])
                    if fail_detach:
                        return subprocess.CompletedProcess(args, 1, b"")
                    attached = False
                    return subprocess.CompletedProcess(args, 0, b"")
                raise AssertionError(f"unexpected dry subprocess: {args}")

            with (
                mock.patch.object(runner.subprocess, "run", side_effect=hdiutil),
                mock.patch.object(runner, "is_mounted", return_value=False),
            ):
                try:
                    runner.remove_owned_run_dir_if_detached(run_dir, data_dir, native_root)
                except RuntimeError as error:
                    assert "attached data image" in str(error)
                else:
                    raise AssertionError("deleted owned run directory before detaching its image")
                assert run_dir.exists()
                try:
                    runner.detach_owned_fdb_data_image(run_dir, data_dir)
                except RuntimeError as error:
                    assert fail_detach and "preserving attached" in str(error)
                else:
                    assert not fail_detach
                    runner.remove_owned_run_dir_if_detached(run_dir, data_dir, native_root)
                    assert not run_dir.exists()
            assert detach_devices == ["/dev/disk99"]
            if fail_detach:
                assert run_dir.exists(), "failed detach must preserve its private root/image"
            assert unrelated["system-entities"][0]["dev-entry"] == "/dev/disk3"


def probe_stop_failure_preserves_owned_apfs_image() -> None:
    with tempfile.TemporaryDirectory(prefix="mount-rs-native-fdb-rustfs-") as parent_name:
        run_dir = Path(parent_name)
        data_dir = run_dir / "data"
        native_root = run_dir / "native"
        (run_dir / "fdb-data.sparseimage").touch()
        with (
            mock.patch.object(runner, "cleanup_owned_native_root") as clean_nfs,
            mock.patch.object(runner, "detach_owned_fdb_data_image") as detach,
            mock.patch.object(runner, "remove_owned_run_dir_if_detached") as remove,
        ):
            try:
                runner.finish_owned_run_cleanup(
                    run_dir, data_dir, native_root, processes_stopped=False
                )
            except RuntimeError as error:
                assert "processes are still live" in str(error)
            else:
                raise AssertionError("live process did not prevent owned image cleanup")
            clean_nfs.assert_called_once_with(native_root, remove=False)
            detach.assert_not_called()
            remove.assert_not_called()
            assert run_dir.exists(), "unproved process exit must preserve private root/image"


def probe_partial_pre_attach_setup_removes_only_its_new_root() -> None:
    with tempfile.TemporaryDirectory(prefix="mount-rs-fdb-partial-dry-") as parent_name:
        candidate = Path(parent_name) / "mount-rs-native-fdb-rustfs-partial"
        candidate.mkdir(mode=0o700)
        original_mkdir = Path.mkdir

        def fail_data_mkdir(path: Path, *args: object, **kwargs: object) -> None:
            if path.name == "data":
                raise OSError("dry injected ENOSPC before APFS attachment")
            original_mkdir(path, *args, **kwargs)

        with (
            mock.patch.object(runner.tempfile, "mkdtemp", return_value=str(candidate)),
            mock.patch.object(runner.Path, "mkdir", new=fail_data_mkdir),
        ):
            try:
                runner.create_owned_native_fixture(4500)
            except OSError as error:
                assert "dry injected ENOSPC" in str(error)
            else:
                raise AssertionError("partial private setup unexpectedly succeeded")
        assert not candidate.exists(), "fresh partial fixture root must be removed"


if __name__ == "__main__":
    probe_exact_unmount_and_preservation()
    probe_symlink_mountpoint_preserves_owned_root_and_foreign_mount()
    probe_exited_leader_with_live_owned_child()
    probe_foundationdb_cluster_id_grammar()
    probe_bounded_native_test_timeout()
    probe_owned_apfs_detach_and_unrelated_finder_preservation()
    probe_stop_failure_preserves_owned_apfs_image()
    probe_partial_pre_attach_setup_removes_only_its_new_root()
    print("NATIVE_FDB_RUSTFS_DRY_CLEANUP_PASS")
