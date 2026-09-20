"""Real SQLite process/locking probes; mount-service/power-loss tests are separate.

Run with a mount directory, or --self-test for an isolated host-filesystem
control. Child processes touch only the parent-selected test database.
"""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path


def connect(path):
    db = sqlite3.connect(path, timeout=0.1)
    db.execute("PRAGMA synchronous=FULL")
    return db


def child(path, mode):
    db = connect(path)
    db.execute("PRAGMA cache_size=8")
    db.execute("BEGIN IMMEDIATE")
    db.execute("UPDATE items SET payload=? WHERE id=1", (b"uncommitted",))
    # Force dirty-page spills rather than testing only SQLite's page cache.
    db.executemany("INSERT INTO items(payload) VALUES(?)", [(b"x" * 8192,)] * 128)
    if mode == "committed":
        db.commit()
    print("ready", flush=True)
    sys.stdin.buffer.read(1)
    os._exit(90)  # Parent should terminate us before this branch.


def integrity(db):
    assert db.execute("PRAGMA integrity_check").fetchall() == [("ok",)]


def run_case(directory, journal):
    path = directory / (journal.lower() + ".sqlite")
    original = bytes(range(256)) * 257
    db = connect(path)
    actual = db.execute("PRAGMA journal_mode=" + journal).fetchone()[0]
    assert actual.upper() == journal, (journal, actual)
    assert db.execute("PRAGMA synchronous").fetchone()[0] == 2
    db.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, payload BLOB NOT NULL)")
    db.execute("INSERT INTO items VALUES(1, ?)", (original,))
    db.commit()
    db.execute("UPDATE items SET payload=? WHERE id=1", (b"rollback",))
    db.rollback()
    assert db.execute("SELECT payload FROM items WHERE id=1").fetchone()[0] == original
    integrity(db)
    db.close()

    for mode in ("uncommitted", "committed"):
        # Configure the contender before the child acquires EXCLUSIVE locks:
        # some SQLite versions read the schema while setting synchronous.
        contender = connect(path) if mode == "uncommitted" else None
        process = subprocess.Popen(
            [sys.executable, __file__, "--child", str(path), mode],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            assert process.stdout.readline() == b"ready\n", process.communicate(timeout=10)
            if mode == "uncommitted":
                try:
                    try:
                        contender.execute("BEGIN IMMEDIATE")
                    except sqlite3.OperationalError as error:
                        assert "locked" in str(error).lower(), str(error)
                    else:
                        raise AssertionError("separate process acquired a conflicting write lock")
                    # WAL readers must still observe committed data while a
                    # writer is active. DELETE spills can hold EXCLUSIVE locks.
                    if journal == "WAL":
                        assert contender.execute("SELECT payload FROM items WHERE id=1").fetchone()[0] == original
                finally:
                    contender.close()
                    contender = None
            process.kill()
            process.communicate(timeout=10)
        finally:
            if contender is not None:
                contender.close()
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=10)

        reopened = connect(path)
        integrity(reopened)
        count = reopened.execute("SELECT count(*) FROM items").fetchone()[0]
        payload = reopened.execute("SELECT payload FROM items WHERE id=1").fetchone()[0]
        if mode == "uncommitted":
            assert count == 1 and payload == original, (count, payload[:20])
        else:
            assert count == 129 and payload == b"uncommitted", count
        if journal == "WAL":
            checkpoint = reopened.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
            assert checkpoint[0] == 0, checkpoint
        reopened.close()

    print(json.dumps({"sqlite": sqlite3.sqlite_version, "journal": journal,
                      "synchronous": "FULL", "process_lock_and_recovery": "pass",
                      "mount_service_crash_tested": False}), flush=True)


def run(directory):
    assert directory.is_dir()
    for journal in ("DELETE", "WAL"):
        run_case(directory, journal)


if __name__ == "__main__":
    if sys.argv[1] == "--child":
        child(sys.argv[2], sys.argv[3])
    elif sys.argv[1] == "--self-test":
        with tempfile.TemporaryDirectory(prefix="mount-rs-sqlite-control-") as root:
            run(Path(root))
    else:
        run(Path(sys.argv[1]))
