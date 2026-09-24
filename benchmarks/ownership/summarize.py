#!/usr/bin/env python3
"""Summarize raw samples; retain the source JSON for independent analysis."""
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    data = json.load(source)


def count_range(case, key):
    values = [trial["counts"].get(key, trial["counts"]["metadata_publish"])
              for trial in case["trials"]]
    low, high = min(values), max(values)
    return str(low) if low == high else f"{low}..{high}"


print("backend workload sync_every mode p50_ms p95_ms publish_attempts publish_success block_puts block_flushes")
for case in data["cases"]:
    dist = case["elapsed_distribution"]
    print(case["backend"], case["workload"], case.get("sync_every_operations", "immediate"),
          case.get("protocol", "writeback" if case.get("writeback") else "write-through"),
          f'{dist["p50_ms"]:.3f}', f'{dist["p95_ms"]:.3f}',
          count_range(case, "metadata_publish"), count_range(case, "metadata_publish_success"),
          count_range(case, "block_put"), count_range(case, "block_flush"))
    operations = case.get("operation_distribution")
    if operations:
        print(f'  operation_ms p50={operations["p50_ms"]:.3f} '
              f'p95={operations["p95_ms"]:.3f} p99={operations["p99_ms"]:.3f} '
              f'n={operations["samples"]}')
