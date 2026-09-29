# TiDB + RustFS: structural lock benchmark

Measured release commit `6a60518d`, compared with the retained `6fdaabf3` run. Each used ten actual server processes, ten clients, ten Drives, five Partitions and 1,000 varied files per Drive. Rates are **whole-cluster logical cycles/s including Open/Close**, not physical disk IOPS. Mostly-idle mode keeps all ten clients connected with one performing I/O; all-active mode runs ten active clients.

One run per revision, five-second requested cells, unchanged workload order and deadlines. A cycle finishes before the active clock stops, so the measured denominator can exceed five seconds. Cache history and backing-store conditions differ. The rates are descriptive comparisons, not isolated causal estimates or sustained capacity qualification.

## All clients active

| Pattern | Previous | Current | Observed change |
|---|---:|---:|---:|
| Sequential read | 650.7 | 627.9 | -3.5% |
| Random read | 640.6 | 656.0 | +2.4% |
| Sequential overwrite | 197.8 | 227.8 | +15.2% |
| Random overwrite | 185.0 | 235.6 | +27.3% |
| Mixed | 295.3 | 342.7 | +16.1% |
| Hot file | 108.2 | 285.4 | +163.7% |
| Append/truncate | 252.7 | 279.1 | +10.5% |
| Create/rename/unlink churn | 2.1 | 55.0 | +2563.8% |

All-active churn completed 282 cycles in 5.129406916 seconds, versus 14 in 6.783522584 seconds: 26.64 times the observed rate. Each churn cycle acknowledges create/open, close, rename and unlink; it performs no payload write. The change removes redundant sibling dentry locks from structural publication. Ordinary payload read/write paths are unchanged, so their rate differences cannot be attributed directly to this change.

## Mostly idle clients

| Pattern | Previous | Current | Observed change |
|---|---:|---:|---:|
| Sequential read | 135.4 | 140.9 | +4.1% |
| Random read | 147.6 | 156.5 | +6.0% |
| Sequential overwrite | 48.4 | 58.3 | +20.5% |
| Random overwrite | 49.1 | 58.1 | +18.4% |
| Mixed | 71.0 | 81.8 | +15.3% |
| Hot file | 54.3 | 58.7 | +8.2% |
| Append/truncate | 61.7 | 56.5 | -8.5% |
| Create/rename/unlink churn | 5.7 | 12.2 | +112.9% |

The two observed losses are all-active sequential read and mostly-idle append/truncate. More repeated measurements would be needed to distinguish normal variation from a regression.

## Commit amplification control

A separate actual TiDB 8.5.7 metadata-only control performs one acknowledged create, then checks the complete namespace through a fresh context. The same diagnostic source was used before and after the production change.

| Existing files | Previous mutation keys | Current mutation keys |
|---|---:|---:|
| 4 | 16 | 12 |
| 1,000 | 1,012 | 12 |

At 1,000 files the observed commit mutation-key count fell **98.81%**. The previous `FOR UPDATE` read added untouched sibling dentries as lock-only mutations. Cooperative structural mutations already serialize through the volume authority row. Keeping that lock and reading the complete dentry proof without sibling locks removes this amplification while retaining fresh validation.

The metric is `tidb_tikvclient_txn_write_kv_num`, a shared TiDB process histogram of assembled transaction mutation keys, including lock-only keys, before commit acknowledgment. Each accepted window had exactly one general observation, no reset or missing series, successful publication, and a fresh complete namespace oracle. Exact transaction identity is not proven. This measures mutation-key amplification; it is neither physical IOPS nor a controlled latency result. Exact before/after counters and receipt hashes are in `lock-key-control.json`.

## Namespace population and remaining costs

| Counter | Previous | Current |
|---|---:|---:|
| Populate 10,000 file names | 160.104 s | 42.428 s |
| Inode SQL calls | 30,030 | 30,030 |
| Returned inode SQL rows | 10,010,020 | 10,010,020 |
| Metadata SQL reads | 20,080 | 20,080 |
| Data/probe marker GETs, each | 10,010 | 10,010 |
| Pool checkouts | 10,040 | 10,040 |
| Read BEGIN / rollback, each | 20 | 20 |
| Structural publications / COMMITs, each | 10,000 | 10,000 |
| Inclusive COMMIT mean | 45.680 ms | 8.365 ms |
| Inclusive structural wrapper mean | 150.995 ms | 36.809 ms |

Namespace population was 73.50% shorter in this run. SQL calls and returned rows were unchanged: the optimization removes commit mutations, not complete proof reads. Namespace COMMIT spans summed to 83.648 seconds and structural wrapper spans to 368.095 seconds across concurrent calls. These inclusive spans overlap and must not be added as exclusive costs.

