# Fresh TiDB / RustFS profiles after the grant-index changes

Measured clean source: `642ff0c868c3a9b16d6fa82b55c952c841d07acf`. Both fresh native runs passed full initial/final payload, size, EOF and membership checks, cross-server reads, scope/revocation denials and clean retirement of all ten server workers. Source/build identity, runtime supervision and offline arithmetic are separate evidence gates.

Rates below are **whole-cluster completed filesystem cycles per second**, including Open/Close. A basic read or overwrite cycle has three acknowledged RPCs. Churn has four. These rates do not describe per-Drive throughput or physical storage IOPS.

## Configuration

- Ten native server processes on one macOS laptop, shared TiDB 8.5.7 metadata and a fresh strict RustFS 1.0.0 blob fixture for each run.
- D10: 10 clients / 10 Drives / 5 Partitions. D20: 20 clients / 20 Drives / 10 Partitions. One client per Drive; two Drives per Partition; lazy construction.
- Each Drive initially contains 1,000 files: 990 at 4 KiB, nine at 128 KiB and one at 1 MiB. The complete initial datasets are 10,000 files / 62,832,640 bytes and 20,000 files / 125,665,280 bytes.
- Sixteen cells per run: mostly-idle and all-active, each with eight patterns. Mostly-idle keeps one client active; all-active keeps every client active with one request in flight per client. Each cell is nominally five seconds; the denominator includes final in-flight completion.
- Negotiated QUIC with local ES256/JWK authentication and MRC5 metadata. Resource/I/O profiling enabled; allocation profiling disabled.
- The generic object-store adapter cache is present. The composed service RAM/disk/QUIC peer cache is unconfigured. External OIDC issuers, mounted Linux/macOS filesystems and cross-host traffic are outside this benchmark.
- The source archive binds 637 Rust/Cargo/build inputs and a fresh release executable. The native pure check set passed 172 tests; six opt-in tests were ignored before the actual native runs.

## All-active throughput

| Pattern | 10 clients cycles/s | 20 clients cycles/s | Observed D20 / D10 |
|---|---:|---:|---:|
| sequential_read | 429.770 | 656.442 | 1.527 |
| random_read | 511.508 | 658.332 | 1.287 |
| sequential_overwrite | 154.370 | 220.300 | 1.427 |
| random_overwrite | 147.796 | 211.040 | 1.428 |
| mixed | 222.243 | 330.886 | 1.489 |
| hot_file | 136.893 | 278.115 | 2.032 |
| append_truncate | 83.834 | 292.787 | 3.492 |
| churn | 22.452 | 58.716 | 2.615 |

Doubling clients and Drives raised basic read/overwrite rates by about 1.3–1.5 times in these observations. Dataset size, cache history, background work and execution history also changed. Each point is a single five-second cell; there are no confidence intervals or isolated scaling/optimization estimates. Both runs remain below 1,000 complete read or overwrite cycles/s.

## Mostly-idle throughput

| Pattern | D10, one active client cycles/s | D20, one active client cycles/s |
|---|---:|---:|
| sequential_read | 102.575 | 110.898 |
| random_read | 111.400 | 118.953 |
| sequential_overwrite | 42.730 | 44.149 |
| random_overwrite | 43.299 | 47.447 |
| mixed | 63.096 | 71.580 |
| hot_file | 30.405 | 55.898 |
| append_truncate | 46.142 | 57.304 |
| churn | 9.774 | 10.712 |

## Measured remaining work

| Run | Pattern | SQL submissions/cycle | Known SELECT rows/cycle | SDK PUT mean ms | HTTP PUT dispatch mean ms | Commit-gate wait mean us | Commit-gate hold mean ms |
|---|---|---:|---:|---:|---:|---:|---:|
| current_d10 | sequential_read | 3.000 | 3.000 | — | — | — | — |
| current_d10 | random_read | 3.000 | 3.000 | — | — | — | — |
| current_d10 | sequential_overwrite | 6.000 | 5.000 | 32.377 | 32.309 | 0.302 | 20.841 |
| current_d10 | random_overwrite | 6.000 | 5.000 | 34.666 | 34.596 | 0.288 | 20.764 |
| current_d10 | churn | 44.000 | 7051.800 | — | — | — | — |
| current_d20 | sequential_read | 3.000 | 3.000 | — | — | — | — |
| current_d20 | random_read | 3.004 | 3.002 | — | — | — | — |
| current_d20 | sequential_overwrite | 6.000 | 5.000 | 44.101 | 44.037 | 0.257 | 29.658 |
| current_d20 | random_overwrite | 6.000 | 5.000 | 49.216 | 49.151 | 0.255 | 28.080 |
| current_d20 | churn | 44.000 | 7050.675 | — | — | — | — |

- Basic reads usually require three classified SQL submissions and three returned rows per cycle. D20 random reads include twelve additional SQL submissions and six additional returned rows above that baseline; the aggregate does not identify their cause.
- Basic overwrites have six SQL submissions, five known returned SELECT rows and one SDK PUT per cycle. SDK PUT means are 32–35 ms at D10 and 44–49 ms at D20. Observed HTTP PUT dispatch means closely match those inclusive SDK spans. This localizes elapsed time to the object-store request/remote-service span; it does not isolate network, RustFS CPU or persistence.
- Mean commit-gate waits are below 0.4 us in these overwrite cells. Gate holds are about 21 ms at D10 and 28–30 ms at D20. Small means do not rule out tail contention. Gate, SQL, SDK and HTTP spans overlap and must not be added as exclusive costs.
- Churn still submits 44 classified SQL calls and returns about 7,051 SELECT rows per cycle. This is a remaining logical metadata amplification seam. Returned rows are not rows scanned, TiKV operations, affected rows or physical IOPS.
- Frontend CPU and RSS endpoints, SQL/HTTP counts, marker work, latency bucket bounds and QUIC UDP counters are retained for every cell in the companion data. Process observation windows include observers/background; RSS peaks are individual process lifetime peaks, not simultaneous fleet memory.

