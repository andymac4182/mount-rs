#!/usr/bin/env python3
"""Attach host/toolchain and relevant-source fingerprints to a completed run."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys

path = Path(sys.argv[1])
data = json.loads(path.read_text(encoding="utf-8"))
roots = [Path("src"), Path("filesystems/mount-rs-chunked/src"),
         Path("providers/mount-rs-memory/src"), Path("providers/mount-rs-sqlite/src")]
sources = sorted(p for root in roots for p in root.rglob("*.rs"))
sources.extend(Path(p) for p in [
    "filesystems/mount-rs-chunked/examples/ownership_benchmark.rs",
    "Cargo.toml", "Cargo.lock", "filesystems/mount-rs-chunked/Cargo.toml",
    "providers/mount-rs-memory/Cargo.toml", "providers/mount-rs-sqlite/Cargo.toml",
    "scripts/cargo-shared", "benchmarks/ownership/run.sh",
])
data["provenance"] = {
    "completed_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "host": platform.platform(),
    "architecture": platform.machine(),
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "source_sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in sources},
    "samples_override": os.environ.get("BENCH_SAMPLES"),
    "operations_override": os.environ.get("BENCH_OPERATIONS"),
    "suite_override": os.environ.get("BENCH_SUITE"),
    "run_context": os.environ.get("BENCH_CONTEXT", "Unisolated development host"),
    "rustflags": os.environ.get("RUSTFLAGS"),
}
path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
