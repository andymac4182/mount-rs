#!/usr/bin/env python3
"""Owned tempfile writer controls; no subprocesses or backend work are started."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("owned_layout_process.py")
spec = importlib.util.spec_from_file_location("owned_layout_process_writer_controls", MODULE_PATH)
if spec is None or spec.loader is None:
    raise AssertionError("pure comparison module must have an inert source loader")
api = importlib.util.module_from_spec(spec)
spec.loader.exec_module(api)


def absent_writer(_path: Path, _receipt: dict[str, object]) -> None:
    """An inert missing-feature sentinel makes the first writer RED semantic."""


writer = getattr(api, "write_private_process_receipt", absent_writer)
WRITE_FAILED = "OWNED_LAYOUT_PROCESS_RECEIPT_WRITE_FAILED"


def clean_receipt() -> dict[str, object]:
    return {
        "schema": "mount-rs.owned-layout-process.v1",
        "action_id": "container.inspect",
        "pid": 24680,
        "pgid": 24680,
        "raw_returncode": 0,
        "normalized_exit": 0,
        "supervisor_signal": None,
        "child_return_signal": None,
        "wait_unavailable_observed": False,
        "deadline_exceeded": False,
        "group_leak_observed": False,
        "group_probe_unavailable_observed": False,
        "retirement_signal_unavailable_observed": False,
        "forced_process_retirement": False,
        "retirement_signals": [],
        "child_reaped": True,
        "group_state": "absent",
        "sticky_failure": False,
        "failure_code": None,
    }


def canonical_bytes(receipt: dict[str, object]) -> bytes:
    return (json.dumps(receipt, ensure_ascii=True, separators=(",", ":")) + "\n").encode("ascii")


class OwnedLayoutProcessWriterFiles(unittest.TestCase):
    def setUp(self) -> None:
        canonical_temp = Path(tempfile.gettempdir()).resolve(strict=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="mount-rs-process-writer-control-", dir=canonical_temp)
        self.root = Path(self.temporary.name)
        os.chmod(self.root, 0o700)
        self.parent = self.root / "evidence"
        self.parent.mkdir(mode=0o700)
        self.target = self.parent / "receipt.json"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def fixture_file(self, path: Path, data: bytes = b"owned fixture bytes\n") -> bytes:
        with path.open("xb") as output:
            output.write(data)
        os.chmod(path, 0o600)
        return data

    def assert_denied(self, path: Path, receipt: dict[str, object] | None = None) -> None:
        rejected = False
        try:
            writer(path, clean_receipt() if receipt is None else receipt)
        except ValueError as error:
            self.assertEqual(str(error), WRITE_FAILED, "writer denial must use a fixed redacted code")
            rejected = True
        self.assertIs(rejected, True, "missing or unsafe publication must not be accepted")

    def test_fixture_is_canonical_private_owned_and_regular(self) -> None:
        self.assertEqual(self.parent.resolve(), self.parent)
        parent_stat = self.parent.stat()
        self.assertEqual(parent_stat.st_uid, os.getuid())
        self.assertEqual(stat.S_IMODE(parent_stat.st_mode), 0o700)
        data = self.fixture_file(self.target)
        file_stat = self.target.stat()
        self.assertTrue(stat.S_ISREG(file_stat.st_mode))
        self.assertEqual(stat.S_IMODE(file_stat.st_mode), 0o600)
        self.assertEqual(file_stat.st_nlink, 1)
        self.assertEqual(self.target.read_bytes(), data)

    def test_success_retains_exact_bytes_digest_mode_and_single_link(self) -> None:
        receipt = clean_receipt()
        expected = canonical_bytes(receipt)
        writer(self.target, receipt)
        self.assertIs(self.target.is_file(), True, "exclusive successful writer must create the receipt")
        observed = self.target.read_bytes()
        self.assertEqual(observed, expected)
        self.assertEqual(hashlib.sha256(observed).digest(), hashlib.sha256(expected).digest())
        self.assertEqual(json.loads(observed), receipt)
        file_stat = self.target.lstat()
        self.assertTrue(stat.S_ISREG(file_stat.st_mode))
        self.assertEqual(file_stat.st_uid, os.getuid())
        self.assertEqual(stat.S_IMODE(file_stat.st_mode), 0o600)
        self.assertEqual(file_stat.st_nlink, 1)
        self.assertLessEqual(file_stat.st_size, 4096)
        self.assertEqual(receipt, clean_receipt(), "writer must not mutate the process outcome")

    def test_leaf_creation_is_exclusive_no_follow_and_anchored_to_parent_fd(self) -> None:
        real_open = os.open
        calls: list[dict[str, object]] = []

        def opened(path, flags, mode=0o777, *, dir_fd=None):
            descriptor = real_open(path, flags, mode, dir_fd=dir_fd)
            calls.append({"path": os.fspath(path), "flags": flags, "mode": mode, "dir_fd": dir_fd, "fd": descriptor})
            return descriptor

        with patch("os.open", side_effect=opened):
            writer(self.target, clean_receipt())
        leaf = [call for call in calls if call["path"] == self.target.name]
        self.assertEqual(len(leaf), 1, "leaf must be created once relative to a pinned directory fd")
        action = leaf[0]
        self.assertIsNotNone(action["dir_fd"])
        self.assertEqual(action["mode"], 0o600)
        for flag in [os.O_WRONLY, os.O_CREAT, os.O_EXCL, os.O_NOFOLLOW]:
            self.assertEqual(action["flags"] & flag, flag)
        parent = [call for call in calls if call["fd"] == action["dir_fd"]]
        self.assertEqual(len(parent), 1)
        self.assertEqual(parent[0]["flags"] & os.O_DIRECTORY, os.O_DIRECTORY)
        self.assertEqual(parent[0]["flags"] & os.O_NOFOLLOW, os.O_NOFOLLOW)

    def test_existing_regular_receipt_is_never_overwritten(self) -> None:
        original = self.fixture_file(self.target)
        self.assert_denied(self.target)
        self.assertEqual(self.target.read_bytes(), original)

    def test_existing_hardlink_is_never_overwritten(self) -> None:
        original_path = self.parent / "existing.json"
        original = self.fixture_file(original_path)
        os.link(original_path, self.target)
        self.assertEqual(self.target.stat().st_nlink, 2)
        self.assert_denied(self.target)
        self.assertEqual(original_path.read_bytes(), original)
        self.assertEqual(self.target.read_bytes(), original)

    def test_leaf_symlink_does_not_modify_its_owned_target(self) -> None:
        original_path = self.parent / "existing.json"
        original = self.fixture_file(original_path)
        self.target.symlink_to(original_path)
        self.assert_denied(self.target)
        self.assertTrue(self.target.is_symlink())
        self.assertEqual(original_path.read_bytes(), original)

    def test_parent_symlink_alias_is_refused(self) -> None:
        alias = self.root / "alias"
        alias.symlink_to(self.parent, target_is_directory=True)
        self.assert_denied(alias / self.target.name)
        self.assertFalse(self.target.exists())

    def test_dot_dot_spelling_alias_is_refused(self) -> None:
        child = self.parent / "child"
        child.mkdir(mode=0o700)
        aliased = child / ".." / self.target.name
        self.assert_denied(aliased)
        self.assertFalse(self.target.exists())

    def test_relative_path_is_refused_without_opening_any_other_directory(self) -> None:
        calls: list[object] = []

        def refused_open(*args, **kwargs):
            calls.append((args, kwargs))
            raise AssertionError("relative destination must be rejected before open")

        with patch("os.open", side_effect=refused_open):
            self.assert_denied(Path("relative-receipt.json"))
        self.assertEqual(calls, [])

    def test_parent_mode_must_be_exactly_private_0700(self) -> None:
        os.chmod(self.parent, 0o755)
        self.assert_denied(self.target)
        self.assertFalse(self.target.exists())

    def test_parent_must_match_the_current_uid_observation(self) -> None:
        observed_uid = os.getuid()
        with patch("os.getuid", return_value=observed_uid + 1):
            self.assert_denied(self.target)
        self.assertFalse(self.target.exists())

    def test_directory_leaf_is_refused(self) -> None:
        self.target.mkdir(mode=0o700)
        self.assert_denied(self.target)
        self.assertTrue(self.target.is_dir())
        self.assertEqual(list(self.target.iterdir()), [])

    def test_parent_path_replacement_before_leaf_open_is_detected(self) -> None:
        real_open = os.open
        retired = self.root / "retired-parent"
        replaced = False

        def opened(path, flags, mode=0o777, *, dir_fd=None):
            nonlocal replaced
            if flags & os.O_CREAT:
                self.assertEqual(os.fspath(path), self.target.name)
                self.assertIsNotNone(dir_fd, "publication must stay anchored to original owned parent")
                self.parent.rename(retired)
                self.parent.mkdir(mode=0o700)
                replaced = True
            return real_open(path, flags, mode, dir_fd=dir_fd)

        with patch("os.open", side_effect=opened):
            self.assert_denied(self.target)
        self.assertIs(replaced, True, "fault control must reach the leaf publication boundary")
        self.assertFalse(self.target.exists(), "replacement parent must remain untouched")
        self.assertTrue(retired.is_dir())

    def test_parent_path_replacement_during_write_is_detected(self) -> None:
        real_write = os.write
        retired = self.root / "retired-parent"
        replaced = False

        def written(descriptor: int, data: bytes) -> int:
            nonlocal replaced
            if not replaced:
                self.parent.rename(retired)
                self.parent.mkdir(mode=0o700)
                replaced = True
            return real_write(descriptor, data)

        with patch("os.write", side_effect=written):
            self.assert_denied(self.target)
        self.assertIs(replaced, True, "fault control must reach an actual bounded write")
        self.assertFalse(self.target.exists(), "replacement parent must remain untouched")

    def test_leaf_identity_replacement_during_write_is_detected(self) -> None:
        real_write = os.write
        original = self.parent / "retired-receipt.json"
        replacement = b"replacement owned fixture\n"
        replaced = False

        def written(descriptor: int, data: bytes) -> int:
            nonlocal replaced
            if not replaced:
                self.target.rename(original)
                self.fixture_file(self.target, replacement)
                replaced = True
            return real_write(descriptor, data)

        with patch("os.write", side_effect=written):
            self.assert_denied(self.target)
        self.assertIs(replaced, True)
        self.assertEqual(self.target.read_bytes(), replacement, "writer must not delete or overwrite replacement")

    def test_new_hardlink_during_publication_is_detected(self) -> None:
        real_write = os.write
        alias = self.parent / "receipt-alias.json"
        linked = False

        def written(descriptor: int, data: bytes) -> int:
            nonlocal linked
            if not linked:
                os.link(self.target, alias)
                linked = True
            return real_write(descriptor, data)

        with patch("os.write", side_effect=written):
            self.assert_denied(self.target)
        self.assertIs(linked, True, "link-count fault must occur after exclusive creation")

    def test_closed_schema_rejects_extra_fields_and_coerced_scalars(self) -> None:
        for update in [{"unexpected": "fixture"}, {"child_reaped": 1}, {"normalized_exit": 0.0}]:
            with self.subTest(update=tuple(update)):
                receipt = clean_receipt()
                receipt.update(update)
                self.assert_denied(self.target, receipt)
                self.assertFalse(self.target.exists())

    def test_encoded_4097_byte_boundary_fault_is_refused_before_creation(self) -> None:
        # Closed valid inputs are smaller; injected codec padding isolates the cap.
        receipt = clean_receipt()
        raw = canonical_bytes(receipt)[:-1].decode("ascii")
        padded = raw + " " * (4096 - len(raw))
        self.assertEqual(len((padded + "\n").encode("ascii")), 4097)
        self.assertEqual(json.loads(padded), receipt)
        real_open = os.open
        creations: list[object] = []

        def opened(path, flags, mode=0o777, *, dir_fd=None):
            if flags & os.O_CREAT:
                creations.append(os.fspath(path))
                raise AssertionError("encoded cap must be checked before leaf creation")
            return real_open(path, flags, mode, dir_fd=dir_fd)

        with patch("json.dumps", return_value=padded), patch("os.open", side_effect=opened):
            self.assert_denied(self.target, receipt)
        self.assertEqual(creations, [], "oversized encoded receipt must not create even a temporary leaf")
        self.assertFalse(self.target.exists())

    def test_failed_writer_prevents_the_matching_pid_file_release(self) -> None:
        original = self.fixture_file(self.target)
        released: list[str] = []
        rejected = False
        try:
            api.retain_before_release(clean_receipt(), retain=lambda receipt: writer(self.target, receipt), release=lambda: released.append("released"))
        except ValueError as error:
            self.assertEqual(str(error), WRITE_FAILED)
            rejected = True
        self.assertIs(rejected, True, "failed publication must stop the owner release path")
        self.assertEqual(released, [])
        self.assertEqual(self.target.read_bytes(), original)

    def test_short_writes_complete_exactly_and_zero_write_is_refused(self) -> None:
        real_write = os.write
        sizes: list[int] = []

        def short_write(descriptor: int, data: bytes) -> int:
            sizes.append(len(data))
            return real_write(descriptor, data[:7])

        with patch("os.write", side_effect=short_write):
            writer(self.target, clean_receipt())
        self.assertGreater(len(sizes), 1, "bounded writer must finish real short writes")
        self.assertEqual(self.target.read_bytes(), canonical_bytes(clean_receipt()))
        zero_target = self.parent / "zero-write.json"
        with patch("os.write", return_value=0):
            self.assert_denied(zero_target)

    def test_failed_publication_never_unlinks_the_owned_pathname(self) -> None:
        real_unlink = os.unlink
        deletions: list[object] = []

        def unlinked(path, *, dir_fd=None):
            deletions.append((os.fspath(path), dir_fd))
            return real_unlink(path, dir_fd=dir_fd)

        with patch("os.write", return_value=0), patch("os.unlink", side_effect=unlinked):
            self.assert_denied(self.target)
        self.assertEqual(deletions, [], "failed publication must not race a replacement through pathname unlink")
        self.assertTrue(self.target.exists(), "created private partial file is retained as unqualified evidence")
        self.assertEqual(self.target.read_bytes(), b"")
        file_stat = self.target.lstat()
        self.assertTrue(stat.S_ISREG(file_stat.st_mode))
        self.assertEqual(stat.S_IMODE(file_stat.st_mode), 0o600)
        self.assertEqual(file_stat.st_nlink, 1)
        self.assert_denied(self.target, clean_receipt())
        self.assertEqual(self.target.read_bytes(), b"", "a later attempt requires a new exclusive path")


if __name__ == "__main__":
    unittest.main(verbosity=2)
