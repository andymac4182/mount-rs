#!/usr/bin/env python3
"""List every lock field in production code and check the readiness doc names it.

A lock field is a struct field or a static whose type mentions one of the
in-house exclusion types. Test modules and test directories are skipped.
Exits 1 if a field is missing from docs/production-readiness.md.
"""
import os
import re
import sys

ROOTS = ["src", "apps", "bindings", "crates", "filesystems", "providers", "transports", "vendor"]
TYPES = r"\b(TurnCell|ParkCell|TurnLock|TurnGate|DrainGate|AsyncGate|Mutex|RwLock)\b"
# The types' own definitions, and quinn's `Turn<T>` cell.
DEFINING = {"src/turn_cell.rs", "src/park_cell.rs", "src/turn_gate.rs", "src/drain_gate.rs"}
QUINN_TURN = re.compile(r"\bTurn<")
FIELD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(\w+):\s*(.+?),?\s*(?://.*)?$")
STATIC = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+(\w+):\s*(.+?)\s*=")
STRUCT = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+(\w+)(?:<[^>]*>)?(\(.*\);)?")
TEST_MOD = re.compile(r"^\s*#\[cfg\((?:all\()?test\b")


# Second references to a lock the doc lists under another name. Each is a
# clone of the same `Arc`, or a wrapper around it, not a lock of its own.
HANDLES = {
    ("bindings/mount-rs-napi", "Acquire.lock"): "NativePathLock state",
    ("bindings/mount-rs-napi", "LockGuard.lock"): "NativePathLock state",
    ("filesystems/mount-rs-chunked", "AsyncGate.state"): "the TurnGate inside gate and lease_gate",
    ("filesystems/mount-rs-kv", "State.open_files"): "each OpenFile",
    ("filesystems/mount-rs-kv", "KvFileHandle.entry"): "each OpenFile",
    ("transports/mount-rs-nfs", "CellDebug.0"): "a borrowed cell, for formatting",
    ("transports/mount-rs-s3", "ConditionalPutLockRegistration.lock"): "per-key conditional-put gate",
    ("transports/mount-rs-s3", "ConditionalPutLockGuard.lock"): "per-key conditional-put gate",
}


def crate_of(path):
    parts = path.split(os.sep)
    return "/".join(parts[:2]) if parts[0] != "src" else "mount-rs-core"


def production_lines(path):
    """Lines of the file up to the first test module at the top level."""
    lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
    for index, line in enumerate(lines):
        if TEST_MOD.match(line) and not line.startswith(" "):
            following = lines[index + 1] if index + 1 < len(lines) else ""
            if following.lstrip().startswith(("mod ", "pub mod ", "pub(crate) mod ")) and "{" in following:
                return lines[:index]
    return lines


def lock_type(path, kind):
    quinn = path.startswith("vendor/quinn-proto") and QUINN_TURN.search(kind)
    return bool(re.search(TYPES, kind) or quinn)


def test_only_files():
    """Files pulled in by `#[cfg(test)] #[path = "..."] mod ...;`."""
    found = set()
    for root in ROOTS:
        for directory, _, files in os.walk(root):
            for name in files:
                if not name.endswith(".rs"):
                    continue
                lines = open(os.path.join(directory, name), encoding="utf-8", errors="replace").read().splitlines()
                for index, line in enumerate(lines[:-1]):
                    target = re.match(r'\s*#\[path = "([^"]+)"\]', lines[index + 1])
                    if TEST_MOD.match(line) and target:
                        found.add(os.path.normpath(os.path.join(directory, target.group(1))))
    return found


def fields():
    test_only = test_only_files()
    for root in ROOTS:
        for directory, names, files in os.walk(root):
            names[:] = [n for n in names if n not in ("tests", "benches", "examples", "target", "node_modules")]
            for name in files:
                if not name.endswith(".rs") or name.endswith("_tests.rs") or name == "tests.rs":
                    continue
                path = os.path.join(directory, name)
                if path in DEFINING:
                    continue
                if path in test_only:
                    continue
                owner = "?"
                previous = ""
                for number, line in enumerate(production_lines(path), 1):
                    gated, previous = TEST_MOD.match(previous), line
                    if gated:
                        continue
                    found = STRUCT.match(line)
                    if found:
                        owner = found.group(1)
                        kind = found.group(2) or ""
                        # A tuple struct that wraps a lock.
                        if lock_type(path, kind):
                            yield crate_of(path), path, number, owner, "0", kind.strip("();")
                        continue
                    found = STATIC.match(line)
                    holder = "static"
                    if not found and re.match(r"    (?! )", line):
                        found, holder = FIELD.match(line), owner
                    if not found:
                        continue
                    kind = found.group(2)
                    # A value in a struct literal, or a borrowed parameter.
                    if re.search(r"::\w+\(|\w\(\w", kind) or kind.startswith("&"):
                        continue
                    if lock_type(path, kind):
                        yield crate_of(path), path, number, holder, found.group(1), kind


def main():
    doc = open("docs/production-readiness.md", encoding="utf-8").read()
    section = doc[doc.index("## Locks"):]
    # The words each crate's table rows put in backticks.
    named = {}
    for line in section.splitlines():
        cells = [cell.strip() for cell in line.split("|")]
        if len(cells) < 4 or "/" not in cells[1]:
            continue
        words = {word for name in re.findall(r"`([^`]+)`", line) for word in re.findall(r"\w+", name)}
        for crate in cells[1].split(","):
            named.setdefault(crate.strip(), set()).update(words)
    missing = handles = 0
    rows = sorted(fields())
    for crate, path, number, owner, field, kind in rows:
        words = named.get(crate, set())
        handle = HANDLES.get((crate, f"{owner}.{field}"))
        listed = field in words or owner in words
        status = "ok     " if listed else "handle " if handle else "MISSING"
        missing += status == "MISSING"
        handles += status == "handle "
        note = f"  (= {handle})" if status == "handle " else ""
        print(f"{status} {crate:34} {owner}.{field:24} {path}:{number}  {kind}{note}")
    print(
        f"\n{len(rows)} lock fields: {len(rows) - missing - handles} named in a row for their crate in "
        f"docs/production-readiness.md, {handles} second handles to a named lock, {missing} missing"
    )
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
