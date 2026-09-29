# TiDB metadata + RustFS blobs: marker metrics and first paired diagnostic

The [report](report.json) records tested code hashes, gate receipt hashes, actual
native-addon identity, scalar native counters, resource coverage, and persistence
checks from 2026-09-28. The tested working tree was based on `36bcf622`; its
recorded code hashes identify the metrics changes beyond that commit.

## Observed performance

| Workers | Drives | Mixed logical operations/s | Median write ms | Median read ms | Distinct changed blobs |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 102.50 | 14.06 | 4.81 | 32 |
| 8 | 1 | 177.48 | 47.71 | 41.25 | 4 |

Each cell performed 32 partial overwrites and 32 verified full-file reads:
4,096-byte changed payloads in 4,098-byte files, with a 65,536-byte chunk size.
The application timer excludes lane setup/open/close. Native/container counters
include that work: their windows were 674.50 and 623.36 ms, versus timed windows
of 624.39 and 360.60 ms. Server metric snapshots enclose the entire profile cell.
The differing content reuse prevents an isolated concurrency-scaling comparison.

## Where the time and requests went

At eight workers, mode-specific shared filesystem gate waits accumulated
2,343.636 ms: reads 1,185.765 ms, write commit 709.060 ms, and write preparation
448.786 ms. The single-worker total was 0.03624 ms. These are overlapping caller
waits, not CPU consumption. The current evidence points to the shared local
operation gate. Pool checkout inclusive totals were only 0.161 and 0.137 ms;
the selected server retry/deadlock counters did not increase in either enclosing
capture. This does not establish the absence of TiDB contention at higher load.

All 64 ordinary blob gets in each native workload window hit the cache, so these
cells do not measure cold S3 reads. Backing verification still made 74 and 144
marker GETs, with the same number of body reads. The one-worker cell made 33
fresh blob creates. At eight workers, 40 logical puts used 23 leaders and 17
successful followers: five fresh creates and 18 expected conditional conflicts,
followed by verification reads. Raw adapter errors from that conflict path are
separate from application failures; both cells had zero failed logical operations.

## Validation and limits

Both cells used the actual Cargo-emitted native addon, exported all 116 Storage
rows, settled their work, and passed shutdown, two fresh reopens, full canary
bytes/EOF, removal, and empty-root checks. The disposable fixture had one PD,
one TiKV, one TiDB and one RustFS. Its container caps totalled 6 GiB within the
measured Docker VM allocation. Both caller durability assertions were false;
production durability and capacity are unqualified. Exact owned Docker containers, volumes and network were removed after the
consumers settled. Private evidence and RustFS host bind data were retained;
Docker Desktop data was preserved.

The marker addition passed the focused tests, signed CLI exports, formatting,
both strict Clippy modes and the full local workspace, including default parallel
test execution (1,940 passed, zero failed, 246 ignored). The warmed Span recorder
reported zero allocations across 38 selected rows, including all six new rows.
Whole metadata/RPC allocation counts, physical device IOPS, Docker block-operation
counts and RustFS server Prometheus metrics remain unavailable. Ignored tests,
hosted CI and the production qualification are separate gates.

## Next controlled comparison

Use mostly separate Drives with matched distinct writes, then the remote cluster
path. The current native runner opens one Drive per provider. The existing remote
TiDB saturation and production harnesses configure TiDB for both metadata and
blocks, so they need an explicit RustFS block selection before their results can
represent this deployment. The production target remains 10 servers, 10,000
clients/Drives, 5,000 Partitions and 1,000 files per Drive.
