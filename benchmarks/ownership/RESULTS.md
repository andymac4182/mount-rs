# Measured exclusive ownership results

The current [raw primary run](results-macos-arm64.json) records host, toolchain, source fingerprints and all samples. It includes exclusive and delegated suites. Exclusive cases use 128 logical operations per trial and seven measured trials after one warmup. Final task builds/tests had completed and the qualification VM was stopped; the development host otherwise remained unisolated. No artificial latency was injected. Cases ran serially. [Historical phase1 evidence](RESULTS-phase1.md) and [earlier concurrent-build raw](results-concurrent-build-macos-arm64.json) are retained separately.

| Backend | Workload | Sync every | Write-through p50 / p95 ms | Writeback p50 / p95 ms | Publish calls | Block puts | Block flush calls |
|---|---|---:|---:|---:|---:|---:|---:|
| memory | random_pages | 1 | 1.997 / 2.039 | 2.106 / 2.259 | 128 → 128 | 128 → 128 | 256 → 128 |
| memory | random_pages | 16 | 1.802 / 1.834 | 1.599 / 1.754 | 128 → 8 | 128 → 128 | 136 → 8 |
| memory | namespace | 1 | 1.033 / 1.197 | 0.787 / 0.827 | 384 → 128 | 0 → 0 | 384 → 128 |
| memory | namespace | 16 | 0.838 / 0.888 | 0.640 / 0.660 | 384 → 8 | 0 → 0 | 264 → 8 |
| sqlite | random_pages | 1 | 147.525 / 163.383 | 150.445 / 154.218 | 128 → 128 | 128 → 128 | 256 → 128 |
| sqlite | random_pages | 16 | 108.669 / 143.675 | 64.643 / 71.161 | 128 → 8 | 128 → 128 | 136 → 8 |
| sqlite | namespace | 1 | 205.656 / 219.320 | 59.123 / 102.251 | 384 → 128 | 0 → 0 | 384 → 128 |
| sqlite | namespace | 16 | 141.257 / 166.180 | 5.405 / 5.920 | 384 → 8 | 0 → 0 | 264 → 8 |

SQLite sync16 median ratios are 1.68× for page writes and 26.13× for namespace operations. The table reports both median and observed high-tail behavior; it does not establish a universal speedup. Immutable block puts are unchanged for exclusive page writes.

Every measured trial verified the complete page file and absence of temporary namespace entries after shutdown and driver reopen. Counts describe provider API calls, not SQL statements or device I/O. With seven trials, p95 and p99 of total duration equal the observed maximum. Sync is included in elapsed time. Memory is volatile, and SQLite uses its existing file-backed provider durability configuration. These results do not establish network performance, power-loss durability, native transport behavior or independent shared SQLite safety.
