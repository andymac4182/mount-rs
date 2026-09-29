# TiDB indexed MRC5 benchmark — 2026-09-29

**Indexed MRC5 reduces directory metadata bytes, but every completed throughput comparison regresses.** The 10-client wire ABBA aggregate loses 23.6% read IOPS, 26.9% write IOPS and 6.9% mixed IOPS. At 1,000 files/drive, SDK reads return 92.8% fewer metadata bytes while completing 56.1–62.8% fewer whole-file cycles per second.

The 100-client pair also regresses: read IOPS fall 9.7%, writes 15.7% and mixed 15.3%. All ten measured arms are qualified. [report.json](report.json) contains compact derived metrics and input hashes.

## Sources and fixture

Baseline `93ad96a` is prior **MRC5 monolithic**, and current `7d19345` is indexed **MRC5**. Both metric banks contain the same 118 labels. All profiling rows are joined by name.

Wire tests have ten service instances in **one process and one Partition**, with a separate Drive per active client. The completed ABBA comparison has ten clients/drives; the completed 100-client pair has 100 clients/drives. Each client has depth one and a preopened handle, performs 4 KiB binary v2 I/O over 32 hot blocks/file, and uses warm SDK/catalog caches. Audit logging is included; authentication is synthetic and the production remote client is bypassed.

SDK tests have two drives and five independent filesystem/pool clients per drive. Each pattern acknowledges 1,000 cycles: Open, one 4 KiB Read with bytes/EOF verification or Overwrite, then Close. Each lane runs 100 iterations. SDK cycles include work outside a preopened wire operation and are reported separately. Cache capacity/admission gauges exist; cache hit/miss counts and all-file warmness are unavailable. No peer cache is configured.

Release runs enable allocation/resource profiling and use 16 Tokio workers. The retained diagnostic fixture has TiDB v8.5.7 (one TiDB, three PD, three TiKV) and loopback RustFS 1.0.0 with durable blocks and a persistent bind mount. Its Docker VM has 14 CPUs and 16,745,295,872 B memory. Maximum measured concurrency is **100 active clients**; this does not qualify production behavior with 10,000 clients.

## Preopened wire operations: ten clients

Two runs per variant execute in baseline/current/current/baseline order. Aggregate IOPS is total completed operations divided by total elapsed workload time including drain. Ranges show individual runs. Latencies are per-run power-of-two histogram upper bounds; they are not pooled quantiles.

| Mode | Baseline IOPS (range) | Current IOPS (range) | Change | p95 upper ms baseline → current |
|---|---:|---:|---:|---|
| read | 2150.5 (2105.5–2195.5) | 1643.0 (1389.0–1897.5) | -23.6% | read 8.192–8.192 → read 8.192–16.384 |
| write | 199.8 (195.7–203.8) | 146.1 (132.1–160.2) | -26.9% | write 65.536–65.536 → write 131.072–262.144 |
| mixed | 360.0 (352.7–367.2) | 335.3 (314.5–356.2) | -6.9% | read 8.192–8.192, write 65.536–65.536 → read 8.192–16.384, write 65.536–131.072 |

Writes add one named inode SQL read (2 → 3 per success), while transaction begin/commit remain one each. TiDB process statements are 8 → 9 per write; reads retain two. Read inode SQL inclusive spans rise 4.351 → 5.703 ms/success; writes rise 4.104 → 8.114 ms. TiKV storage-command counts rise 1.698 → 4.030 per read and 3.541 → 5.057 per write.

Returned inode JSON remains 8,924 B/logical success and serialized inode JSON remains 4,462 B/successful write. Including anchor bytes, known returned metadata is about 9,554 → 9,577 B/success, or 2.333 → 2.338 times the 4 KiB payload. This small namespace does not reduce inode payload bytes. SQL byte fields are unavailable; these numbers use core named provider counters.

| Mode | Rust allocations/success baseline → current | Allocated B/success baseline → current | Process CPU us/success baseline → current |
|---|---:|---:|---:|
| read | 321.6 → 337.6 | 61938 → 62765 | 442.4 → 455.8 |
| write | 878.1 → 917.3 | 229812 → 234936 | 1460.4 → 1550.1 |
| mixed | 599.7 → 626.9 | 145922 → 148548 | 926.3 → 994.2 |

These allocation and CPU costs cover the whole measured process pipeline and background, exclude foreign C allocators, and do not isolate metadata. Values are nonzero; no zero-allocation claim follows. TiKV VM cgroup write bytes per successful write are approximately 34,514 → 34,464 B and write operations 5.790 → 5.842. Those VM counters do not prove physical SSD amplification.

## Preopened wire operations: 100 clients

One qualified run per variant uses 100 active clients and 100 separate drives, with ten service instances in one process and one Partition. Each variant verifies all 100 files with zero logical failures.

