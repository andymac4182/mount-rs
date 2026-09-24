"""Real SQLite workload inside one owned directory, shared providers elsewhere."""
import sys
import sqlite3
import json
from pathlib import Path

import sqlite_exclusive_qualification
import sqlite_hosting

if __name__ == "__main__":
    directory = Path(sys.argv[1]).resolve()
    outside = directory.parent / ("right" if directory.name == "left" else "left") / "outside-owned-scope.sqlite"
    try:
        unexpected = sqlite3.connect(outside)
    except sqlite3.OperationalError:
        pass
    else:
        unexpected.close()
        raise AssertionError("kernel mount permitted SQLite outside its checked-out scope")
    verify_only = "--verify-only" in sys.argv
    print(json.dumps({"phase": "scope_start", "scope": directory.name, "verify_only": verify_only}), flush=True)
    try:
        if not verify_only:
            sqlite_hosting.run(directory)
        sqlite_exclusive_qualification.verify(directory)
    except Exception:
        print(json.dumps({"phase": "scope_failure", "scope": directory.name, "verify_only": verify_only}), flush=True)
        raise
