#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
output="${1:-benchmarks/ownership/results.json}"
mkdir -p "$(dirname "$output")"
./scripts/cargo-shared run --release --locked -p mount-rs-chunked --example ownership_benchmark > "$output.tmp"
python3 benchmarks/ownership/stamp.py "$output.tmp"
mv "$output.tmp" "$output"
python3 benchmarks/ownership/summarize.py "$output"
