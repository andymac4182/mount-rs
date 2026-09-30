# TiDB point-query executor tuning — 2026-09-29 UTC

**Reads improve 13.7%, writes 10.8%, and mixed traffic is essentially unchanged in this one matched local pair.** TiDB allocated bytes per completed read fall 35.6%; Rust allocations remain essentially unchanged. The write gain coincides with a faster backing PUT span. This experiment does not attribute that gain to the SQL hints.

## Compared change and workload

- Control: `801ef633d01d7ef1dc95e0dc5a65b57ddb34ebed`.
- Candidate: `2fef4cf7d5fe639fd3a446f9a78c87d881d54713`.
- Selected-file SQL adds `SET_VAR(tidb_max_chunk_size=32)` and `SET_VAR(tidb_executor_concurrency=1)`. Its five bindings, primary-key predicates and selected columns remain identical. Root-entry and write-authority SQL, other runtime code, wire format and stored representation are unchanged between these revisions.
- 100 active QUIC clients and 100 separate Drives, one Partition, ten server instances in one process. Depth one, preopened handles, one file per Drive containing 32 hot 4 KiB blocks. Nominal five-second read/write/mixed stages with drain included in throughput. Synthetic authentication, binary QUIC v2, 16 runtime workers and identical profiling/audit settings. Candidate runs first, then control.
- Actual TiDB v8.5.7: one TiDB, three TiKV and three PD containers; RustFS 1.0.0. Docker VM has 14 CPUs and approximately 16.7 GB RAM. The same live RustFS memory limit is 2 GiB; the older ready receipt declares 1 GiB. All eight container identities, images, resources and restart/OOM state remain stable. Builds finish before measurements; no concurrent Cargo/rustc is observed.

These are **whole-cluster logical operations/s**, rather than per-Drive rates or physical SSD IOPS. Wire operations exclude Open/Close. Warm cache behavior, launch order and background activity remain confounders. One pair does not establish repeatability, statistical significance or production capacity.

## Throughput

| Pattern | Control ops/s | Candidate ops/s | Observed change |
|---|---:|---:|---:|
| Read | 2,146.37 | 2,441.09 | +13.73% |
| Write | 191.60 | 212.27 | +10.79% |
| Mixed | 375.89 | 376.23 | +0.09% |

All six timed stages have zero failures. Both named workloads pass stored and fresh-context byte/EOF verification for all 100 Drives, persisted MRC5/backing checks and shutdown. Each exact owned scope is cleaned once, with all seven SQL families and bounded RustFS prefixes verified absent.

## Allocations and remaining amplification

| Pattern | Rust allocations/op, control → candidate | TiDB allocated B/op | TiDB heap objects/op |
|---|---:|---:|---:|
| Read | 344.33 → 343.98 | 632,802 → 407,269 | 6,528.85 → 4,887.97 |
| Write | 903.30 → 904.77 | 669,727 → 561,855 | 6,906.82 → 6,113.30 |
| Mixed | 621.07 → 621.20 | 647,259 → 478,254 | 6,708.01 → 5,475.73 |

Rust counters cover the instrumented process pipeline and exclude foreign allocations. TiDB counters cover the entire process over wider observer windows, including background work, divided by completed logical operations. They measure allocation traffic, rather than retained memory or request peaks. [wire100.json](wire100.json) retains raw totals, windows and denominators.

Selected data SQL calls remain 2/read, 4/write and approximately 3/mixed operation; session and transaction commands are separate. TiDB→TiKV reads remain approximately 6.006 RPC/op in both arms. Known returned metadata remains approximately 9,576 B per 4 KiB payload, about 2.34×, excluding SQL protocol bytes. There is no measured physical-I/O reduction.

Inclusive inode SQL time falls 45.821 → 40.415 ms/read; pool checkout is only 14.65 → 12.59 µs/read. In the write stage, backing PUT await falls 443.397 → 395.726 ms/write, while metadata publication worsens 19.969 → 21.378 ms/write. The upload seam still accounts for the largest observed write wait. Overlapping wall spans must not be added as exclusive CPU, network or SQL-lock time. RustFS daemon/disk timing and pure socket-network time are unavailable.

The p50/p95/p99 histogram upper bounds are unchanged in this pair. Equal buckets can hide changes within them. [Detailed wire results](wire100-details.md) retain every bound, CPU/resource counter, interface counter, RPC class and PUT normalization by acknowledged writes.

