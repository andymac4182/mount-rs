# TiDB + RustFS: optimistic create benchmark

Measured release commit `6fdaabf3`, compared descriptively with the retained `78e68035` run. Each run used ten actual server processes, ten clients, ten Drives, five Partitions and 1,000 varied files per Drive. Rates below are **whole-cluster logical cycles/s including Open/Close**, not physical disk IOPS. One run per revision, five-second requested cells; cache history and backing-store conditions differ. These observations do not establish causal throughput gains or regressions.

## All clients active

| Pattern | Earlier | Current |
|---|---:|---:|
| sequential_read | 553.3 | 650.7 |
| random_read | 598.1 | 640.6 |
| sequential_overwrite | 137.4 | 197.8 |
| random_overwrite | 234.3 | 185.0 |
| mixed | 317.7 | 295.3 |
| hot_file | 246.2 | 108.2 |
| append_truncate | 226.3 | 252.7 |
| churn | 13.5 | 2.1 |

Reads and sequential writes completed faster in this sample. Random overwrite, mixed, hot-file and churn completed slower. The create-only optimization does not modify ordinary reads/overwrites, so their changes cannot be attributed to the skipped preflight.

## Metadata traffic and remaining waits

| Namespace population counter | Earlier | Current |
|---|---:|---:|
| Duration | 164.849 s | 160.104 s |
| Inode SQL calls | 60,030 | 30,030 |
| Returned inode SQL rows | 20,020,020 | 10,010,020 |
| Root preflight loads | 10,000 | 0 |
| Metadata SQL reads | 30,080 | 20,080 |
| Data/probe marker GETs, each | 20,010 | 10,010 |
| Structural publications / COMMITs, each | 10,000 | 10,000 |

The expected duplicate-read reduction occurred exactly. Namespace duration fell only 2.88%. Inclusive COMMIT elapsed summed over those 10,000 calls increased from 229.133 s to 456.802 s (22.913 to 45.680 ms/call). The structural wrapper also increased from 1176.822 s to 1509.949 s. These spans overlap and must not be added as exclusive costs.

| All-active churn counter | Earlier | Current |
|---|---:|---:|
| Inode SQL calls/cycle | 22.000 | 19.000 |
| Returned inode rows/cycle | 12061.315 | 10044.143 |
| COMMIT mean ms/call | 50.487 | 704.581 |
| Structural wrapper mean ms/call | 177.814 | 1267.346 |
| Pool checkout mean us/call | 1.999 | 1.673 |
| Local metadata gate wait mean us/call | 0.204 | 0.198 |

Churn retained three publications/COMMITs per cycle. Both runs recorded zero selected storage errors/cancellations and zero CAS/backoff calls. Churn issued no SDK blob PUTs. The observed loss is concentrated in SQL/publication waits; tiny average local gate and pool waits do not support blaming coordinator queueing. The client-side COMMIT span includes transport, scheduling and backing-store wait: it does not identify a TiDB lock, TiKV/Raft stage or physical I/O cause.

Publication still reads complete membership and root dentries. For F live files and t retained tombstones at cycle start, the source row formula changes from `12F + 6t + 24` to `10F + 5t + 22`; full range work remains. Different completed-cycle counts change tombstone history, so row means alone are not a controlled asymptotic comparison.

## CPU, memory and blob path

| Current all-active pattern | Frontend CPU sum, s | Max individual worker RSS endpoint, MiB | SDK PUT mean, ms |
|---|---:|---:|---:|
| sequential_read | 6.331 | 46.891 | — |
| random_read | 6.258 | 46.891 | — |
| sequential_overwrite | 3.025 | 47.281 | 26.762 |
| random_overwrite | 2.972 | 47.672 | 29.744 |
| mixed | 3.758 | 47.984 | 28.437 |
| hot_file | 2.200 | 48.188 | 84.002 |
| append_truncate | 3.601 | 48.484 | 28.170 |
| churn | 0.839 | 48.547 | — |

CPU sums cover eleven separate boundary windows including observers/background, not the active throughput clock. RSS endpoints are not a simultaneous cluster peak. SDK PUT duration includes local client, transport and remote persistence wait. Rust allocator instrumentation was disabled; no zero-allocation claim is made. Churn frontend CPU was 0.839 s while its active clock was 6.784 s, consistent with substantial waiting but not an exclusive stage attribution.

## Qualification and limits

- All 16 cells completed; mostly-idle mode retained ten connected clients with one active client. Exact per-cell counters and clocks for both modes are in `metrics.json.gz`.
- Independent initial and final oracles compared every file byte, EOF and membership: 10,000 files / 62,832,640 initial bytes, and 10,000 files / 62,857,216 final bytes.
- All 100 Drive/server read-and-close pairs passed; authorization/revocation checks passed. All ten workers were cleanly reaped with no forced termination.
- The fresh capped RustFS fixture and collector stopped. Original container identities, resource settings and lifecycle were unchanged; source pins stayed identical. Owned data is retained; dataset absence is unproven.
- This is macOS, the raw SDK RAM-cache configuration, and a local signed-token test. It does not qualify Linux/macOS OS mounts, the composed peer RAM/disk cache, an external OIDC issuer, crash durability, physical IOPS, or the 10,000-client production target.
- The parallel release harness gate had one unresolved resource-sampler `process counter reset` failure (169 passed). Its serial gate passed all 170 enabled cases with six opt-in cases ignored. No production counter check was weakened.

## Reproducible evidence

- Measurement revision: `6fdaabf3cf852e8d6d4853d6a54439884aab9a10`; release binary: `7a8a575a5ecaaaa80726ed7f18f4a04034c3beca98ffdb5e500c2cf808412687`.
- Root owner receipt: `12ae6f4f345a473356e3f64f6af2ae0ef2f1711ef7884dc62248affd5110ec8f`; terminal: `91aeeab11f52f2b3ebbf951ccf058ccddd6232d28251d39236d0d6e4ca788bbb`.
- `witness.json` binds runtime acceptance separately from the frozen extractor, which verifies closed counters only. Its original offline-candidate flags are preserved in the metrics artifact.
- `namespace.json` retains exact namespace counters and all before/after compressed-frame hash references.
- Deterministic `metrics.json.gz`: 134220 bytes, SHA-256 `a1cc7876f7871f01ecfaa97e3bcd7b2168585762ffddc328c95fd45d743ddd0e`.
- Decoded JSON: 5370244 bytes, SHA-256 `6342b6ca23e162ad13fa6996c26b8dd50a1d1d212b69ea3b55408dffd41b7a84`.

The next performance investigation is the TiDB COMMIT/publication stall under structural workloads. A larger population run needs that result before treating the metadata read reduction as a scaling win.
