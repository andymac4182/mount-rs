"""Native exclusive writeback probe; reuse real multiprocess SQLite workload."""
import json
import sqlite3
import sys
from pathlib import Path

import sqlite_hosting


def verify(directory):
    for journal in ("DELETE", "WAL"):
        path = directory / (journal.lower() + ".sqlite")
        # mode=rw prevents a missing durable database from being recreated.
        with sqlite3.connect(path.as_uri() + "?mode=rw", uri=True) as db:
            sqlite_hosting.integrity(db)
            expected = [(1, b"uncommitted")] + [
                (index, b"x" * 8192)
                for index in range(2, sqlite_hosting.SPILL_ROWS + 2)
            ]
            actual = db.execute("SELECT id, payload FROM items ORDER BY id").fetchall()
            assert actual == expected, "durable SQLite rows or payload bytes differ"
        print(json.dumps({"phase": "durable_reopen", "journal": journal, "result": "pass"}), flush=True)


if __name__ == "__main__":
    root = Path(sys.argv[1]).resolve()
    if "--verify-only" not in sys.argv:
        sqlite_hosting.run(root)
    verify(root)
