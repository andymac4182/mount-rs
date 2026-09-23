"""Disposable SQLite file probes for a native mount-rs NFS mount.

The SQLite database files in this fixture live *inside* the tested mount.
Use --self-test for a local-filesystem control, --mount for one NFS view, and
--second-view to examine the unsupported case of two NFS client mountpoints
opening the same SQLite file. The second-view probe only attempts competing
locks; it never commits a transaction after detecting a bypassed lock.

Output is newline-delimited JSON. Rollback-journal, integrity, and load
failures return a nonzero exit status. WAL fallback and second-view hazards
are reported without claiming that those topologies are supported.
"""

import argparse
import errno
import hashlib
import json
import os
import select
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from sqlite_hosting import run_case


JOURNALS = ("DELETE", "TRUNCATE", "PERSIST", "WAL")
LOAD_JOURNALS = ("DELETE", "WAL")
WORKER_TIMEOUT = 90


def emit(**fields):
    print(json.dumps(fields, sort_keys=True), flush=True)


def payload(worker, transaction):
    return hashlib.sha256(f"{worker}:{transaction}".encode()).digest() * 128


def clean_db(path):
    for suffix in ("-wal", "-shm", "-journal", ""):
        Path(f"{path}{suffix}").unlink(missing_ok=True)


def remove_owned_directory(path):
    # NFS can briefly recreate/defer an unlink while closed SQLite journal
    # handles drain. Retry only this fixture's unique directory for 10 seconds.
    deadline = time.monotonic() + 10
    while True:
        try:
            shutil.rmtree(path)
        except FileNotFoundError:
            if not path.exists():
                emit(case="owned_cleanup", status="pass")
                return True
        except OSError as error:
            if error.errno not in (errno.ENOTEMPTY, errno.EBUSY):
                raise
        else:
            if not path.exists():
                emit(case="owned_cleanup", status="pass")
                return True
        if time.monotonic() >= deadline:
            try:
                remaining = sorted(os.listdir(path))[:8]
            except OSError as error:
                remaining = [f"list error: {error}"]
            emit(case="owned_cleanup", status="deferred_until_backing_disposal",
                 remaining=remaining)
            return False
        time.sleep(0.05)


def journal_capability(root, journal):
    path = root / f"capability-{journal.lower()}.sqlite"
    try:
        db = sqlite3.connect(path, timeout=1)
        try:
            actual = str(db.execute(f"PRAGMA journal_mode={journal}").fetchone()[0]).upper()
        finally:
            db.close()
        return actual
    finally:
        clean_db(path)


def load_child(path, journal, worker, transactions):
    db = sqlite3.connect(path, timeout=0.5)
    retries = 0
    latencies = []
    try:
        db.execute("PRAGMA synchronous=FULL")
        if journal == "WAL":
            actual = db.execute("PRAGMA journal_mode").fetchone()[0].upper()
            assert actual == "WAL", (journal, actual)
        print("ready", flush=True)
        assert sys.stdin.buffer.read(1) == b"s", "worker start was not signaled"
        for transaction in range(transactions):
            started = time.monotonic()
            for attempt in range(30):
                try:
                    db.execute("BEGIN IMMEDIATE")
                    db.execute(
                        "INSERT INTO writes(worker, tx_index, payload) VALUES (?, ?, ?)",
                        (worker, transaction, payload(worker, transaction)),
                    )
                    db.commit()
                    break
                except sqlite3.OperationalError as error:
                    db.rollback()
                    if "locked" not in str(error).lower() or attempt == 29:
                        raise
                    retries += 1
            latencies.append(round((time.monotonic() - started) * 1000, 3))
    finally:
        db.close()
    emit(worker=worker, retries=retries, latencies_ms=latencies)


