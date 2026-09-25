# SlateDB on RustFS benchmark

The release-mode runner compares two mount-rs filesystem paths against the
same isolated RustFS service: `KeyValueFs<SlateDbStore>` and `ChunkedFs` with
`SlateDbMetadataStore` plus `RustFsBlockStore`. It times complete write,
full-byte read, and delete operations for ten 4 KiB files and three 1 MiB
files per path. Every read is byte-validated. Raw milliseconds and median/p95
are printed as JSON. Database open, namespace bootstrap, payload creation,
validation, and shutdown are outside the timed operations.
Reads use the same writer immediately after each write, so SlateDB and block
caches may serve them without an object-store request. The read samples are
warm-cache results, not cold remote-read latencies.

From the repository root, build and run through the pinned RustFS harness:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-slatedb-target \
  ./scripts/cargo-shared build --release --locked -p mount-rs-slatedb --example slatedb_rustfs_benchmark
MOUNT_RS_RUSTFS_COMBO_ONLY=1 \
RUSTFS_COMBO_NAME=slatedb-benchmark \
RUSTFS_COMBO_COMMAND="/private/tmp/mount-rs-slatedb-target/release/examples/slatedb_rustfs_benchmark > /private/tmp/slatedb-rustfs-result.json" \
./scripts/test-rustfs.sh
```

Set `CARGO_TARGET_DIR` explicitly when invoking these commands, or use the
checkout-specific directory selected by `scripts/cargo-shared-env.sh`.
The harness creates and removes only its run-owned container, bucket data,
and prefixes. It passes a unique `RUSTFS_COMBO_PREFIX` into the benchmark.
The result is a local, single-node, loopback service measurement. It does not
qualify multi-writer SlateDB operation, replicated RustFS durability, hosted
object stores, or production capacity.

## Local result, 2026-09-25

[Raw release-mode samples](results-2026-09-25-macos-arm64.json) were captured
on macOS 26.6.2 arm64 using the pinned RustFS 1.0.0 image and this dirty
checkout based on `23551a40b16c055179f82af8dc821a7c3bc3fee2`. The
benchmark completed with byte validation and run-owned fixture cleanup.
The displayed medians were recomputed from the captured raw samples with the
usual midpoint average for an even sample count.

| Role | Payload | Iterations | Median write | Median warm read | Median delete |
| --- | ---: | ---: | ---: | ---: | ---: |
| SlateDB key-value filesystem | 4 KiB | 10 | 204.67 ms | 0.038 ms | 100.69 ms |
| SlateDB metadata + RustFS blocks | 4 KiB | 10 | 101.37 ms | 0.050 ms | 101.64 ms |
| SlateDB key-value filesystem | 1 MiB | 3 | 218.32 ms | 0.182 ms | 85.50 ms |
| SlateDB metadata + RustFS blocks | 1 MiB | 3 | 101.86 ms | 0.166 ms | 101.94 ms |

This small sequential sample is diagnostic. It does not establish a stable
speed ratio, cold-read performance, or a production throughput target.

SlateDB's object-store path and the RustFS block prefix must be distinct.
`SlateDbMetadataStore` currently supports the exclusive lease path; MRC2 and
delegated concurrent ownership return `ENOTSUP`. `KeyValueFs` synthesizes
filesystem attributes in process, so its key-value role is best for its flat
object workload, not for persistent POSIX metadata or SQLite hosting.
