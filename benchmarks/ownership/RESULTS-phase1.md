# Historical phase1 exclusive ownership results

Run on macOS 26.6.2 arm64, release build, 128 operations per trial, seven measured trials after one warmup for each of 16 cases. The raw [primary results JSON](results-phase1-macos-arm64.json) contains all durations, provider counts, verified reopen outcomes and source/toolchain fingerprints. Other task builds and tests had completed, and the qualification VM was stopped before this run. The development host otherwise remained unisolated. No artificial latency was injected; cases ran serially. An earlier [concurrent-build run](results-concurrent-build-macos-arm64.json) is retained separately and is not the primary performance evidence.

| Backend | Workload | Sync every | Write-through p50 / p95 ms | Writeback p50 / p95 ms | Publish calls | Block puts | Block flush calls |
|---|---|---:|---:|---:|---:|---:|---:|
| memory | random_pages | 1 | 2.130 / 2.317 | 2.224 / 2.597 | 128 → 128 | 128 → 128 | 256 → 128 |
| memory | random_pages | 16 | 1.768 / 1.959 | 1.796 / 2.130 | 128 → 8 | 128 → 128 | 136 → 8 |
| memory | namespace | 1 | 1.000 / 1.115 | 0.822 / 0.848 | 384 → 128 | 0 → 0 | 384 → 128 |
| memory | namespace | 16 | 0.889 / 1.050 | 0.651 / 0.779 | 384 → 8 | 0 → 0 | 264 → 8 |
| sqlite | random_pages | 1 | 163.989 / 172.051 | 205.857 / 495.746 | 128 → 128 | 128 → 128 | 256 → 128 |
| sqlite | random_pages | 16 | 125.992 / 138.768 | 76.098 / 87.617 | 128 → 8 | 128 → 128 | 136 → 8 |
| sqlite | namespace | 1 | 212.251 / 227.536 | 99.377 / 122.226 | 384 → 128 | 0 → 0 | 384 → 128 |
| sqlite | namespace | 16 | 156.187 / 170.598 | 5.501 / 8.130 | 384 → 8 | 0 → 0 | 264 → 8 |

Every measured trial passed full page-content verification and absence of temporary namespace entries after shutdown and reopen. Counts above are uniform across each case's seven trials. With seven samples, p95 and p99 are the observed maximum. Metadata loads during the timed workloads were zero; page block gets were 72 in both modes. Metadata renew calls are 128 in each mode when syncing every operation, and eight in each mode for batches of 16. SQLite metadata flush calls fell from 128 or eight to zero because durable publication already provides that provider barrier; memory writeback performs one metadata flush per sync.

The durable SQLite random-page batch reduced median elapsed time by 1.66× (125.992 ms to 76.098 ms), and the namespace batch by 28.39× (156.187 ms to 5.501 ms). Syncing after every page produced no median benefit (163.989 ms versus 205.857 ms) and a worse observed p95 (172.051 ms versus 495.746 ms). Memory random-page batch medians also show no improvement (1.768 ms versus 1.796 ms). Writeback did not reduce immutable block puts for the page workload. These measurements establish local provider call and elapsed-time behavior; they do not establish network performance, native FUSE qualification, power-loss durability or shared SQLite across independent mounts.
