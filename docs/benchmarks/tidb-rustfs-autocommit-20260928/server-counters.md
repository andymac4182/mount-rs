# TiDB + RustFS autocommit remote10 server counters

**Independent arithmetic: PASS. Coverage: partial.**

Candidate: `52c1cdeedddd16937088bfa42ee3191c4b328ff8`; ten servers, ten clients, ten Drives, five Partitions, 100 files per Drive. Saved native owner succeeded in 148.935674 seconds. This audit did not execute a workload, Docker request or network call.

## Capture identity and timing

The original observer, ready/config, both receipts, captures, eight raw metric files and after whitelist hashes match. Independent parsing reproduced every selected capture sample and whitelist matched-label delta. There are no parse errors, dropped labels or negative matched deltas.

The original engine and full TiDB/three-TiKV CID, image, labels, start time, restart count, limits, PID and loopback bindings match before/after. Native preflight matches all seven TiDB/PD/TiKV roles plus RustFS. The observer checks only four metric roles before/after; it supplies no post-capture PD/RustFS identity witness.

The TiDB interval is 333.178039 seconds. All four role windows enclose controller creation through final terminal update. The last before-body completion preceded creation by 23.699118 seconds, and the first after request began 163.615905 seconds after the terminal update. Counters therefore include substantial background intervals. Parent reap/EOF have acceptance flags, but no separate clock timestamps in these artifacts.

## Whole-window deltas

Handled SQL: 80,692 observations; 72.221818367 concurrent inclusive seconds. Complete for the selected raw series. These are neither filesystem operations nor CPU seconds.

| SQL type | Observations | Inclusive seconds | Mean ms |
|---|---:|---:|---:|
| Begin | 5775 | 2.024252611 | 0.350520 |
| Commit | 3672 | 12.400959108 | 3.377168 |
| Delete | 0 | 0.000000000 | unavailable |
| Insert | 1598 | 0.921815831 | 0.576856 |
| Rollback | 2115 | 0.143563870 | 0.067879 |
| Select | 54718 | 51.053329262 | 0.933026 |
| Set | 5961 | 0.587911591 | 0.098626 |
| Update | 5321 | 3.006402151 | 0.565007 |
| Use | 2 | 0.000221500 | 0.110750 |
| general | 360 | 0.060794912 | 0.168875 |
| internal | 1170 | 2.022567531 | 1.728690 |



TiKV-client requests: matched subset 101,178 observations and 41.469973512 inclusive seconds. 2 new raw samples form one previously absent histogram pair. Its endpoint cumulative value is retained separately; no interval delta is available. Full-family totals, means and rates are unavailable.

| RPC type | Internal scope | Stale read | Matched observations | Inclusive seconds | Matched mean ms |
|---|---|---|---:|---:|---:|
| BatchGet | false | false | 732 | 0.226856075 | 0.309913 |
| Cop | false | false | 44716 | 15.408871894 | 0.344594 |
| Get | false | false | 45708 | 11.710311072 | 0.256198 |
| MvccGetByKey | false | false | 14 | 0.009938708 | 0.709908 |
| PessimisticLock | false | false | 6183 | 2.963933231 | 0.479368 |
| Prewrite | false | false | 3691 | 10.949939328 | 2.966659 |
| ScanLock | false | false | 6 | 0.009884124 | 1.647354 |
| Commit | true | false | 21 | 0.060929043 | 2.901383 |
| PessimisticLock | true | false | 82 | 0.029747538 | 0.362775 |
| Prewrite | true | false | 25 | 0.099562499 | 3.982500 |


New label groups, endpoint cumulative only:


- `{"scope":"true","stale_read":"false","type":"CheckTxnStatus"}`: 1 cumulative observations, 0.002206875 cumulative seconds; interval delta unavailable.


The covered retry histogram and all 13 covered backoff type groups record zero new observations. Retry is observed only at non-panic Exec returns with a positive retry count; ordinary SQL errors can also return without a panic; neither metric establishes zero lock contention. Six TiKV candidate lock-stage counter deltas are zero, but increment-path coverage remains unverified.

## Acceptance and limits

Propose accepting the independently checked whole-window scalar deltas and four-source identity continuity, preserving partial RPC coverage. Do not claim a per-phase result, logical I/O amplification ratio, pure lock-wait duration, physical NVMe IOPS, production capacity, mounted CLI qualification, or absence of contention. Background attribution cannot be removed from this snapshot pair.

Raw capture, receipt, observer and native-control artifact basenames and SHA-256 bindings are retained in `server-counters.json`. The original evidence remains unchanged. Root review is required before publication.

## Artifact bindings

The JSON preserves all raw metric hashes and independent audit bindings. Counter values are copied directly from the independently reviewed report.

| Artifact | SHA-256 |
|---|---|
| after.capture.json | `58c78aa56ac44d97e4a8945206ad1fc930493e6c6a05c40db05fde2ddcde0fe1` |
| after.receipt.json | `6b759f026a4e5124a3ace6e69c45c368c38d5bdf9ed15146672c3ed8d7c228ed` |
| after.tidb.prom | `d19d51b9f676d4093a64c7dbdb4b0a479bdbb996f4c969cd998da2aa8900e4bb` |
| after.tikv1.prom | `36803abd8606b265f87b6355eb4169412f8dd13be644dc5c913762c7dbe6990e` |
| after.tikv2.prom | `ec87aec2acc365ddec6a16a87768fd20f6357c4f17457bf2a3a599b51b6a9e48` |
| after.tikv3.prom | `a5914eff5c59dea49f9331f91a6fe8519dae0dafb7e19121fee44c28165d1ae3` |
| after.whitelist.json | `9c2646a1ff5c89b27e520a3e57fef61a31d8345d6409545c5552200c1e0465d0` |
| before.capture.json | `ec77b06a730d9bdb3e7a6731fab43e2a80ed548634a6840ecd1a823f0f7c81e6` |
| before.receipt.json | `050df7e94856ea8f94fcbe024d3e415fc90f89060cb2c6e2ef57a0ca23f0738d` |
| before.tidb.prom | `dfa8a177f25b46a807ec6016f425a134f457b94d830089acedadf3756256adef` |
| before.tikv1.prom | `96313e13dd17f09b662ae5fd1f6ad4d848f3c5bab575c5fa9b892d4eb3480727` |
| before.tikv2.prom | `3cfbfdb4c289ac79ee1d2be943b4ffb38397dd6c2848ac3145466d222fe01222` |
| before.tikv3.prom | `ffe917cec21c31f68f8bfe11aeba224a37b2ed1fcee4096846d910ceaad763f0` |

These artifact references record provenance; raw configuration, native receipts and metric bodies are not included in this report.