## Isolated query tradeoffs

Four separate warmed-connection probes use 30 alternating prepared executions per arm at 100 and 1,000 empty files. All typed hit/miss oracles match, every measured arm records 30/30 prepared-cache hits, and all seven SQL families are cleaned.

- Correlated-volume rewrite: mean latency worsens 33.6–44.7%.
- One worker: mean improves 6.1–10.2%; displayed join memory is unchanged.
- Maximum chunk 32: HashJoin memory displays fall 141.5/100.6 KB → 79.1/77.1 KB, but mean latency worsens 1.1–3.3%.
- Combined settings: medians improve at both populations, but the 1,000-file mean worsens 1.4% and p95 worsens 34.5%.

These short samples do not establish a tail distribution. Operator memory displays are not allocated bytes or whole-request peaks. Limiting workers and row batches may hurt larger scans, so hints apply only to this point statement. [Query probes and tagged source explanation](query-probes.md), [sanitized probe measurements](query-probes.json).

## Validation record and recovery

**The original suite remains failed and unqualified.** The preserved control binary reports Git at its embedded original compiler directory, which now contains candidate `2fef4cf7`; it was compiled from control `801ef633` and executed from a separate clean control checkout. The suite incorrectly required those directory revisions to match and stopped after the control workload passed, before its cleanup.

A separate immutable recovery audit retains that failure and the compiler-directory observation. It independently verifies captured full 559-file source closures, Cargo artifact logs, the exact frozen executable copy, compiled revision, execution checkout, unchanged runner/support bytes and before/after fixture identity. It then performs the previously unattempted exact control cleanup once. Root observes terminal exit 0 before report qualification. No workload I/O is replayed; no original measurement or failed receipt is rewritten.

An initial recovery preflight also remains failed: analysis expected a string where persisted-mode receipts contain objects. It stops before cleanup. Corrected analysis validates every requested/persisted MRC5 marker, backing verification and canonical backing ID; 20 read-only controls pass before recovery. Nineteen earlier process-ownership controls cover cancellation handoff, child reaping and inventory refusal.

| Evidence | SHA-256 |
|---|---|
| Original failed suite | `5e44ce71b7597251607eee90d577d287aebd6ea22ddaab1656f527c59eefd535` |
| Failed recovery preflight | `a62e1f0849de1aec86294b00c58bc39525389c2f54a7511741dd15a312602fe1` |
| Qualified recovery audit | `fe5ded8bcb164fcff2996c2dd3c3045614108e148aff3b94345db8bfcd656b0c` |
| Root terminal witness | `24e78b159db5b13c0917358c3d0f0a83e452367f3fd1318f3bc7bcbe6277d58f` |
| Recovered extractor | `5715003529c52b5b6e061d12492aa332078a5526aee05e925d455f0009e0678d` |
| [wire100.json](wire100.json) | `c046d711fb858750c965a33dacd4be14dc5315026eb3f8397d9bfb8c8cf170f1` |
| [wire100-details.md](wire100-details.md) | `2c7875140a95dffe09927fa63e334f7f56ff969cb28d14be16e1f90a5989138c` |
| [query-probes.json](query-probes.json) | `cc328014244e5e64872ea299dcf3b61175fcc441d8cf59d7c1dcf6669e03b9e9` |
| [query-probes.md](query-probes.md) | `506590eafa0c86724a43ff44144f6091db131799525ffb90a0aff848a44c71c5` |

Private endpoints, credentials, scopes and container IDs are omitted. Source/binary/observer/configuration hashes remain in JSON. Go/RPC extraction relies on the pinned observer’s hashed summaries and reset/coverage checks; it does not independently reconstruct all raw snapshots.

The code separately passes 46 local tests, strict all-target TiDB Clippy, formatting and eight actual TiDB controls covering bounded lookups, volume isolation, missing/duplicate selected groups, autocommit, cancellation, same-inode conflicts and independent writes. These checks are independent of throughput.

**Outstanding:** independent production server processes, 10,000 clients / 10,000 Drives / 5,000 Partitions / 1,000 files per Drive, external OIDC, peer-cache paths, full formal qualification, crash/power-loss durability and physical backing IOPS. The [earlier query-layout benchmark](../tidb-indexed-query-optimization-20260929/README.md) retains different revisions and executions; those results are not pooled here.
