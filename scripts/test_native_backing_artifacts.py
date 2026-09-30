"""Small real-filesystem tests for the private guarded artifact reader.

The baseline is intentionally unvalidated. These tests must fail on missing
guards, not on imports or a synthetic exception. No provider or subprocess runs.
"""

import hashlib
import errno
import os
from pathlib import Path
import tempfile
import threading
import tracemalloc
import unittest
from unittest import mock

from native_backing_pair import artifacts
from native_backing_pair.artifacts import ArtifactError, digest_artifact, read_artifact
from native_backing_pair.contracts import (
    Backend, ContractError, parse_backend, require_keys, strict_json, uint64,
)


class ContractsTests(unittest.TestCase):
    def test_valid_closed_inputs(self):
        self.assertEqual(strict_json(b'{"rows":[0,true,null],"name":"x"}'),
                         {"rows": [0, True, None], "name": "x"})
        self.assertEqual(uint64(0), 0)
        self.assertEqual(uint64((1 << 64) - 1), (1 << 64) - 1)
        require_keys({"schema": "x", "backend": "filesystem"},
                     frozenset({"schema", "backend"}))
        self.assertEqual(parse_backend("filesystem"), Backend.FILESYSTEM)
        self.assertEqual(parse_backend("rustfs"), Backend.RUSTFS)

    def test_duplicate_keys_at_every_depth_are_rejected(self):
        for raw in (b'{"x":1,"x":2}', b'{"outer":{"x":1,"x":2}}'):
            with self.subTest(raw=raw), self.assertRaises(ContractError):
                strict_json(raw)

    def test_nonfinite_constants_and_overflowing_float_are_rejected(self):
        for raw in (b'{"x":NaN}', b'{"x":Infinity}', b'{"x":-Infinity}',
                    b'{"x":1e999}'):
            with self.subTest(raw=raw), self.assertRaises(ContractError):
                strict_json(raw)

    def test_malformed_json_and_utf8_have_contract_errors(self):
        for raw in (b'{', b'{"x":"\xff"}', b'{} trailing',
                    '{"x":1}'.encode("utf-16"), '{"x":1}'.encode("utf-32")):
            with self.subTest(raw=raw), self.assertRaises(ContractError):
                strict_json(raw)

    def test_unsigned_integer_rejects_bool_conversion_and_range_errors(self):
        for value in (True, False, -1, 1 << 64, 1.0, "1", None):
            with self.subTest(value=value), self.assertRaises(ContractError):
                uint64(value)

    def test_closed_object_rejects_missing_unknown_and_nonobject(self):
        for value in ({"schema": "x"}, {"schema": "x", "extra": 1}, [], True):
            with self.subTest(value=value), self.assertRaises(ContractError):
                require_keys(value, frozenset({"schema", "backend"}))

    def test_backend_selector_is_closed(self):
        for value in ("metadata", "Filesystem", "", 1, True, None, [], {}):
            with self.subTest(value=value), self.assertRaises(ContractError):
                parse_backend(value)


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="mount-rs-artifact-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.path = self.root / "artifact"

    def write(self, raw=b"owned-artifact"):
        self.path.write_bytes(raw)
        return self.path

    def test_read_and_streaming_digest_have_identical_explicit_pin(self):
        raw = b"owned-artifact\x00\xff"
        path = self.write(raw)
        expected = hashlib.sha256(raw).hexdigest()
        result = read_artifact(path, cap=len(raw), expected_sha256=expected)
        self.assertEqual(result.data, raw)
        self.assertEqual(result.pin.sha256, expected)
        self.assertEqual(result.pin.size, len(raw))
        self.assertEqual(digest_artifact(path, cap=len(raw), expected_sha256=expected),
                         result.pin)

    def test_digest_does_not_retain_the_whole_binary(self):
        block = b"x" * 65536
        expected = hashlib.sha256()
        with self.path.open("wb") as stream:
            for _ in range(128):
                stream.write(block)
                expected.update(block)
        tracemalloc.start()
        try:
            result = digest_artifact(self.path, cap=8 * 1024 ** 2,
                                     expected_sha256=expected.hexdigest())
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(result.size, 8 * 1024 ** 2)
        self.assertEqual(result.sha256, expected.hexdigest())
        self.assertLess(peak, 3 * 1024 ** 2,
                        "digest-only source pins must not retain the whole binary")

    def test_empty_file_is_accepted_with_positive_cap(self):
        self.assertEqual(read_artifact(self.write(b""), cap=1).data, b"")

    def test_exact_cap_is_accepted(self):
        self.assertEqual(read_artifact(self.write(b"1234"), cap=4).data, b"1234")

    def test_cap_plus_one_is_rejected(self):
        with self.assertRaises(ArtifactError):
            read_artifact(self.write(b"12345"), cap=4)

    def test_cap_requires_positive_uint64_without_bool(self):
        path = self.write(b"x")
        for cap in (0, -1, True, 1.0, "1", 1 << 64):
            with self.subTest(cap=cap), self.assertRaises(ArtifactError):
                read_artifact(path, cap=cap)

    def test_source_pin_mismatch_is_rejected(self):
        with self.assertRaises(ArtifactError):
            digest_artifact(self.write(), cap=32, expected_sha256="0" * 64)

    def test_source_pin_requires_lowercase_sha256(self):
        path = self.write()
        for pin in ("0" * 63, "A" * 64, "g" * 64, True, 1):
            with self.subTest(pin=pin), self.assertRaises(ArtifactError):
                digest_artifact(path, cap=32, expected_sha256=pin)

    def test_relative_and_dotdot_paths_are_rejected(self):
        self.write()
        (self.root / "child").mkdir()
        relative = Path(os.path.relpath(self.path, Path.cwd()))
        for path in (relative, self.root / "child" / ".." / "artifact"):
            with self.subTest(path=path), self.assertRaises(ArtifactError):
                read_artifact(path, cap=32)

    def test_leaf_symlink_is_rejected(self):
        target = self.root / "target"
        target.write_bytes(b"real")
        self.path.symlink_to(target)
        with self.assertRaises(ArtifactError):
            read_artifact(self.path, cap=32)

    def test_ancestor_symlink_is_rejected(self):
        real = self.root / "real"
        real.mkdir()
        (real / "artifact").write_bytes(b"real")
        alias = self.root / "alias"
        alias.symlink_to(real, target_is_directory=True)
        with self.assertRaises(ArtifactError):
            read_artifact(alias / "artifact", cap=32)

    def test_hardlinked_regular_file_is_rejected(self):
        self.write()
        os.link(self.path, self.root / "second-link")
        with self.assertRaises(ArtifactError):
            read_artifact(self.path, cap=32)

    def test_directory_is_rejected_with_artifact_error(self):
        self.path.mkdir()
        with self.assertRaises(ArtifactError):
            read_artifact(self.path, cap=32)

    def test_fifo_refuses_even_while_a_writer_keeps_it_open(self):
        os.mkfifo(self.path, 0o600)
        writer = os.open(self.path, os.O_RDWR | os.O_NONBLOCK)
        released = threading.Event()

        def release_writer():
            os.close(writer)
            released.set()

        # A regressed blocking reader reaches EOF after a finite interval and
        # then fails the refusal assertion; it cannot hang this baseline test.
        timer = threading.Timer(0.5, release_writer)
        timer.daemon = True
        timer.start()
        try:
            with self.assertRaises(ArtifactError):
                read_artifact(self.path, cap=32)
        finally:
            timer.cancel()
            timer.join()
            if not released.is_set():
                os.close(writer)

    def test_fifo_without_writer_is_refused_before_delayed_open_release(self):
        os.mkfifo(self.path, 0o600)
        release_attempted = threading.Event()

        def release_blocking_open():
            release_attempted.set()
            try:
                writer = os.open(self.path, os.O_WRONLY | os.O_NONBLOCK)
            except OSError:
                # The correct reader has already refused and has no open FIFO.
                return
            os.close(writer)

        timer = threading.Timer(0.5, release_blocking_open)
        timer.daemon = True
        timer.start()
        try:
            with self.assertRaises(ArtifactError):
                read_artifact(self.path, cap=32)
            self.assertFalse(release_attempted.is_set(),
                             "FIFO refusal must not wait for a writer to open")
        finally:
            timer.cancel()
            timer.join()

    def replace_during_read(self, replacement, path=None):
        path = self.path if path is None else path
        real_read = os.read
        replaced = False

        def read_after_replacement(fd, count):
            nonlocal replaced
            if not replaced:
                replaced = True
                replacement()
            return real_read(fd, count)

        with mock.patch.object(artifacts.os, "read", side_effect=read_after_replacement):
            with self.assertRaises(ArtifactError):
                read_artifact(path, cap=32)
        self.assertTrue(replaced, "the real path replacement must occur during the read")

    def test_named_replacement_is_rejected_even_with_identical_bytes(self):
        self.write(b"same")
        replacement = self.root / "replacement"
        replacement.write_bytes(b"same")
        self.replace_during_read(lambda: os.replace(replacement, self.path))

    def test_same_inode_growth_during_read_is_rejected(self):
        self.write(b"old")
        self.replace_during_read(lambda: self.path.write_bytes(b"too-large"))

    def test_same_inode_truncation_during_read_is_rejected(self):
        self.write(b"original")
        self.replace_during_read(lambda: self.path.write_bytes(b""))

    def test_same_size_rewrite_with_restored_mtime_is_rejected(self):
        self.write(b"old")
        original = self.path.stat()

        def rewrite():
            self.path.write_bytes(b"new")
            os.utime(self.path, ns=(original.st_atime_ns, original.st_mtime_ns))

        self.replace_during_read(rewrite)

    def test_ancestor_replacement_during_read_is_rejected(self):
        parent = self.root / "parent"
        parent.mkdir()
        path = parent / "artifact"
        path.write_bytes(b"same")
        other = self.root / "other"
        other.mkdir()
        (other / "artifact").write_bytes(b"same")

        def replace_parent():
            parent.rename(self.root / "previous-parent")
            parent.symlink_to(other, target_is_directory=True)

        self.replace_during_read(replace_parent, path=path)

    def test_ancestor_symlink_to_original_inode_during_read_is_rejected(self):
        parent = self.root / "parent"
        parent.mkdir()
        path = parent / "artifact"
        path.write_bytes(b"same")
        moved = self.root / "moved-parent"

        def replace_parent():
            parent.rename(moved)
            parent.symlink_to(moved, target_is_directory=True)

        self.replace_during_read(replace_parent, path=path)

    def test_read_failure_closes_every_acquired_descriptor(self):
        self.write()
        real_open = os.open
        opened = []

        def capture_open(*args, **kwargs):
            fd = real_open(*args, **kwargs)
            opened.append(fd)
            return fd

        with mock.patch.object(artifacts.os, "open", side_effect=capture_open):
            with mock.patch.object(artifacts.os, "read", side_effect=OSError(errno.EIO, "fault")):
                with self.assertRaises(ArtifactError):
                    read_artifact(self.path, cap=32)
        self.assertTrue(opened)
        for fd in opened:
            with self.subTest(fd=fd), self.assertRaises(OSError) as caught:
                os.fstat(fd)
            self.assertEqual(caught.exception.errno, errno.EBADF)


if __name__ == "__main__":
    unittest.main()