| All-active churn counter | Previous | Current |
|---|---:|---:|
| Inode SQL calls/cycle | 19 | 19 |
| Returned inode rows/cycle | 10,044.143 | 10,120.298 |
| COMMITs / structural publications per cycle, each | 3 | 3 |
| COMMIT mean | 704.581 ms | 8.124 ms |
| Structural wrapper mean | 1267.346 ms | 40.238 ms |
| Pool checkout mean | 1.673 us | 1.718 us |
| Local metadata gate wait mean | 0.198 us | 0.213 us |

Churn retained 11 inode DML calls, eight metadata reads, three metadata DML calls and five checkouts per cycle. Selected storage errors/cancellations, CAS conflicts/backoffs and recovery calls were zero. It issued no SDK blob PUTs. The native observations show much smaller publication/COMMIT waits; the separate mutation-key control supports the removed amplification mechanism without proving exact native transaction attribution.

Complete membership/root-dentry range work remains. For F live files and t retained tombstones at cycle start, churn still returns `10F + 5t + 22` inode SQL rows per cycle. Different completed-cycle counts change tombstone history, explaining why a higher row average is possible with the same query shape. Bounded structural proofs remain a separate scaling task.

## CPU, memory and blob path

| Current all-active pattern | Frontend CPU sum, s | Max individual worker RSS endpoint, MiB | SDK PUT mean, ms |
|---|---:|---:|---:|
| Sequential read | 6.196 | 44.453 | — |
| Random read | 6.276 | 44.781 | — |
| Sequential overwrite | 3.497 | 45.141 | 17.937 |
| Random overwrite | 3.582 | 45.516 | 17.440 |
| Mixed | 4.267 | 45.906 | 17.747 |
| Hot file | 3.909 | 46.250 | 17.378 |
| Append/truncate | 3.949 | 46.516 | 17.521 |
| Churn | 4.354 | 46.594 | — |

CPU sums cover eleven separate boundary windows including observers/background, not the active throughput clock. RSS endpoints are not a simultaneous cluster peak. SDK PUT spans include local client, transport and persistence wait. Rust allocator instrumentation was disabled; this run makes no zero-allocation claim. Object-store HTTP observations remain partial, non-atomic process counters rather than exact wire request attribution.

## Correctness and qualification

- All 16 timed cells passed. Independent initial and final byte/EOF/membership oracles checked every file: 10,000 files / 62,832,640 initial bytes and 10,000 files / 62,857,216 final bytes.
- All 100 Drive/server read-and-close pairs passed, along with partition/sibling denial and revocation checks. All ten workers exited successfully without forced termination.
- The new capped RustFS fixture and collector stopped. The original eight containers retained exact identities, limits and lifecycle. Source pins and the clean measured checkout stayed unchanged. Owned datasets remain retained; absence is unproven.
- All 53 opt-in actual TiDB compact controls passed, including corruption, fresh parent proof after transaction start, overlapping unrelated selected-file writes, packet rollback and unknown COMMIT acknowledgment controls. The provider's enabled tests and strict all-target Clippy passed. The serial release harness passed 170 enabled tests, with six opt-in tests ignored.
- The earlier revision's parallel harness resource-counter-reset failure remains unresolved; this serial gate does not establish that the parallel failure was fixed.
- This is macOS, loopback QUIC, a raw SDK RAM-cache configuration and local signed-token authentication. It does not qualify OS mounts, composed peer RAM/disk caching, an external OIDC issuer, crash durability, physical IOPS or the 10,000-client production target.

## Evidence

`metrics.json.gz` preserves the frozen extractor's exact clocks, counters and all 352 selected compressed-frame hash bindings. Its offline-candidate flags are preserved. `witness.json` binds root runtime acceptance separately to the closed owner/terminal receipts, release binary and immutable source archive. `namespace.json` retains all 22 namespace frame bindings and validated deltas. `lock-key-control.json` retains the separate before/after mutation-key control.

The extractor's `source_digest` covers a limited harness seam and is identical in both runs. The complete release binding is the measured revision, distinct binary digest and immutable archive manifest in `witness.json`; the current archive contains 565 source rows.

The observed improvement is large for structural writes. The next metadata scaling limit is the complete range proof and its SQL/decode work; these results do not yet establish 1,000 logical operations/s or production capacity.

## Subsequent 100-client attempt

The current source was also exercised with 100 clients, 100 Drives, 50 Partitions and 100,000 files. That attempt stopped during payload population when sampled host free disk fell below the unchanged 64 GiB reserve. No timed cell or fresh full-corpus oracle completed. [The scale attempt report](scale-100.md) preserves the failure, settlement and partial TiDB/TiKV CPU, network and guest block I/O observations. It establishes no 100-client throughput result.
