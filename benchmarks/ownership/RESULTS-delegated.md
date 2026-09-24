# Measured directory delegation overhead

These six SQLite cases are from the same final [raw primary run](results-macos-arm64.json) and share its source/host provenance. Seven trials follow one warmup per case, with 128 operations per trial; the disjoint workload performs 128 writes per client, 256 total. Both protocols publish immediately. Each coordinator has independent SQLite provider connections, and final verification uses a fresh provider pair. No other task builds/tests or qualification VM were active, but the development host remained unisolated.

| Workload | Protocol | Total p50 / p95 ms | Operation p50 / p95 ms | Publish attempts | Successful publications | Block puts | Block flushes |
|---|---|---:|---:|---:|---:|---:|---:|
| disjoint_clients | MRC2_CAS | 297.233 / 307.508 | — | 256..281 | 256 | 256..281 | 272..297 |
| disjoint_clients | MRC3_delegated | 436.936 / 472.154 | — | 263..284 | 256 | 276..336 | 292..352 |
| handoff | MRC2_CAS | 233.064 / 325.198 | 0.635 / 0.809 | 128 | 128 | 128 | 256 |
| handoff | MRC3_delegated | 597.557 / 628.826 | 2.989 / 3.671 | 128 | 128 | 128 | 256 |
| same_directory_admission | MRC2_CAS | 7.580 / 8.132 | 0.057 / 0.069 | 0 | 0 | 0 | 0 |
| same_directory_admission | MRC3_delegated | 47.387 / 51.649 | 0.360 / 0.460 | 0 | 0 | 0 | 0 |

Ranges span the seven trials. Total-duration p95/p99 are the observed maximum with seven samples; per-operation distributions aggregate all 896 measured operations for handoff/admission. Publication/rebase retries can produce extra immutable blocks; these are counted, not hidden. A revision check may reject a prepared write before the publication call, so block puts can exceed publication attempts. Successful publication count is asserted to equal all 256 writes in the disjoint case. Full contents of both 256 KiB files are compared after shutdown and fresh provider reopen in every trial.

Disjoint clients run on separate barrier-started OS threads and sync after every 16 writes. Handoff operation latency measures delegated checkin plus checkout, then receiving open/stat/full read/close; legacy MRC2 instead measures sender sync plus receiving open/stat/full read/close, because CAS provides no exclusive ownership handoff. Total handoff time also includes preceding writes and closes. Same-directory admission measures MRC3 overlapping-checkout denial versus legacy accepted open/stat/full read/close. These scenarios have different authority guarantees and their timings are not safety-equivalent. Every delegated overlap attempt must be denied.

MRC3 overhead includes provider authority checks and complete namespace-delta validation. These local measurements make no network claim and do not qualify native mount cache revocation or live NFS/FSKit handoff. See the [harness guide](README.md) for reproduction and exact timing boundaries. The [preliminary run](results-delegation-preliminary-macos-arm64.json) is retained separately because other activity may have been present.

For disjoint clients, conditional metadata loads were 546–571 in MRC2 and
842–962 in MRC3; MRC3 also performed 842–962 delegation-state reads. A
128-operation delegated handoff trial performed 128 checkins, 128 checkouts,
128 unconditional metadata loads, 1536 conditional loads and 1664 delegation
reads. Same-directory MRC3 admission attempted 128 denied checkouts; legacy
accepted all 128 accesses. These are provider API counts.
