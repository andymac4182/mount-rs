# TiDB observations for the clean matched remote10 run

The bounded control completed successfully on clean revision `083c82919bdeac7c7182563bbbdaa0e91810b200`: 10 frontend workers, 10 clients, 10 drives, 5 partitions, and 100 files per drive. Both fresh verification passes covered all 1,000 files. Saved control receipts show zero cleanup errors, successful child reaping, group absence and pipe EOF. The accompanying [performance comparison](comparison.md) remains separate from these server counters.

All eight raw Prom body hashes, capture/whitelist hashes, and both pinned phase receipts reconcile. The frozen observer source is unchanged. Original engine identity and full original identities/limits for TiDB and all three TiKV instances match before and after. Native preflight independently matches all seven original TiDB roles plus RustFS. The saved eight-role current-inspection record has no timestamp; this Prom packet provides post-capture continuity for its four metric roles, with PD/RustFS post-capture identities outside its coverage.

## Recorded counters

| Observation | Result | Meaning |
|---|---:|---|
| Retried-statement observations | 0 new | No recorded pessimistic statement retry observations; no average retry value is available. |
| Client backoff observations | 0 new across all 13 exported finite groups | No recorded backoffer observations in the covered groups; no average duration is available. |
| Query histogram | 155,863 observations / 70.179794 s inclusive sum | Complete selected family coverage; concurrent query durations do not represent elapsed time or filesystem operation counts. |
| Request histogram, matched subset | 72,221 observations / 24.883574 s inclusive sum | Partial coverage because two new raw histogram pairs appeared. |
| TiKV candidate lock-stage counter | Six zero deltas | Callsite coverage remains unverified; pure lock-wait duration remains unavailable. |

These counters do not establish a TiDB transaction-lock contention bottleneck. A successful request can wait without a recorded statement retry or client backoff. The lock-stage counter's zero remains unverified. The capture supports “no recorded retries/backoffs in this window,” with lock-wait coverage still incomplete.

## Covered query observations

| SQL type | Observations | Mean ms | Inclusive sum s |
|---|---:|---:|---:|
| Begin | 34,918 | 0.269885 | 9.423852 |
| Commit | 2,933 | 4.141727 | 12.147684 |
| Insert | 1,545 | 0.559045 | 0.863724 |
| Rollback | 32,047 | 0.054046 | 1.732024 |
| Select | 43,874 | 0.895201 | 39.276050 |
| Set | 35,104 | 0.073712 | 2.587595 |
| Update | 4,317 | 0.696000 | 3.004632 |
| Use | 2 | 0.107667 | 0.000215 |
| general | 368 | 0.178432 | 0.065663 |
| internal | 755 | 1.428284 | 1.078354 |

Delete had no interval observations; its mean is null. Query time, request time and backoff time may overlap. They cannot be added or subtracted as disjoint components of one operation.

## Request coverage limit

The request histogram added four raw selected samples: two complete sum/count pairs. Both are external (`scope=false`) with `stale_read=false`.

| New pair | End cumulative observations | End cumulative seconds |
|---|---:|---:|
| PessimisticLock | 5,232 | 2.725257074 |
| Prewrite | 2,952 | 10.703156180 |

The before snapshot has no corresponding raw series. Their end values stay cumulative and are excluded from interval deltas. Request totals, rates and full-family means remain unavailable. A displayed “no interval observations” group describes matching raw series only and cannot establish absence across these new identities. No resets or missing raw samples were found.

## Window and scope

| Metric source | Before/after interval s |
|---|---:|
| TiDB | 213.040393834 |
| TiKV1 | 213.045075250 |
| TiKV2 | 213.046930125 |
| TiKV3 | 213.048293833 |

Each source uses its own body-completion timestamps; endpoint sampling is sequential. The native terminal's created/updated timestamps lie inside the metric enclosure. The latest before sample precedes native creation by 27.734906 s; native terminal update precedes the first after request by 16.625189 s. These gaps include work outside the native terminal window and server background activity. The whole control receipt reports 171.329363 s.

This is a whole-control enclosure with preflight/startup, work, verification, cleanup and background activity. It supplies no stage-only rates, logical amplification denominator, per-drive attribution, physical NVMe IOPS, or production-capacity qualification.

## Evidence

- Before receipt: `d86078ac51fb368bfdf6d5ccefe9c809b1f3b627e1489f44771bebf8ac3e4f5b`
- After receipt: `094839e64607fdd186265d68377eab3247d99520f7fa681f9454822875c466b2`
- Clean terminal: `cc49389b493b9f6f156c8c5b06123495b4186b52bfcea5869a8c0e72a934f0d6`
- Clean control receipt: `1f4caadaaeebbec78e4264c88e4c58972f9cd27bc9e626a6cd8c8a156021994d`
- [Scalar report](server-counters.json). The root acceptance proposal is retained with the private evidence packet.

Audit was file-only. Original evidence, source, observers, repository and ledger were left unchanged. Root review of the proposed partial acceptance remains separate from this arithmetic audit.