| Mode | Baseline IOPS | Current IOPS | Change | p95 upper ms baseline → current |
|---|---:|---:|---:|---|
| read | 2748.6 | 2481.0 | -9.7% | read 65.536 → read 65.536 |
| write | 243.7 | 205.3 | -15.7% | write 524.288 → write 1048.58 |
| mixed | 442.6 | 375.0 | -15.3% | read 32.768, write 524.288 → read 65.536, write 1048.58 |

The added write inode query repeats (2 → 3), while transaction begin/commit remain one each. Read inode SQL counts stay two, with observed session calls 0.002602 → 0.000962 per success; these nonzero session counts are retained. TiKV storage-command counts rise 1.633 → 3.666 per read, 3.883 → 5.842 per write, and 3.023 → 5.081 per mixed success.

| Mode | Rust allocations/success baseline → current | Allocated B/success baseline → current | Process CPU us/success baseline → current |
|---|---:|---:|---:|
| read | 323.7 → 340.0 | 62245 → 63071 | 510.0 → 534.1 |
| write | 880.2 → 922.9 | 230469 → 235612 | 1711.5 → 1856.4 |
| mixed | 600.3 → 627.6 | 146299 → 149520 | 1081.0 → 1150.8 |

Full run times are 241.935 → 252.300 s. Provisioning takes 55.602 → 60.662 s, driver setup 58.846 → 64.410 s, and setup 13.731 → 12.667 s. Timed workload windows include drain; lifecycle durations are separate from IOPS. This is a 100-client diagnostic, with one run per variant, and provides no production 10,000-client qualification.

## SDK whole-file cycles: directory cardinality

One matched run per variant is available at each file count, using the same source-bound harness. F100 runs baseline then current; F1000 reverses the order. Operation counts, observed cache gauges and HTTP connector attempt/byte totals match within each pair.

| Files/drive | Pattern | Cycles/s baseline → current | Change | p95 cycle ms baseline → current | Returned metadata B/payload baseline → current |
|---|---|---:|---:|---:|---:|
| 100 | mixed | 294.4 → 270.9 | -8.0% | 57.755 → 61.949 | 8110.8 → 3455.8 |
| 100 | random_overwrite | 178.4 → 175.7 | -1.5% | 68.276 → 67.903 | 7044.8 → 2671.8 |
| 100 | random_read | 734.1 → 644.9 | -12.1% | 17.767 → 21.060 | 9176.7 → 4239.7 |
| 100 | sequential_overwrite | 182.9 → 172.5 | -5.7% | 62.308 → 66.754 | 7044.8 → 2671.8 |
| 100 | sequential_read | 685.7 → 610.6 | -11.0% | 19.630 → 22.900 | 9176.7 → 4239.7 |
| 1000 | mixed | 240.8 → 228.6 | -5.0% | 71.063 → 82.474 | 54929.0 → 3479.5 |
| 1000 | random_overwrite | 153.8 → 143.7 | -6.6% | 81.310 → 109.096 | 50259.6 → 2690.1 |
| 1000 | random_read | 474.7 → 208.4 | -56.1% | 28.256 → 59.042 | 59598.4 → 4268.9 |
| 1000 | sequential_overwrite | 155.3 → 126.0 | -18.9% | 80.097 → 105.639 | 50259.4 → 2689.9 |
| 1000 | sequential_read | 515.6 → 192.0 | -62.8% | 24.461 → 59.784 | 59597.9 → 4268.4 |

Current performs one successful `sdk.metadata.load_compact_root_file` call per cycle in every pattern. Capability initialization is outside timed windows; the timed root-file-capability count is zero. Selected SQL calls fall 6 → 5/read, stay 6/overwrite, and fall 6 → 5.5/mixed. **Selected SQL** means inode/metadata reads and writes; session calls and transaction boundaries are separate. Including session queries, overwrite totals are 7 → 7 and mixed totals 6.5 → 6.0. These method counts are distinct from TiDB process statement counters.

At F1000, known returned metadata falls 92.8% for reads, 94.6% for overwrites and 93.7% for mixed. Allocated bytes/payload fall 24.9–26.9% for reads, 19.2–19.3% for overwrites and 22.6% for mixed. Inode serialization remains about 456 B/write. Byte savings are verified; throughput improvements are not.

| Files/drive | Setup s baseline → current | Full run s baseline → current | Timed five-stage s baseline → current |
|---|---:|---:|---:|
| 100 | 11.354 → 14.315 | 69.835 → 72.804 | 17.291 → 18.367 |
| 1000 | 118.984 → 197.547 | 222.118 → 340.669 | 21.139 → 29.276 |

All SDK runs verify every stored file in full, check EOF and exact root membership, and pass owned cleanup and shutdown. F100 verifies 200 files/819,200 B; F1000 verifies 2,000 files/8,192,000 B. Full run times include setup/population, observer work, the whole-file oracle and purge; these costs are not represented by timed cycle rates.

## Localized waiting and remaining evidence gap

