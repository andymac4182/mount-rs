"""Descriptor-bound, capped artifact reads and explicit source-pin checks.

Both public functions use the same single-descriptor read. Parent directories
are opened from the root without following symlinks and kept until validation.
Importing this module performs no filesystem or process operations.
"""

from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import stat

from .contracts import ContractError, uint64


_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_CHUNK = 65536


class ArtifactError(ContractError):
    """An artifact failed admission, continuity or its explicit source pin."""


@dataclass(frozen=True)
class ArtifactDigest:
    sha256: str
    size: int


@dataclass(frozen=True)
class ArtifactRead:
    data: bytes
    pin: ArtifactDigest


def _directory_identity(info):
    return info.st_dev, info.st_ino, stat.S_IFMT(info.st_mode)


def _file_identity(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
            info.st_ctime_ns, info.st_mode, info.st_uid, info.st_nlink)


def _require(ok, label):
    if not ok:
        raise ArtifactError(label)


def _read(path, *, cap, expected_sha256, retain):
    try:
        cap = uint64(cap)
    except ContractError as error:
        raise ArtifactError("artifact_cap_type") from error
    _require(cap > 0, "artifact_cap_type")
    _require(isinstance(path, Path) and path.is_absolute() and len(path.parts) > 1
             and ".." not in path.parts, "noncanonical_artifact")
    _require(expected_sha256 is None or
             (type(expected_sha256) is str and _SHA256.fullmatch(expected_sha256)),
             "artifact_pin_shape")

    descriptors = []
    try:
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
        directory_flags = flags | os.O_DIRECTORY
        parent = os.open(os.sep, directory_flags)
        descriptors.append(parent)
        root_identity = _directory_identity(os.fstat(parent))
        _require(root_identity[2] == stat.S_IFDIR, "artifact_parent_shape")
        ancestors = []
        for name in path.parts[1:-1]:
            child = os.open(name, directory_flags, dir_fd=parent)
            descriptors.append(child)
            identity = _directory_identity(os.fstat(child))
            named = os.stat(name, dir_fd=parent, follow_symlinks=False)
            _require(identity[2] == stat.S_IFDIR and
                     _directory_identity(named) == identity, "artifact_parent_changed")
            ancestors.append((parent, name, child, identity))
            parent = child

        fd = os.open(path.name, flags, dir_fd=parent)
        descriptors.append(fd)
        before = os.fstat(fd)
        _require(stat.S_ISREG(before.st_mode) and before.st_uid == os.getuid()
                 and before.st_nlink == 1 and 0 <= before.st_size <= cap,
                 "artifact_shape_or_cap")
        digest = hashlib.sha256()
        remaining = before.st_size
        chunks = [] if retain else None
        while remaining:
            chunk = os.read(fd, min(_CHUNK, remaining))
            _require(bool(chunk) and len(chunk) <= remaining, "artifact_short_read")
            digest.update(chunk)
            if retain:
                chunks.append(chunk)
            remaining -= len(chunk)
        _require(not os.read(fd, 1), "artifact_grew_during_read")
        after = os.fstat(fd)
        named = os.stat(path.name, dir_fd=parent, follow_symlinks=False)
        _require(_file_identity(before) == _file_identity(after) == _file_identity(named),
                 "artifact_changed")
        for original_parent, name, child, identity in ancestors:
            _require(_directory_identity(os.fstat(child)) == identity and
                     _directory_identity(os.stat(name, dir_fd=original_parent,
                                                 follow_symlinks=False)) == identity,
                     "artifact_parent_changed")
        _require(_directory_identity(os.stat(os.sep, follow_symlinks=False)) == root_identity,
                 "artifact_parent_changed")
        pin = ArtifactDigest(digest.hexdigest(), before.st_size)
        _require(expected_sha256 is None or pin.sha256 == expected_sha256,
                 "fixed_source_or_binary_pin_changed")
        return ArtifactRead(b"".join(chunks), pin) if retain else pin
    except OSError as error:
        raise ArtifactError("artifact_read_failed") from error
    finally:
        close_error = None
        for fd in reversed(descriptors):
            try:
                os.close(fd)
            except OSError as error:
                close_error = error
        if close_error is not None:
            raise ArtifactError("artifact_close_failed") from close_error


def read_artifact(path: Path, *, cap: int, expected_sha256=None) -> ArtifactRead:
    return _read(path, cap=cap, expected_sha256=expected_sha256, retain=True)


def digest_artifact(path: Path, *, cap: int, expected_sha256=None) -> ArtifactDigest:
    return _read(path, cap=cap, expected_sha256=expected_sha256, retain=False)