## Focused before/after controls

The [paired grant-index benchmark](../grant-index-paired-20260930/README.md) measured median hot authorization/audit cycle CPU at 10,000 grants falling from 69.399 to 0.754 us/cycle, about 92 times. Both warmed paths made zero allocation/reallocation requests in that scoped window. At ten grants the indexed path cost about 4% more CPU; cold index construction allocates. This excludes JWT validation, catalog reload, transport and storage.

The [legacy fresh-create control](../legacy-create-rebase-20260930/README.md) reduced logical blob PUT calls from three to two and creator publications from two to one, with a complete durable content oracle. It is a provider-call result. The native MRC5 workload already used the concurrent path before this legacy extension, so its improvement cannot be attributed to this predicate change.

## Historical same-geometry context

The retained 24a D10/F1000 observations are included as unpaired context. Source, dates and cache/state history differ; the new external runtime-floor witness does not retroactively upgrade their supervisor evidence.

| Pattern | Historical 24a cycles/s | Current 642 cycles/s |
|---|---:|---:|
| sequential_read | 386.389 | 429.770 |
| random_read | 467.445 | 511.508 |
| sequential_overwrite | 131.149 | 154.370 |
| random_overwrite | 155.693 | 147.796 |
| mixed | 232.094 | 222.243 |
| hot_file | 160.352 | 136.893 |
| append_truncate | 196.341 | 83.834 |
| churn | 32.616 | 22.452 |

The current observations include higher basic read/sequential overwrite rates and lower random overwrite, mixed, hot-file, append/truncate and churn rates. They do not establish a general end-to-end throughput win or a causal regression.

## Backing-store I/O accounting

| Run | Guest cgroup device | Whole capture seconds | Accounted reads | Accounted writes | Read bytes | Write bytes |
|---|---|---:|---:|---:|---:|---:|
| current_d10 | 254:0 | 435.064982 | 15388 | 69257 | 124162048 | 344125440 |
| current_d20 | 254:0 | 674.199155 | 19003 | 114917 | 151822336 | 607031296 |

These RustFS Linux guest cgroup counters span setup, all cells, complete-content checks, background work and writeback. They cannot be divided by individual active-cell durations or treated as host SSD IOPS. Contemporary TiKV cgroup operation counts and physical Mac storage IOPS are unavailable in these runs. Missing coverage stays unavailable.

## Runtime and correctness qualification

| Run | Whole supervisor seconds | Host-floor samples | Max sample gap ms | Minimum host free bytes | Cross-server verified pairs |
|---|---:|---:|---:|---:|---:|
| current_d10 | 458.209097 | 4502 | 267.003792 | 79377383424 | 100 |
| current_d20 | 697.285627 | 6866 | 148.441708 | 78392496128 | 200 |

The external supervisor enforced a 3,300-second whole limit, 120-second cleanup reserve, 64 MiB per-stream captures and the fixed 64 GiB host free-space floor. Requested disk sampling was 100 ms; actual maximum gaps are retained above. Both runs finished without abort, signal, forced stop or retry. Their pinned owner receipts qualify nested worker/collector retirement and stopping the exact new RustFS container; outer owner-group absence alone is insufficient.

All original eight backend containers, their lifecycle/configuration/network identities and six volumes were preserved. Owned stopped fixture data is retained; dataset absence and crash/power-loss durability are not established. The 10,000-client / 10,000-Drive / 5,000-Partition / 1,000-files-per-Drive target remains unqualified.

## Artifact identity

- Release executable SHA-256: `b291c9a4e3233efa1aca262d8f1b47dbd9aa4f129aaf9fbb28e5b8ad8be832b2` (43,018,944 bytes).
- Archived source closure SHA-256: `612295cd5c4b25f7172f3c3478e16f082b0ce7d0087a539d1a4b5e0ca3eada58`; manifest: `aff979fcd60d81f00e2d493a4d69a226e9361d4babbc01f5e0dbf09bb5fd3c5f`.
- Offline extractor checks every manifest-listed source/binary binding and the fixed owner/terminal evidence. Root independently recomputed the source closure during build qualification. The extraction does not reconstruct that closure algorithm or inventory unlisted files.
- Public observations: 27,905,227 decoded bytes, SHA-256 `ba9f6df6e9b6fd7244de84934cccc70eb1695b57048a78f1a9e85e9b00ca17fd`. Deterministic gzip (mtime=0): 1,891,484 bytes, SHA-256 `c27e21291a77d0d608fc83fe0c7a4c228ae07250b1587c0f352d406175888302`.
- `summary.json` retains all 48 cells, exact rational inputs and selected metric profiles. `observations.json.gz` retains the full public projection with individual process observations and archive/metric/oracle hash bindings. `qualification.json` joins the fresh build/checks, actual runtime supervisors and offline extraction.
- Independent offline verification passed all 48 cell/table joins, exact rational rates, ten-worker scalar sums, public privacy checks and both actual supervisor projections. Verifier source SHA-256: `04990d18a658d66cfc154cbd0e71f73b253c548136a4c7cb909d0bc6bb83b294`; actual verification receipt: `1f0a4e7b1c859bb348a737f35020637c08e58008cfb2464edc7ea15a5f3043f0`. Its one positive and five negative controls passed separately, receipt `09505f2de23e8aab17a18048471c885f4a8c94d642c19c8cafdbbd521722cc31`.