def read_ready(process):
    # A child either announces readiness quickly or exits with a setup error.
    # The outer native test also places a deadline around this whole fixture.
    if not select.select([process.stdout], [], [], 10)[0]:
        raise AssertionError("worker did not announce readiness within 10 seconds")
    line = process.stdout.readline().strip()
    if line != "ready":
        output, error = process.communicate(timeout=10)
        raise AssertionError(
            f"worker setup failed: status={process.returncode}, line={line!r}, "
            f"stdout={output[-400:]!r}, stderr={error[-400:]!r}"
        )


def run_load(root, journal, workers, transactions):
    path = root / f"load-{journal.lower()}.sqlite"
    db = sqlite3.connect(path, timeout=2)
    try:
        actual = db.execute(f"PRAGMA journal_mode={journal}").fetchone()[0].upper()
        assert actual == journal, (journal, actual)
        db.execute("PRAGMA synchronous=FULL")
        db.execute(
            "CREATE TABLE writes (worker INTEGER NOT NULL, tx_index INTEGER NOT NULL, "
            "payload BLOB NOT NULL, PRIMARY KEY(worker, tx_index))"
        )
        db.commit()
    finally:
        db.close()

    processes = []
    started = time.monotonic()
    try:
        for worker in range(workers):
            process = subprocess.Popen(
                [sys.executable, __file__, "--load-child", str(path), journal,
                 str(worker), str(transactions)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            processes.append(process)
            read_ready(process)
        for process in processes:
            process.stdin.write("s")
            process.stdin.flush()
            process.stdin.close()
            process.stdin = None

        results = []
        for process in processes:
            output, error = process.communicate(timeout=WORKER_TIMEOUT)
            assert process.returncode == 0, (
                f"load worker exited {process.returncode}: {error[-800:]}"
            )
            result = json.loads(output.strip())
            assert len(result["latencies_ms"]) == transactions, result
            results.append(result)
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=10)

    db = sqlite3.connect(path, timeout=2)
    try:
        assert db.execute("PRAGMA integrity_check").fetchone() == ("ok",)
        rows = db.execute(
            "SELECT worker, tx_index, payload FROM writes ORDER BY worker, tx_index"
        ).fetchall()
        assert len(rows) == workers * transactions, len(rows)
        for worker, transaction, actual in rows:
            assert actual == payload(worker, transaction), (worker, transaction)
    finally:
        db.close()
    elapsed = time.monotonic() - started
    latencies = sorted(latency for result in results for latency in result["latencies_ms"])
    emit(
        case="load",
        journal=journal,
        status="pass",
        workers=workers,
        transactions_per_worker=transactions,
        committed_rows=len(rows),
        busy_retries=sum(result["retries"] for result in results),
        elapsed_ms=round(elapsed * 1000, 3),
        transactions_per_second=round(len(rows) / elapsed, 3),
        p95_commit_ms=latencies[max(0, int(len(latencies) * 0.95) - 1)],
    )


def hold_child(path):
    db = sqlite3.connect(path, timeout=0.2)
    try:
        db.execute("BEGIN IMMEDIATE")
        db.execute("UPDATE locks SET value=value+1 WHERE id=1")
        print("ready", flush=True)
        assert sys.stdin.buffer.read(1) == b"r", "holder release was not signaled"
        db.rollback()
    finally:
        db.close()


def alias_probe(first, second):
    db_path = first / "alias-lock.sqlite"
    contender_path = second / db_path.name
    db = sqlite3.connect(db_path, timeout=2)
    try:
        db.execute("CREATE TABLE locks(id INTEGER PRIMARY KEY, value INTEGER NOT NULL)")
        db.execute("INSERT INTO locks VALUES(1, 7)")
        db.commit()
    finally:
        db.close()

    holder = subprocess.Popen(
        [sys.executable, __file__, "--hold-child", str(db_path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    bypass = False
    error = None
    try:
        read_ready(holder)
        contender = sqlite3.connect(contender_path, timeout=0.2)
        try:
            try:
                contender.execute("BEGIN IMMEDIATE")
            except sqlite3.OperationalError as caught:
                if "locked" not in str(caught).lower():
                    error = str(caught)
            else:
                bypass = True
            finally:
                contender.rollback()
        finally:
            contender.close()
    finally:
        if holder.poll() is None:
            holder.stdin.write("r")
            holder.stdin.flush()
            holder.stdin.close()
            holder.stdin = None
            output, stderr = holder.communicate(timeout=10)
            assert holder.returncode == 0, (output, stderr)
    verify = sqlite3.connect(db_path, timeout=2)
    try:
        integrity = verify.execute("PRAGMA integrity_check").fetchone()[0]
        value = verify.execute("SELECT value FROM locks WHERE id=1").fetchone()[0]
    finally:
        verify.close()
    emit(
        case="second_view_lock",
        status="lock_bypassed" if bypass else "blocked" if error is None else "error",
        error=error,
        integrity=integrity,
        rolled_back_value=value,
    )
    return bypass or error is not None or integrity != "ok" or value != 7


def cache_visibility_probe(first, second):
    marker = first / "visibility-sentinel.txt"
    mirror = second / marker.name
    expected = b"mount-rs-sqlite-visibility"
    assert not mirror.exists(), "visibility sentinel already exists"
    created_ms = None
    deleted_ms = None
    try:
        with marker.open("wb") as stream:
            stream.write(expected)
            stream.flush()
            os.fsync(stream.fileno())
        started = time.monotonic()
        deadline = started + 10
        while time.monotonic() < deadline:
            try:
                if mirror.read_bytes() == expected:
                    created_ms = round((time.monotonic() - started) * 1000, 3)
                    break
            except FileNotFoundError:
                pass
            time.sleep(0.05)
        marker.unlink()
        started = time.monotonic()
        deadline = started + 10
        while time.monotonic() < deadline:
            if not mirror.exists():
                deleted_ms = round((time.monotonic() - started) * 1000, 3)
                break
            time.sleep(0.05)
    finally:
        marker.unlink(missing_ok=True)
    hazard = created_ms is None or deleted_ms is None
    emit(case="second_view_cache_visibility",
         status="stale_or_missing" if hazard else "pass",
         created_ms=created_ms, deleted_ms=deleted_ms)
    return hazard


def run_suite(root, second, workers, transactions, strict_wal, local_alias_control=False):
    failures = []
    supported = []
    for journal in JOURNALS:
        try:
            actual = journal_capability(root, journal)
        except Exception as error:
            actual = f"error:{type(error).__name__}:{error}"
        if actual != journal:
            status = "unsupported" if journal == "WAL" and not strict_wal else "fail"
            emit(case="journal_capability", journal=journal, actual=actual, status=status)
            if status == "fail":
                failures.append(f"{journal} journal capability: {actual}")
            continue
        emit(case="journal_capability", journal=journal, actual=actual, status="pass")
        supported.append(journal)
        try:
            run_case(root, journal)
            emit(case="kill_reopen_locking", journal=journal, status="pass")
        except Exception as error:
            failures.append(f"{journal} kill/reopen/locking: {type(error).__name__}: {error}")
            emit(case="kill_reopen_locking", journal=journal, status="fail", error=str(error))

    for journal in LOAD_JOURNALS:
        if journal not in supported:
            continue
        try:
            run_load(root, journal, workers, transactions)
        except Exception as error:
            failures.append(f"{journal} load: {type(error).__name__}: {error}")
            emit(case="load", journal=journal, status="fail", error=str(error))

    if second is not None:
        try:
            cache_hazard = cache_visibility_probe(root, second)
            lock_hazard = alias_probe(root, second)
            hazard = cache_hazard or lock_hazard
            emit(case="second_view", status="hazard" if hazard else "no_hazard_observed")
        except Exception as error:
            emit(case="second_view", status="error", error=str(error))
            # Two independent native NFS mountpoints opening one SQLite file
            # are unsupported; this diagnostic result is not an acceptance
            # failure for the supported single-view profile.

    if local_alias_control:
        try:
            cache_hazard = cache_visibility_probe(root, root)
            lock_hazard = alias_probe(root, root)
            hazard = cache_hazard or lock_hazard
            emit(case="local_alias_control", status="fail" if hazard else "pass")
            if hazard:
                failures.append("local alias lock was bypassed or corrupted")
        except Exception as error:
            emit(case="local_alias_control", status="fail", error=str(error))
            failures.append(f"local alias control: {type(error).__name__}: {error}")

    emit(case="summary", failed=len(failures), failures=failures,
         sqlite_version=sqlite3.sqlite_version, supported_journals=supported)
    return bool(failures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--self-test", action="store_true")
    group.add_argument("--mount", type=Path)
    parser.add_argument("--second-view", type=Path)
    parser.add_argument("--lock-only", action="store_true",
                        help="probe conflicting locks without running the journal/load matrix")
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--transactions", type=int, default=8)
    parser.add_argument("--load-child", nargs=4, metavar=("PATH", "MODE", "ID", "COUNT"))
    parser.add_argument("--hold-child", metavar="PATH")
    args = parser.parse_args()
    assert 1 <= args.workers <= 8, "workers must be in 1..8"
    assert 1 <= args.transactions <= 100, "transactions must be in 1..100"
    if args.self_test:
        assert args.second_view is None, "second view requires a mounted view"
        with tempfile.TemporaryDirectory(prefix="mount-rs-sqlite-adversarial-control-") as owned:
            if args.lock_only:
                failed = alias_probe(Path(owned), Path(owned))
                emit(case="local_lock_only_control", status="fail" if failed else "pass")
            else:
                # Validate the contention probe against a normal local path
                # before interpreting it from two distinct NFS client views.
                failed = run_suite(Path(owned), None, args.workers, args.transactions,
                                   True, local_alias_control=True)
    else:
        assert args.mount.is_dir(), "mount must be an existing directory"
        assert args.second_view is None or args.second_view.is_dir(), "second view must exist"
        owned = Path(tempfile.mkdtemp(prefix="mount-rs-sqlite-adversarial-", dir=args.mount))
        try:
            if args.second_view is not None:
                second = args.second_view / owned.name
                deadline = time.monotonic() + 10
                while not second.is_dir() and time.monotonic() < deadline:
                    time.sleep(0.05)
                if not second.is_dir():
                    emit(case="second_view_visibility", status="stale_or_missing",
                         waited_ms=10000)
                    second = None
                else:
                    emit(case="second_view_visibility", status="pass")
            else:
                second = None
            if args.lock_only:
                try:
                    hazard = alias_probe(owned, second or owned)
                    emit(case="native_lock_only", status="hazard" if hazard
                         else "no_hazard_observed")
                except Exception as error:
                    emit(case="native_lock_only", status="error", error=str(error))
                # This deliberately tests an unsupported NFS lock topology.
                # A detected hazard is a useful diagnostic, not a failure of
                # the supported single-view SQLite profile.
                failed = False
            else:
                failed = run_suite(owned, second, args.workers, args.transactions, False)
        finally:
            # Remove only the unique directory made by this fixture.
            cleaned = remove_owned_directory(owned)
            if not cleaned and args.second_view is None:
                # A single-view acceptance run requires exact cleanup. The
                # two-view diagnostic disposes its entire backing after the
                # native test unmounts both clients.
                failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    if "--load-child" in sys.argv:
        index = sys.argv.index("--load-child")
        load_child(Path(sys.argv[index + 1]), sys.argv[index + 2],
                   int(sys.argv[index + 3]), int(sys.argv[index + 4]))
    elif "--hold-child" in sys.argv:
        index = sys.argv.index("--hold-child")
        hold_child(Path(sys.argv[index + 1]))
    else:
        sys.exit(main())