Within current F100 → F1000, root-file lookup inclusive spans rise 4.550 → 33.780 ms/sequential read and 4.374 → 31.955 ms/random read. Total inode SQL spans rise 14.179 → 46.224 ms and 13.794 → 44.246 ms per cycle, with bounded bytes and unchanged call counts. Additional cold payload requests also contribute: HTTP attempts/cycle rise 2.16 → 2.8 and 2 → 2.388, with dispatch spans 1.912 → 4.132 ms and 1.502 → 3.031 ms.

The root-entry query has verified LEFT joins with primary-key/name-hash predicates and full-name equality. No EXPLAIN plan or request-specific TiKV trace establishes the exact plan cause. Runs are serial against changing retained datastore state; the F1000 root-file span falls across stages from 33.8/32.0 ms in reads to 23.5/11.4/5.6 ms later. These spans localize waiting and do not prove a cardinality-only cause. Inclusive nested spans overlap and must not be added as exclusive CPU time.

## Reproduce the SDK harness

Use identical `crates/mount-rs-service/tests/sdk_tidb_metadata_benchmark.rs` bytes in both checkouts, Rust 1.95.0, release/locked builds with allocation profiling, one build job, and a separate `CARGO_TARGET_DIR` per checkout. Set `MOUNT_RS_INDEXED_BENCH_SOURCE_REVISION` to that checkout's actual HEAD during compilation and execution. Build before timing; run binaries serially with no Cargo/build activity during measured runs.

```sh
bench_revision="$(git rev-parse HEAD)"
export MOUNT_RS_INDEXED_BENCH_SOURCE_REVISION="$bench_revision"
export CARGO_TARGET_DIR="/absolute/private/target-for-this-checkout"
RUSTUP_TOOLCHAIN=1.95.0 ./scripts/cargo-shared test \
  -p mount-rs-service --test sdk_tidb_metadata_benchmark \
  --release --locked --features allocation-profiling -j1 --no-run
```

Supply an explicitly owned fixture through `MOUNT_RS_TIDB_URL`, `MOUNT_RS_RUSTFS_ENDPOINT`, `MOUNT_RS_RUSTFS_BUCKET`, `MOUNT_RS_RUSTFS_REGION`, `MOUNT_RS_RUSTFS_ACCESS_KEY_ID`, `MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY`, `MOUNT_RS_REMOTE_RUSTFS_CID`, and `MOUNT_RS_BACKING_RUSTFS_OWNER`. Set `MOUNT_RS_RUSTFS_DURABLE=1` and `MOUNT_RS_PROFILE_IO=1`. Concrete connection settings and credentials remain private.

Set `MOUNT_RS_INDEXED_BENCH_DRIVES=2`, `MOUNT_RS_INDEXED_BENCH_LANES_PER_DRIVE=5`, `MOUNT_RS_INDEXED_BENCH_FILES=100` or `1000`, and `MOUNT_RS_INDEXED_BENCH_ITERATIONS=100`. Set `MOUNT_RS_INDEXED_BENCH_OUTPUT` to a new absolute private output path. Invoke the emitted test executable directly:

```sh
"/absolute/private/path/to/sdk_tidb_metadata_benchmark-test-binary" \
  --ignored --exact actual_sdk_tidb_rustfs_metadata_benchmark --nocapture
```

The retained measurements bind to the original harness hash recorded below. Later cleanup-only hardening does not requalify an old binary against new source; any new matched run must retain its own source/binary receipts.

## Coverage and provenance

All qualified wire windows have zero logical failures, no counter resets, complete observed metric series and successful stored/fresh file verification. Datastore capture windows are approximately 5.79–6.39 s around approximately five-second workloads and include background activity. Linux VM cgroup/proc I/O does not measure physical Mac SSD IOPS; the host block device is unselected, and RustFS physical/backend I/O is outside the TiDB observer. Zero VM read counters do not establish no storage reads.

Direct SDK boundaries are observed. Production remote client, OIDC validation, service diagnostic observer, peer-cache and NAPI forwarding-box coverage are unavailable or absent in the wire fixture. Allocator atomics affect throughput. Durable settings and successful byte verification do not test crash or power-loss durability. These diagnostic fixtures provide no production capacity qualification.

The original baseline-a supervisor expected the test name and `ok` on one line; observer output separated them. Its completed single named case, bare `ok`, one-passed summary and unchanged artifact hash were requalified without replay. `report.json` records qualified raw artifact/receipt hashes, binary/harness/config/observer identities and this correction. Raw connection config, credentials, endpoints and broad HTTP inventories are not committed.

The measured SDK harness is preserved privately at SHA-256 `c7b7e2c0c513f3ffdeb1773cdc4815dcd00c14721b4f836ec76cd4c5758a285f` (57,943 B), identical across measured baseline/current runs and bound to their binaries. Later additive cleanup hardening streams bounded inventories across all owned scopes before any mutation. It does not change workload, setup, oracle or metrics code; these timings were not replayed for that cleanup change.
