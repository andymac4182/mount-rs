# W26 progress ledger — Apache Ozone S3 backend

This ledger is the working record for the W26 Apache Ozone S3 backend
workstream. It distinguishes repository implementation, local evidence, and
hosted/native/provider acceptance. Estimates are provisional and are intended
for engineering planning, not a commitment.

## Current authority override — 2026-09-22, latest published chunk

This block supersedes earlier current-status blocks below. Historical rows are
retained for auditability, but the following is the status other worktrees must
use as their build-on and production-readiness boundary.

| Field | Current value |
| --- | --- |
| Shared implementation tip | `origin/main` = `1e7e75716349f1eff09fd9b77bdeb652c8d0c1b2`; this published tip includes the SQLite autocommit fenced-CAS publication chunk, the earlier PGlite/TiDB publication optimization, TiDB session setup/isolation optimization, FoundationDB lockfile correction, bounded mutation window, R2 upload coalescing/cache and the W26 evidence-packet controls. |
| Current implementation chunk | `1e7e7571` (`perf(w26): use sqlite autocommit publication fast path`). The successful SQLite publication is now one parameterized fenced conditional UPDATE under SQLite autocommit; only a zero-row result opens the `Immediate` classification transaction. The lease/revision/fail-closed semantics are unchanged. |
| Local verification | `./scripts/cargo-shared test --workspace --all-targets --locked` passed; `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings` passed; `cargo fmt --all -- --check` and `git diff --check` passed. The focused SQLite package has 11/11 tests; the complete workspace run remains the strongest local regression evidence. |
| Security evidence | Diff scan `1b6c1c72-1e6c-4d85-a008-5a8fced9e7c6` completed and sealed with complete changed-file coverage, three reviewed surfaces and zero reportable findings. Measured scan usage: 1,336,729 total tokens, 1,331,116 input, 1,268,480 cached input. Hosted/customer TLS, IAM, tenancy, provider-native allocation, 99.99% availability, five-minute RPO/RTO, backup/DR, native and release controls remain explicit external gates. |
| Current hosted packet | Run `35688061634` selected exact SHA `06fc70612b9387a281ab050f711fc878713177ea`, which predates `1e7e7571` and therefore does not test the new SQLite autocommit fast path. All W26 jobs are terminal: base Ozone `106619031921` passed; compositions `106619031684` failed; TiDB `106619031746` failed; FoundationDB `106619031804` failed; aggregate `106621589545` failed closed. |
| Current production decision | **NO-GO**. The current packet is complete diagnostic evidence, not acceptance: the hard 1,000-IOPS-per-drive target remains unmet, and the next packet must run on a fresh exact SHA after this published chunk. No customer deployment, backup/DR, or release claim is made. |

### Current hosted packet — run `35688061634`

All four provider artifacts are terminal and retained. Each provider completed
400 writes, 400 full-byte reads/verifications and 400 deletes: 1,200/1,200
successful lifecycle operations, zero timeouts and zero cleanup failures. The
rows below are diagnostic because the hard threshold is per provider and the
aggregate correctly failed closed when the composition pass marker was absent.

| Work item / producer | Status and evidence | Completion | Remaining action | Provisional engineering estimate | External blocker / gate |
| --- | --- | ---: | --- | ---: | --- |
| W26.1–W26.2 / base Ozone `106619031921` | **PASS**: Ozone readiness, gateway policy, immutable block contract, fault window, restart/reopen and cleanup markers passed on the run SHA | 100% | Preserve the exact base artifact and markers in the next one-revision packet | 0–1 h review | Hosted runner, pinned Ozone image and customer topology |
| W26.3a / compositions `106619031684` | **FAIL / diagnostic**: SQLite/R2 `940.817629` IOPS, elapsed `1,275.486304 ms`, p95 write/read/delete `187.646450/75.215778/56.631260 ms`; PGlite/R2 `1,004.326920` IOPS, elapsed `1,194.830066 ms`, p95 `221.108790/71.009336/32.646368 ms`; SQLite missed the target, PGlite met it; both rows completed 1,200/1,200 with zero timeout/cleanup failures | 100% functional / 55% performance qualification | Rerun the composition matrix on a SHA containing `1e7e7571`; require SQLite >=1,000 and the composition/aggregate markers | 1.5–4 d per remediation/review cycle | Ozone capacity, runner variability and PGlite/R2 latency are external inputs; SQLite publication cost remains W26-owned |
| W26.3d / TiDB `106619031746` | **FAIL / diagnostic**: TiDB/R2 `332.247378` IOPS, elapsed `3,611.766651 ms`, p95 write/read/delete `642.112097/150.210998/33.677281 ms`; 1,200/1,200 lifecycle operations, zero timeout/cleanup failures; direct Ozone/TiDB bounded/reopen markers passed before the hard benchmark failure | 100% functional / 44% performance qualification | Requalify after the SQLite chunk and compare TiDB's remaining metadata/remote latency; preserve durable restart, fencing and ambiguous-commit markers | 1–3 d hosted review plus further provider work if still below target | Hosted TiDB/PD/TiKV, Ozone topology and provider-native latency |
| W26.3c / FoundationDB `106619031804` | **FAIL / diagnostic**: FoundationDB/R2 `399.594055` IOPS, elapsed `3,003.047681 ms`, p95 write/read/delete `477.856604/42.458388/68.789782 ms`; 1,200/1,200 lifecycle operations, zero timeout/cleanup failures; bounded-listing, composition and reopen markers passed before the hard benchmark failure | 100% functional / 44% performance qualification | Requalify on the next exact SHA; preserve the `--locked`, durable restart and cleanup evidence; do not infer production capacity from this pass | 0.5–1.5 d hosted review/startup | Hosted FoundationDB image/client, Ozone topology and customer durability |
| W26.4–W26.14 / evidence controls | **IMPLEMENTED / locally green**: strict provider selection, exact profile, artifact integrity/retention, no-skip, marker and one-revision aggregation controls remain in force; aggregate `106621589545` failed closed on missing composition `OZONE_IOPS_PASS` | 100% implementation / hosted packet open | Keep fail-closed verification and require all producer plus aggregate markers on one exact current SHA | 0.5–1.5 d review | CI scheduling, artifact service and hosted provider fixtures |
| W26.15 / P8 per-drive target | **OPEN / NO-GO**: the packet is terminal but not qualifying because SQLite, TiDB and FoundationDB missed 1,000; PGlite was just above it. No row may be converted to skip or averaged across providers | 95% W26-owned implementation / 44% hosted qualification | Run a fresh matrix against `1e7e7571` or its later exact published descendant; close every provider and aggregate marker without lowering the target | 1.5–4 d per remediation cycle plus external queue | Ozone fixture capacity/topology, provider latency, hosted runners and artifact retention |
| P14 integration-readiness review | **NO-GO** | 40% | Re-audit a terminal all-provider, end-to-end and aggregate pass, then issue an explicit readiness decision; no deployment or release claim | 1–2 d after W26.15 | Customer secure Ozone topology, 99.99% evidence, five-minute RPO/RTO, DR and release stream |

The current run's aggregate log recorded
`W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS`.
That is the expected fail-closed outcome for a hard provider miss. The next
hosted matrix must select the newly published `origin/main` exact SHA (or a
later exact descendant after reconciliation), and must requalify all four
configured metadata-provider paths end to end. CI is the available environment;
customers own Ozone deployment, backup/DR and operations, and another stream
owns releases.

## Current authority override — 2026-09-22

This block is the current reconciliation point for readers arriving from other
worktrees. Older rows below remain as an audit trail, but they must not be used
as current status when they name an earlier tip or hosted run.

| Field | Current value |
| --- | --- |
| Shared implementation tip | `origin/main` = `0ca59c85a36227a07730f0282194ef7af8650fbf`; this contains the TiDB isolation chunk `0f95cb7d`, SQLite publication CAS `ba4e89d0`, the FoundationDB lockfile correction `8428a5ef`, and the earlier W26 performance, safety and evidence-packet work. |
| Current local verification | `./scripts/cargo-shared test --workspace --all-targets --locked` passed; strict workspace Clippy with `-D warnings`, `cargo fmt --all -- --check` and `git diff --check` passed. TiDB focused tests are 7/7, including the effective-isolation fail-closed regression. |
| Current hosted packet | Manual CI run `35686340751`, exact tested SHA `1e64bc250b5e863620f069aad173945dc474c5b4`; base Ozone `106613849779` passed, composition `106613849813` failed hard IOPS, TiDB `106613849794` failed hard IOPS, FoundationDB `106613849672` failed before its benchmark on the standalone test lockfile, and aggregate `106616062969` failed closed. |
| Current production decision | **NO-GO**. The run did not contain SQLite `ba4e89d0` or FoundationDB lockfile `8428a5ef`, so it is diagnostic and cannot qualify the current shared tip. The hard 1,000-IOPS target, Tier 1 99.99% objective and five-minute RPO/RTO boundary are unchanged. |
| Current security evidence | TiDB isolation scan `5975446d-8bb7-457e-92d3-74c5c6ccf671`, SQLite CAS scan `ea882470-0ef4-4f7a-8d91-03c8d85d7fb7` and prior W26 scans completed with full changed-file coverage and zero reportable findings. Hosted/customer TLS, IAM, tenancy, provider-native allocation and recovery evidence remain external gates. |

### Current hosted packet — run `35686340751`

The producer jobs were terminal, and the aggregate was terminally failed. The
packet is retained as diagnostic evidence only; no failed or incomplete row is
promoted to acceptance.

| Work item / producer | Status and evidence | Completion | Remaining action | Provisional estimate | External blocker / gate |
| --- | --- | ---: | --- | ---: | --- |
| W26.1–W26.2 / base Ozone `106613849779` | **PASS**: Ozone health/readiness, block contract, fault window, gateway failure, restart/reopen and cleanup markers passed on the run SHA | 100% | Preserve the exact marker/artifact in the next one-revision packet | 0–1 h review | Hosted runner, image and customer Ozone topology |
| W26.3a / compositions `106613849813` | **FAIL / diagnostic**: SQLite/R2 `997.056981` IOPS, elapsed `1,203.542047 ms`, p95 write/read/delete `195.384716/22.844125/12.675054 ms`; PGlite/R2 `592.425705` IOPS, elapsed `2,025.570447 ms`, p95 `263.350230/128.917052/55.045747 ms`; both completed 1,200/1,200 lifecycle operations with zero timeouts and cleanup failures | 100% functional / 43% performance qualification | Re-run after `ba4e89d0` on the current published SHA; keep the hard target and aggregate pass-marker requirement | 1.5–4 d per remediation/review cycle | Ozone capacity, runner variability and PGlite/R2 latency are external inputs; publication cost remains W26-owned |
| W26.3d / TiDB `106613849794` | **FAIL / diagnostic**: TiDB/R2 `277.585842` IOPS, elapsed `4,322.987050 ms`, p95 write/read/delete `755.523941/182.752699/41.393459 ms`; 1,200/1,200 lifecycle operations, zero timeouts and cleanup failures; TiDB Rust/N-API bounded seed/reopen and Ozone recovery markers passed before the benchmark | 100% functional / 43% performance qualification | Re-run after `0f95cb7d` and compare session-setup latency on the current SHA; preserve restart/fencing/ambiguous-commit markers | 1–3 d hosted review plus further provider work if still below target | Hosted TiDB/PD/TiKV, Ozone topology and provider-native latency |
| W26.3c / FoundationDB `106613849672` | **FAIL / diagnostic preflight**: Ozone and FoundationDB readiness, image, authority heartbeat, block reachability, fault/restart/reopen and cleanup markers passed, but `tests/foundationdb/Cargo.lock` rejected `--locked`; no benchmark JSON or IOPS row was produced | 90% functional / 0% current benchmark evidence | Re-run on current `0ca59c85`, which includes `8428a5ef`; do not infer throughput from this packet | 0.5–1.5 d hosted review/startup | Hosted FoundationDB runtime and exact lockfile/provider image are external |
| W26.14 / aggregate `106616062969` | **FAIL / fail closed**: all producer artifacts downloaded, but the packet lacked the required `OZONE_IOPS_PASS` marker after hard provider failure/preflight failure | 100% implementation / 50% hosted qualification | Require every provider, full end-to-end marker set and aggregate pass on one exact current SHA | 0.75–1.5 d review | CI artifact service and producer scheduling |
| W26.15 / P8 per-drive target | **OPEN / NO-GO**: current diagnostic packet did not qualify; SQLite was close at 997.06 but remains below 1,000 and the other provider rows were lower or absent | 95% W26-owned implementation / 43% hosted qualification | Dispatch a fresh non-canceling matrix against `0ca59c85`; continue only correctness-preserving optimization and keep the target unchanged | 1.5–4 d per cycle plus external queue | Ozone fixture capacity/topology, provider latency, hosted runners and artifact retention |
| P14 integration-readiness review | **NO-GO** | 40% | Re-audit a terminal all-provider, end-to-end and aggregate pass, then hand off an explicit decision; no release/deployment claim | 1–2 d after W26.15 | Customer secure Ozone topology, 99.99% evidence, RPO/RTO, DR and release stream |

The next hosted matrix must select the exact current published SHA
`0ca59c85a36227a07730f0282194ef7af8650fbf`. The 356863 packet selected an
earlier SHA and therefore did not test the SQLite CAS optimization or the
FoundationDB lockfile correction. CI-only qualification is the available
environment; customer deployment, backup/DR and release execution remain out
of W26 ownership.

## Snapshot

| Field | Current value |
| --- | --- |
| Workstream | W26 — Apache Ozone S3 backend |
| Ledger snapshot | 2026-09-22, Australia/Brisbane |
| Repository | `mount-rs` |
| Snapshot base | `83e0d3b7` (`origin/main`, including W26 implementation commits `c7f0e6d0`, `183660a4`, `d05548e8`, SQL publication fast path `214b9a6b` and TiDB session setup optimization `e41bed05`, the published R2 content-addressing/cache, metadata mutation batching, queue-cancellation hardening, concurrent whole-file-create inode rebasing, corrected Ozone content-addressed block-contract test, deduplicated content-addressed cleanup, the Ozone test lockfile fix `0cef5d44`, the FoundationDB test lockfile fix `8428a5ef` and the latest concurrent-mainline reconciliation; the exact W26 commits and concurrent-mainline merges are recorded below) |
| Checklist completion | 11 of 12 W26 tracker rows checked: shipped implementation rows are complete, while W26.15 (the terminal 1,000-IOPS remediation/qualification gate) remains open; hosted/provider production gates remain open |
| Provisional execution completion | W26 implementation scope: 100% for the 11 shipped tracker rows; current terminal qualification packet: **FAILED / NO-GO** because W26.15's hard performance gate is open; production-rollout readiness: 60% (scope, benchmark matrix, lifecycle safety, bounded remote enumeration, provider-bounded KV/N-API contract, durable-provider bounded-listing tests, strict provider-specific hard-threshold IOPS wiring, fail-closed artifact/profile/metric verification and retention, all-provider credential-free production-config policy, one-revision CI evidence-packet aggregation, complete end-to-end surface marker enforcement, the credential-free customer rollout contract, content-addressed R2 caching, single-flight R2 upload coalescing, metadata mutation batching, concurrent-create inode rebasing, queue cancellation safety, lease-renewal caching with forced validation at explicit durability/destructive boundaries, the provider publication-barrier capability, bounded mutation collection and the PGlite/TiDB conditional metadata-publication fast path are implemented and locally tested; no terminal production gates yet). Customer deployment, native, provider-durability, capacity/SLO, backup/DR and release-stream gates remain separately bounded |
| Current acceptance state | The retained W26 packet on `9c098e5` remains the last accepted hosted result within its documented provider/platform boundaries. The published W26 code now includes optimistic read/write paths, atomic whole-file `FsDriver::write_file`, lazy atime/EOF handling, content-addressed R2 blocks with a bounded process-local cache, single-flight coalescing for concurrent identical block uploads with cancellation-safe follower release, fenced metadata mutation batching, a bounded pending queue, cancellation-safe runner cleanup, same-revision rebasing for concurrent new-file creates, cached provider lease renewal with forced validation at explicit metadata/destructive boundaries, the opt-in provider publication-barrier capability that skips only a redundant post-publish probe while retaining explicit `syncfs` flushes, an eight-round bounded cooperative mutation collection window, the PGlite/TiDB conditional fenced CAS publication fast path (`214b9a6b`) with locked failure classification, and TiDB session setup/verification once per private pool connection (`e41bed05`) with reset-round-trip avoidance. Local full locked-workspace tests and strict Clippy pass; the focused R2 suite has 19 unit tests plus 2 HTTP interop tests, and the focused ChunkedFs suite has 21 passing tests in the current tree. The latest terminal W26 packet is diagnostic run `35683158821` on exact SHA `14dbf2c6`: SQLite/R2 measured 754.59, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17 IOPS; all four provider rows completed their 1,200-operation lifecycle with zero timeout/cleanup failures, and aggregate `106606577835` failed closed on missing `OZONE_IOPS_PASS`. Security scans `5a8fcd70-71d4-461b-bb2a-dec8461c22bc`, `c4fc0012-b0e2-421c-9db8-ca3edfce730c` plus prior W26 scans are complete with zero reportable findings within their local scopes; hosted/provider/customer controls remain explicit external boundaries. No Live Cloudflare R2 or Live AWS S3 result is recorded. |
| Latest hosted workflow | Run `35683158821` is terminal for the W26 packet on exact SHA `14dbf2c61b5d86606b165692e0ba1e0e7af545dc`; base Ozone job `106604292731` passed, compositions `106604292790`, TiDB `106604293431`, FoundationDB `106604292803` and aggregate `106606577835` failed closed. All four provider rows completed their lifecycle with zero timeout/cleanup failures but missed 1,000 IOPS. This packet is diagnostic only; exact metrics, marker evidence and retained artifacts are recorded below. |
| Last terminal hosted W26 packet | Run `35683158821` on exact revision `14dbf2c61b5d86606b165692e0ba1e0e7af545dc`: base Ozone passed; SQLite/R2 measured 754.59 IOPS, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17. Every row completed 400 writes, 400 reads, 400 verified reads and 400 deletes (1,200/1,200 operations), with zero timeouts, zero cleanup failures and success rate 1. The producer rows failed solely `IOPS_TARGET_NOT_MET`; aggregate `106606577835` failed closed on missing hard pass markers. This is a diagnostic failure, not acceptance evidence. |
| Next hosted qualification | Continue W26-owned performance remediation, publish the next safe chunk, then dispatch another non-canceling full matrix on its exact pushed SHA | Accept W26.15 only if every configured provider, FoundationDB preflight, the complete end-to-end packet and the aggregate emit terminal pass markers on one exact revision. The strict 1,000-IOPS target is unchanged. | GitHub-hosted runners, Ozone/provider startup, provider latency and artifact retention are external gates; queued, in-progress, canceled, failed or skipped jobs are not evidence |
| Local Docker boundary | Docker Desktop capacity was about 5 CPUs and 8.2 GiB; this is sufficient for the durable FoundationDB proof but below the TiDB harness's 10 GiB durable-topology minimum |
| Production rollout track | Open, currently **NO-GO**; 0 of 15 production gates are terminally accepted. The 60% figure reflects scope decisions, local hardening, provider-boundary implementation, strict provider-configuration and artifact-integrity/metric/retention qualification, expanded credential-free security policy, one-revision evidence-packet aggregation, complete end-to-end packet surface enforcement, the credential-free customer rollout contract, content-addressed block caching, single-flight upload coalescing, metadata batching, cancellation safety, lease-renewal optimization, publication-barrier capability, bounded mutation collection and the SQL publication fast path, not deployable readiness |
| W26 production target | Customer-deployed Ozone integration; W26 owns provider/client correctness and CI qualification, not customer deployment, backup/DR or release promotion |
| Required service envelope | Target 1,000 IOPS per drive; Tier 1 99.99% reliability; 5-minute RPO and 5-minute RTO. RPO/RTO and availability remain dependent on the customer's Ozone topology and operations |
| Available qualification environment | CI only; no staging environment is available. Production-like evidence must therefore be achieved through controlled hosted CI/provider fixtures and clearly labeled customer-owned prerequisites |
| Release/acceptance decision | W26 implementation and local qualification controls remain accepted within their documented scope; production rollout remains **NO-GO** because terminal run `35683158821` failed every hard 1,000-IOPS provider row: SQLite/R2 754.59, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17. Aggregate `106606577835` failed closed on missing hard pass markers. No failed, queued or partial packet is promoted, and no broader native, customer secure-runtime, Ozone backup/DR or release claim is made |

### Current publication override — exact shared tip `83e0d3b7`

The TiDB session-setup optimization was implemented in `e41bed05`, reconciled
with concurrent mainline changes, and published in merge tip
`9293b1f630ff3cba34f4eeb679b6e327429b4f23`, and the subsequent unrelated
mainline reconciliation is now shared at `83e0d3b7e11cf272d71c204fada5ea827d4a9f29`.
It configures and verifies
`tidb_txn_mode='pessimistic'` once for every newly created private pool
connection, disables redundant `COM_RESET_CONNECTION` round trips, and keeps
the `RepeatableRead` transaction guard, fail-closed effective-mode check,
parameterized SQL, rollback handling and ambiguous-commit semantics.

Local evidence for this chunk: focused TiDB check, 6 unit tests, strict
provider Clippy, full locked workspace tests, strict workspace Clippy,
formatting and diff checks all passed. The complete security diff scan
`c4fc0012-b0e2-421c-9db8-ca3edfce730c` covered the changed provider file and
reported zero findings with no deferred candidates. This is local
implementation/security evidence only; the next hosted W26 packet must use
the exact published revision `83e0d3b7` or a later exact pushed revision.
Production remains **NO-GO** until every configured provider reaches the hard
1,000-IOPS marker and the complete one-revision Ozone evidence packet passes.

| Current work item | Status / completion | Evidence | Remaining action | Provisional engineering estimate | External blocker / gate |
| --- | --- | --- | --- | ---: | --- |
| TiDB session setup and publication-path optimization | Implemented and locally tested / 100% implementation | `e41bed05`; focused and workspace locked tests, strict Clippy, formatting/diff checks; security scan `c4fc0012-b0e2-421c-9db8-ca3edfce730c` zero findings | Re-run TiDB/R2 and full W26 Ozone matrix on the exact published revision; compare lifecycle IOPS and p95/p99 without weakening the hard threshold | 0.5–1.5 d implementation and hosted review | Hosted TiDB/Ozone startup, runner capacity, provider latency and artifact retention |
| W26.15 / P8 per-drive 1,000-IOPS gate | Open / 40% hosted qualification | Terminal run `35683158821`: SQLite/R2 754.59, PGlite/R2 817.10, TiDB/R2 143.04, FoundationDB/R2 345.17; all 1,200/1,200 lifecycle operations succeeded but every hard performance row failed | Close every configured provider row and aggregate marker on one exact revision; retain metrics, logs and digests | 1.5–4 d per remediation cycle plus external queue | Hosted runners, Ozone fixture capacity/topology, provider quotas and CI artifact service |
| P14 integration-readiness review | NO-GO / 39% | Aggregate `106606577835` failed closed on missing `OZONE_IOPS_PASS`; customer security, 99.99%/RPO/RTO, native, DR and release boundaries remain explicit | Re-audit a terminal passing packet and hand off an explicit readiness/NO-GO decision; no release claim | 1–2 d after W26.15 | Customer Ozone secure topology, capacity/SLO, DR and release-stream confirmation |

## Terminal hosted Ozone qualification diagnosis

Run `35635486040` was dispatched against `cfe7e29e001dc01f2fa430a54bc8981b44db0b05` and reached a terminal failure. It is retained as diagnostic evidence only because a failed producer or aggregate job cannot close W26. The provider rows used the fixed production profile of 4 KiB payloads, 400 iterations, concurrency 64 and a hard target of 1,000 lifecycle IOPS:

| Provider row | Result | Lifecycle evidence | Latency summary | Terminal outcome |
| --- | ---: | --- | --- | --- |
| SQLite/R2 | 61.97 IOPS; target not met | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 successful operations; elapsed 19,363.20 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 1,746.47/2,614.53/2,640.25 ms; read 884.14/1,334.54/1,335.08 ms; delete 98.34/159.11/170.22 ms | `ozone-compositions` failed; artifact `w26-ozone-compositions-evidence`, ID `10657030963`, digest `06b89ae0af448fea5659a0c998ee83f94409d5c6118f77ea4aeed91a2611d9c2`; diagnostic only |
| PGlite/R2 | 63.56 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 18,878.87 ms; timeouts 0; cleanup failures 0 | The producer log reports the same successful lifecycle profile and hard-target failure; no pass marker was emitted | `ozone-compositions` failed; same diagnostic artifact, not acceptance |
| TiDB/R2 | 14.14 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 84,869.40 ms; timeouts 0; cleanup failures 0; TiDB Rust and N-API bounded seed/reopen markers passed before the benchmark | write median/p95/p99 6,642.25/7,169.08/7,235.93 ms; read 5,779.92/6,446.97/6,459.42 ms; delete 925.49/1,327.45/1,373.15 ms | `ozone-tidb` failed; artifact `w26-ozone-tidb-evidence`, ID `10656158672`, digest `e4b269067f59464e4eb0731d4d87c88b3124f194ffbbff1a6dcfbcaf6e5b894c`; diagnostic only |
| FoundationDB/R2 | 23.07 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 52,012.17 ms; timeouts 0; cleanup failures 0; FoundationDB Rust and N-API bounded seed/reopen markers passed before the benchmark | write median/p95/p99 4,037.70/6,157.16/6,223.79 ms; read 2,762.10/4,366.18/4,378.44 ms; delete 656.87/996.64/1,054.79 ms | `ozone-foundationdb` failed; artifact ID `10656887088`, digest `33ba0dea3fb6731b9a330dfc60e515b0234db8f9aa564d4ee15f6f13e810ce92`; diagnostic only |

All provider jobs completed the requested lifecycle calls successfully, so both
packets are performance qualification failures rather than lifecycle-correctness
or cleanup failures. The first remediation moved immutable block reads and
writes outside the volume-wide metadata gate, but the replacement metrics show
that whole-namespace publication and provider latency still dominate. W26 must
either implement and verify a further safe metadata-publication/concurrency
design that preserves fencing, revision CAS, immutable-block ordering and POSIX
semantics, or obtain an Ozone/customer capacity qualification showing that the
CI fixture is not representative. Until then, the 1,000-IOPS gate is open and
production readiness remains **NO-GO**.

The first W26.15 remediation chunk is now published. `ChunkedFs::write_at` overlaps immutable block rewrite/flush work outside the metadata gate, then reacquires the gate and commits only when the inode, file size and immutable layout still match the captured base; conflicts fall back to the serialized path. `ChunkedFs::read_at` applies the same lifecycle/read-overlap pattern, revision/base check and safe orphan-atime handling. A lifecycle read/write barrier ensures shutdown waits for these optimistic operations before fencing and releasing the writer lease. Local evidence for implementation commit `c4378dc1` is green: 8 concurrency tests including blocked-read gate release, shutdown ordering and stale-read-versus-concurrent-write preservation, 14 chunked unit tests, full locked workspace tests and strict workspace Clippy. Replacement hosted run `35641941218` on exact tested code head `88b707ba` is terminal for W26 and failed the hard performance check, so it is diagnostic rather than acceptance evidence.

Replacement run `35641941218` is now terminal for the W26 packet even though the
parent GitHub workflow remains active for unrelated native jobs. Its exact
provider artifacts show the first overlap remediation improved throughput but
did not approach the hard target:

| Provider row | Result | Lifecycle evidence | Latency summary | Terminal outcome |
| --- | ---: | --- | --- | --- |
| SQLite/R2 | 85.70 IOPS; target not met | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 successful operations; elapsed 14,001.97 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 893.06/1,231.06/1,386.00 ms; read 1,129.42/1,315.05/1,338.59 ms; delete 173.13/231.02/242.65 ms | `ozone-compositions` `106473112920` failed; `IOPS_TARGET_NOT_MET`; no `OZONE_IOPS_PASS` |
| PGlite/R2 | 94.04 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 12,760.37 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 833.41/1,081.65/1,111.56 ms; read 1,019.53/1,149.45/1,193.28 ms; delete 171.50/245.48/256.42 ms | `ozone-compositions` `106473112920` failed; `IOPS_TARGET_NOT_MET`; no `OZONE_IOPS_PASS` |
| TiDB/R2 | 14.50 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 82,760.98 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 5,335.45/5,714.35/5,762.14 ms; read 6,630.92/6,927.71/6,948.16 ms; delete 1,193.20/1,582.47/1,646.66 ms | `ozone-tidb` `106473112915` failed; `IOPS_TARGET_NOT_MET`; no provider acceptance |
| FoundationDB/R2 | 30.46 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 39,401.30 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 2,174.91/3,474.97/3,589.89 ms; read 3,101.10/5,137.20/5,213.61 ms; delete 474.90/936.62/1,178.95 ms | `ozone-foundationdb` `106473112409` failed; `IOPS_TARGET_NOT_MET`; no provider acceptance |

The composition and provider logs show owned Ozone cleanup completed; the
failure is the deliberate hard-performance/aggregate fail-closed path, not a
timeout or cleanup regression. The aggregate `106477164582` failed because
the provider logs did not contain the required pass markers. This evidence
keeps W26.15 open: the next design chunk must address metadata publication and
provider latency rather than reduce the target or convert failed rows to
skips.

### Published atomic whole-file write remediation

The next W26-owned performance chunk is now implemented and published. The
generic `FsDriver::write_file` contract preserves the existing open/write/close
fallback for drivers without a specialization. `ChunkedFs` specializes it with
an optimistic whole-file path: it resolves the final target, stages immutable
block bytes without holding the volume-wide metadata gate, flushes the block
store, then reacquires the lease/fence and publishes the complete namespace in
one revision-checked operation. A concurrent namespace change falls back to
the existing serialized open/write/close behavior, and abandoned immutable
blocks remain subject to the existing reconciliation grace policy. The N-API
`Filesystem.writeFile` surface now uses this public driver contract, so the
benchmark's public write path exercises the optimization rather than bypassing
it.

Evidence for implementation commit `96a25f17` is local only until the next
hosted packet: `./scripts/cargo-shared test --workspace --all-targets --locked`
and `./scripts/cargo-shared clippy --workspace --all-targets --locked --
-D warnings` both exited 0; the focused ChunkedFs suite passed 15/15,
including the one-revision creation/reopen/byte-integrity regression; the
N-API crate check and storage benchmark unit tests passed; formatting and
`git diff --check` passed. Security diff scan
`11fa7e54-bb66-488d-a369-c1a7ac46da81` completed with zero reportable findings
across `src/driver.rs`, `integrations/mount-rs-napi/src/lib.rs` and
`integrations/mount-rs-chunked/src/lib.rs`. Its coverage is partial by design:
hosted/provider TLS/IAM, customer Ozone security, capacity, SLO/RPO/RTO,
backup/DR and release gates remain external. The implementation was reconciled
with concurrent mainline work and pushed to `origin/main` at `92f900be`.

This chunk does not claim that the 1,000-IOPS target is met. A new manual CI
qualification on the published revision is required, and only a terminal
all-provider packet with every pass marker can update W26.15.

### Current-tip hosted packet `35649202405`

The atomic-write revision was qualified through the complete W26 producer and
aggregate path on hosted Linux. This is the current terminal diagnostic packet,
not an acceptance result: the base gateway job passed, while every configured
provider row missed the hard threshold and the aggregate failed closed.

| Provider row | Result | Lifecycle evidence | Latency summary | Terminal outcome |
| --- | ---: | --- | --- | --- |
| SQLite/R2 | 91.94 IOPS; target not met | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 successful operations; elapsed 13,052.27 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 812.98/1,149.51/1,361.27 ms; read 1,020.14/1,165.82/1,205.45 ms; delete 159.86/205.24/213.11 ms | `ozone-compositions` `106497104006` failed; artifact `w26-ozone-compositions-evidence`, ID `10661329293`; `IOPS_TARGET_NOT_MET`; no `OZONE_IOPS_PASS` |
| PGlite/R2 | 95.82 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 12,523.00 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 817.05/979.48/1,029.07 ms; read 964.73/1,101.26/1,127.59 ms; delete 171.63/220.03/231.52 ms | `ozone-compositions` `106497104006` failed; same artifact; `IOPS_TARGET_NOT_MET`; no `OZONE_IOPS_PASS` |
| TiDB/R2 | 16.10 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 74,521.97 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 4,871.32/5,320.50/5,649.89 ms; read 5,978.51/6,392.12/6,691.18 ms; delete 1,016.64/1,336.85/1,467.71 ms | `ozone-tidb` `106497104443` failed; artifact `w26-ozone-tidb-evidence`, ID `10661094464`; `IOPS_TARGET_NOT_MET`; no provider acceptance |
| FoundationDB/R2 | 34.97 IOPS; target not met | 400/400/400/400 operation counts; 1,200/1,200 successful operations; elapsed 34,312.97 ms; timeouts 0; cleanup failures 0 | write median/p95/p99 2,167.39/2,553.72/2,714.75 ms; read 2,744.20/2,994.05/3,010.92 ms; delete 526.60/617.35/638.86 ms | `ozone-foundationdb` `106497103955` failed; artifact `w26-ozone-foundationdb-evidence`, ID `10661084241`; `IOPS_TARGET_NOT_MET`; no provider acceptance |

The provider logs retained owned cleanup markers (`OZONE_CLEANUP_PASS` and,
for the generic lane, `OZONE_COMPOSITION_CLEANUP_PASS`). TiDB and FoundationDB
also ended with their expected combo-failure status because the hard benchmark
threshold failed; this is not a timeout or cleanup regression. The aggregate
job `106501216372` reported
`W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000`.

### Published lazy-atime and EOF-read remediation

The next W26-owned performance chunk is implemented and published in commit
`d1bc8fb9`, reconciled with concurrent mainline changes and pushed at
`6429c7ba`. `ChunkedFs::read_at` now returns EOF before staging metadata and
coalesces ordinary read atime updates in coordinator state instead of issuing
one remote namespace publication per read. The pending value is visible to
local `stat`, is merged into the next fenced namespace snapshot, and is
flushed through the existing provider durability boundary on a mutation,
explicit sync/fsync or graceful shutdown. The implementation keeps the
inode/layout base check, lease renewal, revision CAS, orphan handling and
block-before-metadata ordering. The deliberate boundary is that atime may be
lost if the process crashes before one of those metadata publication points;
this is metadata freshness only and is not data durability or customer RPO
evidence.

The new regression `reads_coalesce_atime_and_eof_does_not_publish` proves
that a read changes local stat without changing the metadata revision, an EOF
probe performs no publication, and explicit sync persists the atime. The full
locked workspace test suite and strict workspace Clippy both exited 0 after a
narrow cleanup of the generated shared Cargo cache was needed to recover local
disk capacity; the changed ChunkedFs suite passed 16/16. Focused security diff
scan `5c0c59fb-a2c6-4e03-be6c-c334ca7cf0e7` completed with zero reportable
findings and partial coverage that records the crash boundary and all hosted,
provider, customer, SLO/RPO/RTO, backup/DR, native and release gates. No hosted
performance result exists for `6429c7ba` yet; the hard 1,000-IOPS threshold is
unchanged and the next complete packet must prove every configured provider.

The current result confirms that atomic whole-file publication is a measured
improvement over the prior 85.70/94.04/14.50/30.46 IOPS packet, but it does not
close W26.15. The next W26-owned design must remove or safely defer more
volume-wide metadata publications while preserving atime/POSIX behavior,
lease fencing, revision CAS, immutable-block ordering, cleanup and recovery;
the 1,000 target and fail-closed verifier remain unchanged.

### Published content-addressed caching, metadata batching and queue hardening

The W26-owned performance/safety chunks are included in current
`origin/main` `f10dbf22`. The R2 block path uses content-addressed object IDs and a
bounded process-local cache (64 MiB / 4,096 entries) so repeated benchmark
payloads do not repeatedly transfer identical immutable blocks. The
`ChunkedFs` metadata path batches eligible whole-file and unlink mutations into
one lease-fenced namespace publication while retaining the serialized conflict
fallback. The batcher then bounds pending requests at 1,024, drains unresolved
requests with `EIO` if its runner is canceled, and skips a queued mutation whose
reply channel has already closed. New-file mutations from one batch are rebased
to unique current inodes, while stale existing-file mutations still conflict.
These safeguards prevent unbounded queue growth, stale runner state and
post-cancellation namespace publication; they do not weaken the durable
publication acknowledgement boundary.

Implementation commit `c4e9a253` was published in reconciled remote tip
`a1cb4ca92bd0c1b619c56231002ee182852b01a8`; the queue-hardening follow-up was
committed as `d152fa1a`, and concurrent-create rebasing was committed as
`d3d6299e` and published through `059f801e` after concurrent mainline
reconciliation. The focused ChunkedFs suite passes 19/19, including
`canceled_mutation_runner_releases_queued_requests_without_publishing` and the
concurrent whole-file one-publication and concurrent-create inode-rebase
regressions. The full locked workspace
test suite, strict workspace Clippy and `git diff --check` passed. Security
diff scan `6d5a7665-d8cd-46b7-a9bd-73fec54f5431` completed with zero reportable
findings across the queue-hardening diff; its report explicitly defers hosted
Ozone TLS/IAM, customer isolation, provider durability, 1,000-IOPS capacity,
99.99% availability, five-minute RPO/RTO, backup/DR, native mounts and release
controls.

The current exact-SHA hosted qualification run is `35669685204` on this exact
revision. The prior terminal producer packet `35655276021` is diagnostic only:
SQLite/R2 measured 20.35 IOPS, PGlite/R2 205.48, TiDB/R2 18.09 and
FoundationDB/R2 40.50, despite 1,200/1,200 successful lifecycle operations and
zero timeout/cleanup failures in every row. No threshold is lowered and no
queued or failed job is promoted to acceptance.

The first rerun exposed a stale integration-test assertion before the
FoundationDB benchmark: `tests/ozone/src/lib.rs` still required identical
immutable bytes to receive different IDs, which contradicts the published
content-addressed R2 contract. The test now asserts same-bytes ID reuse and
different-bytes ID separation while retaining the range-read, missing-object,
conditional-create, stale-ETag, gateway fault/restart and cleanup checks. The
Ozone test crate compile-check passed, security diff scan
`46d4cf32-9d57-4f7e-975f-2616ddc53dc5` found zero reportable findings, and the
fix is committed as `edb6a43e` and included in current `origin/main` `f10dbf22`. Fresh
manual qualification `35663517544` is queued against that corrected exact SHA;
the prior run's FoundationDB failure is retained as a stale-test-contract
diagnosis, not provider-performance acceptance or failure evidence.

### Published provider lease-renewal fast path and forced validation boundaries

The next W26-owned performance/safety chunk is published in
`perf(w26): cache provider lease renewals on hot paths` (`786408b3`), included
in the reconciled remote tip `f10dbf2257364b4ca5acf2ec2d151b5258883dcb`.
`ChunkedFs` now caches a provider lease while its expiry remains outside a
five-second renewal margin, avoiding a remote lease transaction on every hot
operation. Explicit metadata/stat barriers, `syncfs`, shutdown and destructive
block reconciliation force provider validation/renewal; the provider's
fenced publish operation remains authoritative for durable namespace mutation.
The cache starts unset so the first real operation still validates the lease,
and reacquisition marks the new fence as validated. The implementation does
not claim provider clock authority or customer topology safety: FoundationDB's
oracle and each hosted provider's clock/expiry contract remain external gates.

Evidence is local and current-tip: focused ChunkedFs tests passed 19/19,
`./scripts/cargo-shared test --workspace --all-targets --locked` passed, strict
workspace Clippy with `-D warnings` passed, formatting and `git diff --check`
passed, and final security diff scan
`4265d7aa-1425-46dc-8620-b37a60ebf97a` completed with complete coverage and
zero reportable findings. The scan superseded an earlier pre-guard candidate;
forced validation was added before reconciliation and the final scan reviewed
that corrected working tree. Fresh hosted run `35669685204` is now terminal for
all W26 jobs but cannot close the 1,000-IOPS gate.

### Terminal lease-renewal qualification packet `35669685204`

The run selected exact tested SHA
`f10dbf2257364b4ca5acf2ec2d151b5258883dcb`. Base Ozone job `106563125215`
passed. The provider jobs completed their functional lifecycle and then failed
the fixed hard performance target; aggregate job `106566312898` failed closed
because the required `OZONE_IOPS_PASS` markers were absent. The parent workflow
was still active only for unrelated jobs, so the W26 result is terminal and
diagnostic even though the overall workflow status is not terminal.

| Provider row | IOPS / elapsed | Latency p50 / p95 / p99 (write; read; delete) | Lifecycle and failure evidence |
| --- | ---: | --- | --- |
| SQLite/R2 (`mount-rs-split-sqlite-r2`) | 631.16 / 1,901.25 ms | 109.913 / 622.616 / 840.388 ms; 88.582 / 139.080 / 147.021 ms; 17.647 / 48.828 / 68.467 ms | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 operations; timeout 0; cleanup failures 0; failed solely `IOPS_TARGET_NOT_MET`; artifact `10670349622` |
| PGlite/R2 (`mount-rs-split-pglite-r2`) | 552.65 / 2,171.34 ms | 202.549 / 1,116.198 / 1,189.241 ms; 12.625 / 40.090 / 41.819 ms; 5.022 / 9.112 / 25.304 ms | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 operations; timeout 0; cleanup failures 0; failed solely `IOPS_TARGET_NOT_MET`; same composition artifact `10670349622` |
| TiDB/R2 (`mount-rs-split-tidb-r2`) | 120.54 / 9,954.91 ms | 814.797 / 3,409.677 / 3,619.092 ms; 235.856 / 876.036 / 1,244.212 ms; 16.108 / 129.869 / 130.784 ms | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 operations; timeout 0; cleanup failures 0; failed solely `IOPS_TARGET_NOT_MET`; artifact `10669938745` |
| FoundationDB/R2 (`mount-rs-split-foundationdb-r2`) | 178.98 / 6,704.77 ms | 677.031 / 1,648.631 / 1,650.033 ms; 57.191 / 743.866 / 744.194 ms; 9.045 / 175.083 / 388.212 ms | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 operations; timeout 0; cleanup failures 0; failed solely `IOPS_TARGET_NOT_MET`; artifact `10669964184` |

The functional markers were green before the benchmark failure: the
composition lane passed SQLite/PGlite chunked composition, Node SDK/CLI,
remote HTTP CLI and scoped cleanup; TiDB passed v8.5.7 identity, Rust and
N-API seed/reopen bounded-listing checks and its integration marker; and
FoundationDB passed configured/image/readiness/reachable checks, bounded
listing, latency, chunked reopen, lease-publication policy and N-API markers.
The retained base artifact `10670619624` contains the Ozone health, bucket,
fault-window, restart, integration and cleanup markers. This separates the
remaining W26 gate from a functional or cleanup regression: provider
publication/latency still misses the non-negotiable target on every row.

The next implementation investigation is therefore bounded to safe
publication-path throughput improvements that preserve the durable
acknowledgement boundary, revision CAS, fencing, immutable-block ordering,
POSIX behavior and fail-closed cleanup. A customer/Ozone capacity
qualification remains an external alternative only if the CI fixture is shown
not to represent the supported production topology; the target will not be
lowered and failed rows will not be converted to skips.

### Published metadata publication-barrier fast path

The next W26-owned performance chunk is now implemented and published in
`perf(w26): skip redundant provider flush probe` (`c7f0e6d0`), included in
the exact hosted revision `fbc8346d292147bba006cfdcbc3b8f8b7127e2e7` after
concurrent-mainline reconciliation. `MetadataStore` now exposes a conservative
opt-in capability stating that a successful provider transaction already
includes the same acknowledgement barrier as the post-publish `flush` call.
The default is false for custom providers. SQLite, PGlite, TiDB and
FoundationDB opt in based on their awaited transaction-commit semantics;
SDK, observability and N-API erasure layers forward the capability without
changing provider selection or credentials. `ChunkedFs` still flushes
immutable blocks before publication, preserves lease/fence/revision CAS and
fails closed on publish errors. Explicit `syncfs` still calls the provider
metadata flush, and the new regression proves an opted-in provider skips only
the redundant post-publish probe while `syncfs` failure still fails closed.

Local evidence is green: focused ChunkedFs tests pass 20/20, the full locked
workspace/all-target test suite passes, strict workspace Clippy with
`-D warnings`, formatting and `git diff --check` pass. Security diff scans
`e44de27d-96d8-4330-a831-b995d6458a6c` and
`47641152-6539-48e1-96b6-bf2201033486` reviewed their changed-file scopes with
complete coverage and zero reportable findings. Fresh hosted run
`35674425514` is selected on exact published SHA `a81a0827`; its W26 producer
jobs are queued and not yet terminal, so it is a qualification packet in
progress rather than acceptance evidence.

### Published single-flight R2 block-upload coalescing

The next correctness-preserving performance chunk is published in
`perf(w26): coalesce concurrent R2 block uploads` (`d05548e8`), included in
the current shared tip `412c422e2485a5c7ce2caf55892ec6475faab8d8`. The R2 block
store now coordinates concurrent misses by content-addressed block ID: one
leader performs the provider `PutMode::Create` operation, followers await the
same result, and the leader's cache insertion happens before successful
completion is released. Collision verification, prefix validation and
conditional-create handling are unchanged. A drop guard removes abandoned
entries and wakes followers with a bounded `EIO` failure, so a canceled upload
cannot strand requests or leave a stale completion reusable. The coordination
is process-local and scoped to clones of one `R2BlockStore`; separate store
instances still use the existing provider conditional-create and immutable-byte
verification path.

Local evidence is green: the R2 package has 19 unit tests and 2 HTTP interop
tests, the full locked workspace/all-target test suite passes, strict workspace
Clippy with `-D warnings`, formatting and `git diff --check` pass. Security
scan `739f4f5b-8cc4-4154-93f7-9e486745eab1` reviewed both changed files with
complete coverage and zero reportable findings. This is local
implementation/security evidence, not a hosted IOPS pass. Fresh run
`35676336466` targets the published shared tip and is the first hosted packet
that can qualify this chunk.

### Terminal hosted Ozone qualification packet `35674425514`

The previously pending packet is now terminal. Base Ozone and the durable
provider seed/restart jobs passed where applicable, but all four W26 benchmark
rows failed only the hard performance marker; no timeout, cleanup or lifecycle
correctness failure was observed. Retained artifacts are:

| Provider row | Result | Lifecycle evidence | Artifact / terminal outcome |
| --- | ---: | --- | --- |
| SQLite/R2 | 637.01 IOPS | 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 operations; elapsed 1,883.80 ms; timeouts 0; cleanup failures 0 | `ozone-compositions` job `106577785981`; artifact ID `10671999742`; `IOPS_TARGET_NOT_MET` |
| PGlite/R2 | 457.65 IOPS | 400/400/400/400 operations; 1,200/1,200; elapsed 2,622.08 ms; timeouts 0; cleanup failures 0 | `ozone-compositions` job `106577785981`; same artifact; `IOPS_TARGET_NOT_MET` |
| TiDB/R2 | 147.31 IOPS | 400/400/400/400 operations; 1,200/1,200; elapsed 8,146.15 ms; timeouts 0; cleanup failures 0 | `ozone-tidb` job `106577785755`; artifact ID `10672129411`; `IOPS_TARGET_NOT_MET` |
| FoundationDB/R2 | 126.50 IOPS | 400/400/400/400 operations; 1,200/1,200; elapsed 9,486.40 ms; timeouts 0; cleanup failures 0 | `ozone-foundationdb` job `106577785903`; artifact ID `10672792512`; `IOPS_TARGET_NOT_MET` |

Base Ozone job `106577785881` passed and its retained evidence is artifact
`10672387255`. The complete packet verifier job `106579811902` failed closed
after downloading the four producer artifacts because no provider emitted
`OZONE_IOPS_PASS`. This remains diagnostic evidence only; it does not qualify
W26.15 or production readiness. The result improved SQLite over the prior
436.67-IOPS packet but did not close the 1,000-IOPS gate, and the next packet
must remain strict.

### Current terminal work-item status from run `35674425514`

The detailed rows below retain the current terminal packet rather than the
older `35669685204` baseline. This override prevents historical queued,
in-progress or lower-throughput wording elsewhere in the ledger from being
mistaken for the current result:

| Work item | Current status | Current completion | Current evidence / remaining action |
| --- | --- | ---: | --- |
| W26.3a | Functional composition passed; hard IOPS failed | 100% functional / 90% qualification | SQLite/R2 637.01 and PGlite/R2 457.65 IOPS; each completed 1,200/1,200 lifecycle operations with zero timeout/cleanup failures; retain artifacts and rerun against the single-flight code |
| W26.3c | Durable FoundationDB lifecycle/restart/listing markers passed; hard IOPS failed | 90% base / 86% extension | FoundationDB/R2 126.50 IOPS; 1,200/1,200 lifecycle operations; zero timeout/cleanup failures; preserve provider/restart evidence and rerun |
| W26.3d | Durable TiDB lifecycle/listing markers passed; hard IOPS failed | 100% base / 86% extension | TiDB/R2 147.31 IOPS; 1,200/1,200 lifecycle operations; zero timeout/cleanup failures; preserve fencing/ambiguous-commit evidence and rerun |
| W26.4b / W26.14 | CI and complete end-to-end packet fail closed on the performance marker | 100% implementation / 67% and 49% qualification | Base Ozone job `106577785881` passed, producer jobs `106577785981`, `106577785755`, `106577785903` failed only the hard IOPS marker, and aggregate `106579811902` failed closed; retain artifacts and rerun one exact revision |
| W26.15 / P8 | **NO-GO**; implementation controls are shipped, performance qualification remains open | 92% implementation / 40% hosted qualification; P8 58% | Every configured provider missed 1,000 IOPS; continue safe provider/publication work or obtain a clearly comparable customer/Ozone capacity qualification; do not lower or skip the gate |
| P14 | **NO-GO** integration-readiness review | 37% | The latest terminal packet is diagnostic only; close only after all configured providers, full end-to-end markers and the aggregate pass on one revision, with customer/Ozone security, SLO/RPO/RTO, DR and release boundaries still explicit |

### Current live qualification override — run `35676336466`

The current hosted packet supersedes the terminal-row status only as a live
pending state. It selected exact SHA
`503f3f757c8b599e11dbd0c5f99f8cfcc67aee5` and includes implementation commit
`d05548e8`; W26 producer jobs `106583542086` (base Ozone), `106583542156`
(compositions), `106583542085` (TiDB) and `106583542280` (FoundationDB) are
queued, and the aggregate job is not yet present. No row is acceptance evidence
until the configured producers and aggregate are terminally successful on this
exact revision. The terminal failure and completion percentages above remain
the authoritative qualification baseline while this run is pending.

## Scope decisions recorded from product direction

These decisions were supplied on 2026-09-21 and supersede the earlier
assumption that W26 might own a deployable staging or customer production
environment. They define what W26 must make ready for other streams and
customers to deploy.

| Decision | Recorded answer | W26 consequence and evidence boundary |
| --- | --- | --- |
| Deployment ownership | Customers deploy Ozone; W26 is not the deployment operator | W26 must provide a production-grade Ozone-compatible integration and CI qualification packet. Customer topology, capacity placement, backup/DR and on-call execution are external gates. |
| Metadata providers | Support all available metadata providers where feasible | Qualify SQLite, PGlite, TiDB and FoundationDB against Ozone where the provider can run in CI; publish provider-specific limitations rather than treating one provider's result as universal. |
| Performance target | Each drive must sustain 1,000 IOPS | Add a repeatable CI workload and report operations, latency percentiles, concurrency, errors, resource envelope and provider/topology. CI performance is qualification evidence, not a customer capacity guarantee. |
| Reliability target | Tier 1 service, 99.99% reliability | W26 must test client retry, fencing, idempotency, restart/failover and error observability; 99.99% service availability is ultimately a customer Ozone deployment/SLO responsibility. |
| Recovery objectives | 5-minute RPO and 5-minute RTO | W26 must preserve acknowledged-commit and reopen/recovery semantics; Ozone backup/replication/restore mechanisms and measured RPO/RTO are customer/provider-owned. |
| Security | Proper production security requirements are required | Add secure endpoint/authentication/TLS/secret-reference, least-privilege, redaction, negative-path and audit-boundary checks that can run in CI; do not place credentials in the repository or ledger. |
| End-to-end surface | Everything must work end to end | Qualify Rust, Node, CLI, HTTP and advertised native/mount surfaces through the Ozone-backed path; native platform implementation and runner availability remain cross-workstream gates. |
| Backup/DR | Ozone owns backup and DR | Do not implement a competing W26 backup system. Record Ozone/customer backup, restore and failure-domain requirements as an external acceptance dependency and test W26 recovery behavior around them. |
| Release process | Another stream owns releases | W26 supplies reproducible CI evidence, compatibility notes and release inputs; promotion, signing, canary and rollback execution remain external. |
| Test environment | CI only; no staging | Do not claim staging or production acceptance. Build the strongest bounded hosted CI matrix possible and label customer-environment evidence as pending until supplied by the deployment stream. |

## Work-item ledger

| Work item | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / gates |
| --- | --- | ---: | --- | --- | --- | --- |
| W26.1 — Ozone 2.2.1 gateway harness, digest pinning, health/bucket lifecycle | Implementation and local/hosted gateway gates complete | 100% | `scripts/test-ozone.sh` ran against the official pinned Ozone 2.2.1 image on arm64. Health, bucket creation, immutable and duplicate-object behavior, stale ETag, CAS, concurrent writers, range/full/missing reads, binary fixtures, fault window, bounded stopped-gateway behavior, restart/reopen, integration, and cleanup passed. Final hosted run `35585066458`, job `106286459564`, emitted `OZONE_HEALTHY`, `OZONE_READY`, `OZONE_INTEGRATION_PASS`, and `OZONE_CLEANUP_PASS` on Linux-amd64. | Keep the nonsecure loopback limitation visible; no further W26 gateway action remains. | 0 h acceptance; 0–1 h review | Hosted Linux runner and image architecture are external. Local and hosted harnesses are loopback/nonsecure and not production replicated durability. |
| W26.2 — immutable-object and fault/restart behavior | Implementation and local/hosted gateway gates complete | 100% | The arm64 Ozone run passed immutable/duplicate-object, stale-ETag, CAS, fault-window, stopped-gateway, restart/reopen, and cleanup checks. The failure timeout was widened to 15 seconds after a measured 5.6-second loaded Docker failure window (`62bc099`). Final hosted Ozone job `106286459564` passed on `35585066458`. | Keep the provider/platform and production-durability boundaries explicit; no further W26 action remains. | 0 h acceptance; 0–1 h review | Hosted CI is the provider/platform gate. No claim is made for production TLS, authentication, power loss, or a production replicated Ozone deployment. |
| W26.3a — SQLite and disk-backed PGlite composition over real Ozone | Functional composition complete; latest terminal packet failed hard IOPS; single-flight qualification queued | 100% functional; 90% production qualification | Run `35674425514` completed 1,200/1,200 lifecycle operations with zero timeout/cleanup failures but measured SQLite/R2 637.01 and PGlite/R2 457.65 IOPS, both below 1,000; producer and aggregate failed closed. Published single-flight R2 upload coalescing is `d05548e8`; fresh run `35676336466` targets exact SHA `503f3f75` with composition job `106583542156` queued. | Inspect the retained current-tip composition artifacts and close only on strict provider pass markers plus aggregate end-to-end success; otherwise continue correctness-preserving publication/provider work. | ~1–4 d follow-up performance/design; 0.5–1.5 d hosted review | Current Ozone fixture capacity/topology and customer target capacity remain external, but the remaining publication/provider latency cost is still a W26 gate. |
| W26.3b — independent single-node TiDB composition over Ozone | Local smoke and contract complete; not replicated acceptance | 100% of this sub-item | Single-node TiDB/Ozone run passed TiDB identity/provider/fencing, ChunkedFs partial/truncate/CAS/stale-fencing/reopen, ambiguous commit, and cleanup. Evidence was explicitly labeled `single-node-smoke-not-replicated-acceptance`. | Preserve this as a lower-level contract signal only; it does not replace the durable hosted topology gate. | 0 h for current scope | Single-node TiDB is intentionally not a durability or failover claim. Local machine capacity is below the durable TiDB harness minimum. |
| W26.3c — independent durable three-node FoundationDB composition over Ozone | Durable provider lifecycle and restart markers passed; latest terminal packet failed hard IOPS; single-flight qualification queued | 90% base; 86% extension | The arm64 durable FoundationDB proof and provider-backed bounded-listing contract remain green. Terminal hosted job `106577785903` in run `35674425514` completed all 1,200 lifecycle operations and measured 126.50 IOPS, with zero timeout/cleanup failures but `IOPS_TARGET_NOT_MET`; artifact `10672792512` is diagnostic only. Current `origin/main` `412c422e` includes the single-flight upload change; fresh run `35676336466` job `106583542280` is queued. Local FoundationDB binary linking remains blocked by missing `libfdb_c`. | Inspect the retained rerun while preserving durable restart, fencing, bounded-listing and cleanup assertions; do not promote until the provider and aggregate markers pass. | ~1–4 d shared concurrency follow-up; 0.75–1.5 h hosted review | FoundationDB client image/runtime, Ozone capacity/topology and customer secure durability remain external; local provider-native linking remains blocked. |
| W26.3d — durable multi-node TiDB composition over Ozone | Durable TiDB lifecycle and N-API bounded markers passed; latest terminal packet failed hard IOPS; single-flight qualification queued | 100% base; 86% extension | Terminal hosted job `106577785755` in run `35674425514` completed all 1,200 lifecycle operations and measured 147.31 IOPS, with zero timeout/cleanup failures but `IOPS_TARGET_NOT_MET`; artifact `10672129411` is diagnostic only. Current `origin/main` `412c422e` includes the single-flight upload change; fresh run `35676336466` job `106583542085` is queued. The atomic path retains fencing/CAS and serialized conflict fallback, with local full-workspace tests and Clippy green. | Inspect the retained rerun while preserving restart/ambiguous-commit/fencing/bounded-listing/cleanup assertions; do not promote until the provider and aggregate markers pass. | ~1–4 d shared concurrency follow-up; 0.75–1.5 h hosted review | GitHub-hosted TiDB/PD/TiKV, N-API runtime, Ozone capacity/topology and customer secure durability remain external. |
| W26.4a — Node provider matrix and Rust/Node CLI coverage | Implementation and local/hosted composition gate complete | 100% | The live arm64 composition run passed Node SDK coverage including PGlite-to-S3 partial/truncate/reopen (`pass=7 skip=1 fail=0`), Node CLI coverage, and the matching ignored Rust CLI live Ozone split-provider/reopen test. Final hosted `ozone-compositions` job `106286459622` passed on `35585066458` with `SUMMARY node-sdk pass=7 skip=1 fail=0`. | Keep the Node matrix's one intentional skip and native/provider boundaries visible. | 0 h acceptance; 0–1 h review | Hosted NAPI/PGlite build and Linux Node runtime are provider/native gates. Local arm64 evidence is not Linux-amd64 evidence. |
| W26.4b — CI wiring, pinned images, cleanup, and documentation | Implementation complete; latest terminal packet failed the hard provider gate; single-flight qualification queued | 100% implementation; 67% hosted packet qualification | CI installs NAPI/PGlite with locked Rust dependencies, runs the Ozone composition matrix, Node/CLI coverage, the shipped HTTP server/client reopen path, and durable Ozone/TiDB/FoundationDB jobs. Run `35674425514` retained base/provider artifacts; base Ozone passed, every configured provider missed 1,000 IOPS and aggregate `106579811902` failed closed. Fresh run `35676336466` targets exact SHA `503f3f75` with W26 producers `106583542086` (`ozone`, queued), `106583542156` (`ozone-compositions`, queued), `106583542085` (`ozone-tidb`, queued) and `106583542280` (`ozone-foundationdb`, queued); the aggregate job is not yet present. The current shared tip is `412c422e` and includes the same W26 code plus later unrelated mainline changes. | Retain exact current-tip artifacts, policy markers, end-to-end markers, performance markers and cleanup; promote only after all producer jobs and `w26-ozone-evidence` are terminally successful on one revision. | ~1–4 d performance follow-up; 0.75–1.5 d hosted review per retained run | Workflow concurrency, hosted provider startup, Ozone capacity/topology, artifact service and customer TLS/IAM remain external; no failed/queued/skipped artifact is acceptance evidence. |
| W26.14 — complete end-to-end Ozone evidence-surface gate | Implementation complete; latest terminal packet failed performance; next safe remediation required | 100% implementation; 50% production qualification | Expanded `scripts/verify-w26-ozone-evidence-packet.mjs` to require gateway health/ready/restart, SQLite/PGlite composition and bounded listing, Rust CLI, Node provider matrix/CLI, remote HTTP CLI, TiDB Rust/N-API seed/reopen and FoundationDB Rust/N-API seed/reopen markers. Local benchmark/evidence tests, Node/shell/YAML checks, diff checks, full locked workspace tests and strict Clippy pass. Run `35683158821` retained complete provider logs/artifacts and aggregate `106606577835` failed closed because composition lacked `OZONE_IOPS_PASS`; every configured provider row completed 1,200/1,200 lifecycle operations with zero timeout/cleanup failures but missed 1,000 IOPS. | Close the packet only after a post-remediation run has terminal pass evidence from every producer and aggregate; retain native/mount and customer secure-runtime evidence separately. | ~1–4 d W26 performance follow-up; ~0.75–1.5 d hosted review; ~0.75–1.25 h security review | Ozone fixture capacity/topology, hosted provider/N-API execution, artifact authorization, native mount runners, customer TLS/IAM/rotation and measured SLO/RPO/RTO remain external. |
| W26.15 — per-drive 1,000-IOPS qualification and safe performance remediation | Metadata batching, content-addressed cache, queue-cancellation safety, concurrent-create inode rebasing, lease-renewal fast path, publication-barrier optimization, bounded eight-round mutation collection, single-flight R2 upload coalescing, Ozone test-contract/cleanup corrections and PGlite/TiDB conditional publication CAS implemented/published; hard target still open; **NO-GO** | 94% W26-owned implementation / 42% hosted qualification | Published W26 performance work includes atomic whole-file write `96a25f17`, lazy atime/EOF `d1bc8fb9`, R2 content addressing/cache, single-flight coordination `d05548e8` (published through current descendants), mutation batching `c4e9a253`, queue hardening `d152fa1a`, concurrent whole-file-create rebasing `d3d6299e`, lease-renewal caching/forced validation `786408b3`, publication-barrier capability `c7f0e6d0`, bounded mutation collection `183660a4` and SQL publication fast path `214b9a6b`. The new provider path issues the fenced conditional update first, retaining a locked read only for stale/revision/missing-row classification; it preserves parameterized values, transaction rollback, provider-clock expiry checks, commit boundaries and fail-closed unexplained outcomes. Focused PGlite/TiDB unit tests pass, full locked workspace tests, strict Clippy, formatting and diff checks pass; security scan `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` completed with complete changed-file coverage and zero reportable findings. Terminal run `35683158821` is diagnostic at SQLite/R2 754.59, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17 IOPS; every row completed 1,200/1,200 operations with zero timeout/cleanup failures but failed `IOPS_TARGET_NOT_MET`, and aggregate `106606577835` failed closed because no provider emitted `OZONE_IOPS_PASS`. | Continue with a correctness-preserving provider/storage performance chunk, run local gates and security review, publish it, then dispatch another exact-SHA full matrix; do not lower the target or convert failed rows to skips. | ~1.5–4 d implementation/design; ~0.75–1.5 d hosted review per rerun; external queue time excluded | GitHub runner/provider startup, Ozone fixture capacity/topology, provider-native performance and clock contracts, artifact retention, customer secure topology and capacity are external gates; no staging environment exists. |
| W26.7 — credential-free Ozone production configuration policy | Credential-free hosted policy gate passed; hosted/customer security gate open | 100% implementation; 65% production qualification | `scripts/verify-w26-ozone-production-config.mjs` rejects inline secret-like values, unknown provider fields, non-HTTPS R2 endpoints, unsafe paths/prefixes, non-durable metadata and non-TLS TiDB URLs. Positive SQLite, PGlite, TiDB and FoundationDB fixtures plus independent HTTP, inline-secret, FoundationDB-authority and TiDB-TLS negative fixtures passed in terminal `ozone` job `106451808629` on run `35635486040`, without provider connections or real credentials. Local positive/negative execution and focused scans `d74e3e86-2e0a-45cf-9819-e31f428eb5d4` / `7addeeb5-4601-4951-aca9-becffb9bd4b9` also passed. | Bind the policy to customer Ozone endpoint/IAM/certificate/secret-rotation evidence and confirm runtime deployment configuration matches the validated references. | 1–2 h review/handoff | Customer Ozone TLS/IAM, secret manager and rotation, host permissions, provider-native security and audit evidence remain external. |
| W26.5 — explicit immutable-block reconciliation and open-unlink safety | Implementation and local contract tests complete; provider/platform acceptance open | 100% implementation; 50% production qualification | `BlockStore::reconcile` now fails closed by default; `ChunkedFs::reconcile_blocks` rejects zero grace before taking the lease, renews the writer lease, roots the committed namespace and open-unlinked handles, and delegates scoped cleanup. R2/Ozone blocks stream only their validated prefix, retain live/recent objects, delete only aged unreferenced objects and return bounded counts without materializing the entire listing. Rust SDK, observability, fault-injection and N-API wrappers forward the capability; locked R2/chunked/wrapper tests, strict Clippy and the rebuilt N-API chunked test passed. | Add provider-native enumeration/reconciliation where supported or retain explicit `ENOTSUP`; exercise ambiguous publication, object loss, quotas/space pressure, metrics/alerts and the customer/Ozone maintenance owner in hosted CI. The current-tip security scan is complete with zero local reportable findings, but its hosted/customer follow-ups remain open. | ~2.5–4.5 h implementation and local verification; ~0.5–2 d hosted/provider/security review | Ozone/customer retention policy, provider listing/deletion semantics, hosted credentials/topologies and alert collector are external gates. |
| W26.6 — bounded remote directory enumeration and response materialization | Built-in and KV/N-API provider-boundary implementation complete; provider/native qualification open | 100% implementation; 81% production qualification | `FsDriver::readdir_bounded` fails closed with `ENOTSUP` by default. HTTP `/entries` and directory-file routes request the bound before serialization and map overflow to the existing 413/connection-close contract. Memory, host, chunked, versioned, persisted, observability, CLI and native N-API wrappers implement or forward the boundary. `KeyValueStore::get_keys_bounded` is an optional provider-side contract; the unstorage bridge exposes `getKeysBounded(prefix, maxKeys)` and `Filesystem.readdirBounded(path, maxEntries)`, while providers without the callback remain fail-closed. Terminal local evidence: KV 13 integration tests, N-API Rust 16 tests, HTTP 8 unit + 12 integration, core 11, host 5, chunked 14, CLI 44, observability 4 and persistence 4; release N-API packaging, unstorage bridge, typecheck, chunked smoke, strict affected-package Clippy, formatting and diff checks passed. The parity fixtures pass capable-provider overflow/success and absent/legacy-callback `ENOTSUP`; SQLite/PGlite Ozone composition asserts bounded success and `EOVERFLOW`; the adapter forwards exactly the caller's limit with a 13-test regression asserting `[2, 3]`. Published `d1c9e44` adds the same provider-backed success/overflow and seed/reopen assertions to the TiDB/RustFS and FoundationDB/RustFS composition lanes. `b80c19c` adds the feature-built FoundationDB Node/N-API Ozone test, and `ef6a876` adds the corresponding TiDB Node/N-API Ozone test; both assert bounded success/overflow in seed/reopen and scope their prefixes under the owned Ozone run. TiDB test compilation and strict Clippy pass; FoundationDB `cargo check --tests` passes, while local test-binary linking is blocked by missing `libfdb_c`; both non-feature Node invocations skip safely on this host. `MOUNTX_SOURCE` parity remains skipped because the provider source is unset. Current standard security scan `5ad61e60-20e3-4223-885a-d4b516d49bb1` is complete at `44b01a7` with zero reportable findings across 16 W26-relevant surfaces; semantic coverage is explicitly partial and defers customer Ozone TLS/IAM/rotation, provider-native allocation, dependency provenance, native/platform and production SLO/recovery controls. | Review terminal Ozone composition results for all provider-backed assertions, exercise the contract in the remaining hosted provider jobs, add provider-native pagination where a backend can safely enforce it, retain `ENOTSUP` for legacy providers, run `MOUNTX_SOURCE` parity, and retain the explicit security follow-ups. | ~3.5–5.5 h implementation/local verification; ~1–3 d provider/security/hosted review | Provider-side key enumeration, JavaScript callback implementation, hosted provider topologies, secure Ozone fixture, native FoundationDB library, mountx source and customer security controls remain external or cross-workstream gates. |

| W26.8 — strict provider-configuration IOPS qualification | Strict provider selection implemented and exercised; all four configured rows failed the hard target | 100% implementation; 25% production qualification | `--require-configured` correctly prevented missing-provider skips from becoming passes. Run `35635486040` exercised the configured SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 rows; each produced a configured lifecycle result and each failed only because measured IOPS was below 1,000. No configuration skip was promoted. | Keep strict no-skip behavior, fix or qualify the performance path, and rerun all four rows with provider markers and retained artifacts. | 1–2 d implementation/design; 0.75–1.25 h hosted review | W26 `ChunkedFs` serialization is a current implementation gate; Ozone credentials, provider topologies, artifact service and customer capacity remain external. |

| W26.9 — IOPS artifact integrity and fixed production profile | Fail-closed artifact/profile verifier implemented; terminal artifacts correctly rejected | 100% implementation; 25% production qualification | The verifier still requires the exact 4 KiB/400-iteration/concurrency-64 profile, `requireConfigured=true`, target >=1,000, complete lifecycle samples, zero timeout/cleanup failures and exact provider set. Producer artifacts from run `35635486040` were retained, but no wrapper emitted a pass marker because SQLite/R2 61.97, PGlite/R2 63.56, TiDB/R2 14.14 and FoundationDB/R2 23.07 were below target. | Preserve the verifier unchanged; after a performance fix or accepted Ozone capacity qualification, rerun and require all four provider artifacts to pass it. | 1.5–2.5 h verification/test work; 0.75–1.25 h hosted review | Provider startup, artifact retention/access, provider TLS/IAM, Ozone topology and customer capacity remain external; weakening the target is prohibited. |
| W26.10 — fail-closed retention of IOPS evidence artifacts | Retention implementation passed; failed producer artifacts retained for diagnosis | 100% implementation; 55% production qualification | The three W26 IOPS upload steps retained the generic composition, TiDB and FoundationDB JSON/log artifacts on run `35635486040`; artifact digests and IDs are recorded above. The failed provider steps did not silently lose their evidence, and missing artifacts would still fail closed. This retention proves diagnostic availability, not performance acceptance. | Retain the next rerun’s artifacts and confirm identity, digest, exact profile, provider marker and cleanup outcome only after the hard IOPS gate passes. | 0.25–0.5 h local verification; 0.5–1 h hosted review | GitHub artifact service, workflow concurrency, hosted provider startup and terminal provider success remain external. |
| W26.11 — aggregate one-revision Ozone evidence packet | Aggregate verifier implemented and failed closed on the terminal provider failures | 100% implementation; 20% production qualification | Aggregate job `w26-ozone-evidence` `106458415293` downloaded all four producer artifacts from run `35635486040`, then emitted `W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000`. This is the intended fail-closed behavior: a failed producer cannot be promoted through a retained artifact. | Rerun one current-tip packet only after all four producer jobs can emit strict pass markers; retain exact source revision, policy markers, provider acceptance/restart/fault markers and cleanup outcome. | 1–2 h hosted packet review per rerun; no verifier implementation work currently indicated | Artifact service, hosted provider startup, Ozone credentials/TLS/IAM, customer capacity/SLOs, native clients, Ozone backup/DR and release ownership remain external. |
| W26.12 — strict IOPS metric and payload-map integrity | Metric and payload integrity implementation passed; hosted rows are complete but below target | 100% implementation; 25% production qualification | The terminal provider JSONs had finite latency/operation statistics, exact 400 write/read/delete/verified-read samples, 100% lifecycle success, zero timeouts and zero cleanup failures. They nevertheless correctly reported `iopsTargetMet=false` for all four provider rows. This distinguishes metric integrity from target qualification. | Keep the strict metric checks, fix or qualify the operation path, then run the fixed profile plus a meaningful soak/capacity variant on an accepted hosted topology. | 0.75–1.25 h local verification; 0.5–1.25 h hosted review | Hosted provider startup, artifact service, Ozone capacity, latency/SLO interpretation and customer topology remain external. |
| W26.13 — credential-free customer Ozone production-rollout contract | Implementation complete; customer/hosted admission evidence open | 100% implementation; 30% production qualification | `scripts/verify-w26-ozone-rollout-contract.mjs` validates the Tier-1 99.99%/5-minute RPO/5-minute RTO envelope, qualified Ozone version, TLS peer/identity verification, external CA/SigV4 references, tenant-scoped prefixes, three-node/three-replica/three-failure-domain durable topology, all four advertised metadata providers, operations controls and customer-owned restore-drill requirements without network access or credential values. The positive fixture and independent weak-RTO/inline-secret negatives are wired into the Ozone policy log; the aggregate packet requires all three new markers. Local contract/packet/benchmark tests, YAML parsing, Node syntax and diff checks passed. Commit `8d2cfbb` was reconciled and published at `0398d94`; security diff scan `33c09a35-77f5-417c-862c-e5848185f50e` found zero reportable findings with customer/hosted coverage explicitly deferred. | Obtain customer topology/IAM/certificate/secret-manager evidence; retain terminal one-revision provider packet; measure availability/RPO/RTO and run the customer-owned restore/failover drills. | 1.5–2.5 h implementation/tests; 0.75–1.25 h security review; 0.5–1 h handoff review | Customer Ozone topology, certificate and IAM owners, secret manager, hosted provider execution, artifact retention and operations are external. |

## Production rollout readiness — post-demo track

The successful demo and W26 hosted packet prove the requested qualification
scope; they do not authorize a production rollout. This section is a separate
workstream with its own completion percentages, evidence, owners/gates and
exit criteria. A production gate can only move to complete when the evidence is
from the named production-like environment, on a retained revision, with the
provider/native boundary stated explicitly. Local Docker, demo behavior,
non-secure loopback services, and a green hosted fixture are useful
qualification evidence but are not production approval.

### Current production decision

| Decision | Status | Evidence now available | Exit condition |
| --- | --- | --- | --- |
| W26 integration readiness | **NO-GO** | The retained green packet is still `35585066458`, while the last terminal diagnostic packet `35674425514` on `a81a0827` failed closed: all four configured provider rows completed 1,200/1,200 lifecycle operations with zero timeouts/cleanup failures yet measured only 637.01/457.65/147.31/126.50 IOPS against 1,000, and aggregate job `106579811902` rejected the missing pass marker. W26-owned packet, policy, metric, retention, end-to-end verifier, R2 cache, single-flight upload coalescing, metadata batching, queue-cancellation controls, corrected Ozone content-addressed-ID test, lease-renewal validation, publication-barrier optimization and bounded mutation collection are implemented and locally tested in current `origin/main` `412c422e` (W26 implementation SHA `d05548e8`); W26.15 is open for hosted qualification and any further correctness-preserving performance work. Manual run `35676336466` is queued on its exact tested SHA `503f3f75`, while current shared mainline has since advanced with unrelated changes. | A current-tip retained revision must close W26.15 and produce terminal pass markers for every configured provider plus the full end-to-end packet; customer Ozone deployment, DR and release remain explicit external dependencies |
| Customer production target | Customer-deployed Ozone; topology not supplied | Product direction fixes the service envelope at 1,000 IOPS per drive, 99.99% reliability and five-minute RPO/RTO, but W26 does not operate the customer topology | W26 documents the Ozone/provider/client contract; customers and deployment streams provide secure topology, backup/DR, monitoring and measured availability/recovery evidence |
| Qualification baseline | Historical green packet only | Revision `9c098e5` and hosted run `35585066458` are the last accepted packet within their documented boundaries. Terminal run `35674425514` on exact revision `a81a0827` is diagnostic and failed the hard provider-performance gate; current `origin/main` `412c422e` contains the single-flight R2 upload coalescing change `d05548e8`, while queued run `35676336466` qualifies its tested ancestor `503f3f75` | Any performance/concurrency change gets a fresh full packet on the tested revision; no failed, queued, canceled or skipped result is promoted |

### Production gate ledger

W26.13 closes the repository-side contract-definition slice across P0/P1/P3/P6
and P10; it does not close the corresponding hosted or customer gates:

| Contract slice | Status | Completion | Evidence | Remaining action / gate |
| --- | --- | ---: | --- | --- |
| Tier-1 service envelope and ownership | Implemented, declaration-only | 100% implementation / 20% production gate | `tests/ozone/production-rollout-contract.json` requires 99.99% availability, 5-minute RPO/RTO and customer/Ozone recovery ownership | Customer supplies measured availability and recovery evidence; W26 does not operate the service |
| Secure Ozone topology and tenancy | Implemented, deployment external | 100% contract / 20% production gate | Contract requires external endpoint/CA/bucket references, TLS peer/identity verification, SigV4, tenant-scoped prefixes and three-node/three-replica/three-domain durable topology | Customer supplies endpoint, certificates, IAM policy, replication/storage and tenant-isolation evidence |
| Provider and operations support matrix | Implemented, hosted/provider evidence open | 100% contract / 62% qualification gate | Contract enumerates SQLite, PGlite, TiDB and FoundationDB; existing policy/IOPS gates validate concrete provider shapes and strict artifacts | Retain one-revision terminal provider packet and customer-native provider operations evidence |
| Backup/DR and restore drill boundary | Implemented, Ozone/customer-owned | 100% contract / 10% production gate | Contract requires customer-owned backup/restore, a restore drill at least every 30 days and measured RPO/RTO evidence | Ozone/customer supplies backup, restore, failure-domain and timed-drill records |

| Gate / work item | Work type | Status | Completion | Evidence now available | Remaining actions / exit evidence | Provisional engineering time | External blockers / hosted or native gates |
| --- | --- | --- | ---: | --- | --- | ---: | --- |
| P0 — production scope, support matrix, SLO/RPO/RTO and ownership | Implementation / operations | Scope captured; CI acceptance baseline open | 60% | Product direction now records customer-deployed Ozone, all feasible metadata providers, 1,000 IOPS per drive, 99.99% reliability and 5-minute RPO/RTO; no customer topology is supplied | Turn these targets into provider-specific CI assertions, define the advertised client/platform matrix, document customer/Ozone-owned prerequisites and obtain owner sign-off on the support matrix | 0.5–1.5 d | Product/support decisions are mostly supplied; provider support limits and customer deployment owners remain external |
| P1 — customer Ozone topology and deployment rehearsal | External dependency — not a W26 deployment task | Customer-owned / not measured by W26 | 0% W26 deployment evidence | Current Ozone evidence is a pinned all-in-one, non-secure, loopback CI fixture with anonymous volumes and no production replication claim | Customer/deployment stream must provision and operate secure multi-node Ozone; W26 consumes CI-accessible endpoints or fixtures and documents the required topology contract | 0–1 d W26 contract review | Customer infrastructure, persistent storage, network policy, image architecture, certificates and environment access |
| P2 — production metadata-provider support matrix | Hosted/provider CI | Provider matrix exercised; current terminal performance gate failed | 72% | The benchmark has explicit SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 rows. Strict configuration prevented skips, and run `35674425514` exercised all four rows with complete lifecycle counts; SQLite/R2 637.01, PGlite/R2 457.65, TiDB/R2 147.31 and FoundationDB/R2 126.50 IOPS all missed 1,000. Provider-bounded listing and durable restart markers passed where reached; producer artifacts are diagnostic only. | Resolve or qualify the shared performance path, then retain terminal artifacts/markers for all four providers on one revision; record provider versions/HA/failure semantics and unsupported combinations, add capable KV/provider rows, and do not promote an explicitly skipped or failed provider | 2–5 d shared performance/design; 1–2 d hosted review | `ChunkedFs` metadata publication/provider latency is W26-owned; CI capacity, provider images/versions, Ozone capacity/topology and customer secure durability remain external |
| P3 — authentication, TLS, secret lifecycle and redaction | Implementation / hosted/provider CI | Local transport and expanded credential-free production-config policy hardened; secure integration gate open | 60% | R2/Ozone config parsing and runtime validation reject non-HTTP(S), embedded credentials, query/fragment, missing-authority and remote plaintext-HTTP endpoints before client construction; HTTP config and runtime now reject non-loopback binds, require loopback behind a TLS reverse proxy, keep credentials as environment references and redact diagnostics. The `b8d8fb1` policy gate validates SQLite/PGlite/TiDB/FoundationDB metadata shapes, HTTPS R2 blocks, exact external secret references and TiDB TLS options without provider connections; `7018b59` adds independent inline-secret, shared-provider-authority and TiDB hostname-verification negative cases. Local positive/negative policy checks passed | Add authenticated HTTPS endpoint CI where available, certificate identity/rotation checks, secret injection/rotation references, least privilege and clean-client negative tests; retain no secret values; review customer runtime configuration against the policy | 3–7 d | Secure Ozone CI endpoint or customer-supplied fixture, certificates/identity, secret manager integration, customer IAM and security review |
| P4 — replicated block durability and storage failure protection | Provider/customer deployment dependency plus CI contract | Lifecycle protection implemented; durability qualification open | 40% | Restart/reopen and durable provider checks pass in bounded CI topologies. The explicit reconciliation path rejects zero grace, protects committed and open-unlinked roots, retains a configurable grace window, streams the configured R2/Ozone prefix and scopes deletion to validated block IDs; no customer storage, power-loss or Ozone replication guarantee exists | Test client behavior for object loss, unavailable gateway, retries, integrity mismatch and recovery in CI; document Ozone replication/fsync/storage requirements, retention ownership, deletion authorization and space-pressure alerts that customers must satisfy | 1–3.5 d W26 CI work; deployment work external | Ozone storage and replication semantics, failure controls, retention policy and customer topology; CI restart is not power-loss evidence |
| P5 — fencing, ambiguous commit and stale-writer recovery under failover | Implementation / hosted/provider CI | Lease-protected reconciliation implemented; failover matrix open | 35% | W26 exercises CAS, stale fencing, ambiguous commit and durable restart in bounded provider compositions; hosted TiDB marker reports `ambiguous_commit=pass`; reconciliation renews the writer lease before deriving roots and never runs implicitly on shutdown | Extend all feasible Ozone/provider CI lanes with concurrent clients, retry, gateway/provider loss, delayed responses and post-ambiguity reconciliation; prove no stale publication, duplicate block or lost acknowledged commit | 3–7 d | Distributed CI fault controls, provider failover behavior and multiple-client scheduling |
| P6 — backup, restore, disaster recovery and retention | External dependency — Ozone/customer owned | Not a W26 implementation task | 0% W26 DR evidence | Product direction assigns backup and DR to Ozone/customer deployment; W26 has no competing backup system | Document the Ozone/customer requirements needed to meet 5-minute RPO/RTO and test W26 reopen/error behavior around supplied recovery scenarios when CI fixtures expose them | 0.5–1.5 d W26 contract documentation | Ozone backup/replication/restore design, failure domains, KMS and customer operations |
| P7 — observability, alerts, dashboards and runbooks | Implementation / CI contract / cross-workstream | Local HTTP/OTLP and provider-boundary evidence passed; deployment integration open | 45% | `mount-rs-http` passed 8 unit and 12 integration tests; the OTLP-enabled HTTP suite passed 11 integration tests including bounded error telemetry; full-feature observability passed 5 unit tests plus local collector and exporter-failure tests; CLI observability passed 44 unit, 9 CLI, 2 HTTP subprocess and 1 native-artifact test. `reconcileBlocks` returns scanned/protected/recent/deleted counts and fails closed when unsupported, while `readdir_bounded` is forwarded through observability/CLI wrappers. The N-API unstorage bridge also preserves the provider-boundary `EOVERFLOW` contract. These are local collector/fixture results, not deployed alerting. | Add/retain machine-readable Ozone/provider error categories, reconciliation and bounded-listing metrics, and health evidence in the hosted packet; coordinate dashboards, alerts and runbooks with W30/customer operations | 2–5 d W26 contract/tests | Collector reachability, alerting and paging are external; W30 and customer operations own deployed dashboards/paging |
| P8 — load, capacity, soak and cost envelope | Hosted/provider CI plus W26.15 remediation | Hard-threshold and one-revision packet gates implemented; latest terminal packet failed and production remains NO-GO | 60% | The benchmark records successful write+read+delete lifecycle IOPS, supports a hard `--min-iops` threshold, strict `--require-configured` provider qualification and redacted JSON. The fixed 4 KiB/400-iteration/concurrency-64 profile is enforced. Terminal run `35683158821` completed all 1,200 lifecycle operations per row with zero timeouts/cleanup failures but measured only 754.59/817.10/143.04/345.17 IOPS; aggregate job `106606577835` failed closed because the composition log lacked `OZONE_IOPS_PASS`. Published `96a25f17` optimized whole-file publication, `d1bc8fb9` avoids per-read/EOF metadata publication, later chunks add R2 content addressing/cache, single-flight upload coalescing, mutation batching, queue cancellation safety, concurrent-create inode rebasing, duplicate-cleanup correction, lease-renewal caching/forced validation, publication-barrier capability, bounded eight-round mutation collection and PGlite/TiDB conditional metadata publication `214b9a6b`. The next W26 chunk must diagnose provider/storage latency without weakening fencing, durability or the hard threshold. | Inspect retained provider JSON/logs, preserve p95/p99 latency and lifecycle/error evidence, implement the next safe performance chunk and add soak/capacity variants only after every hard provider row closes | 1.5–4 d W26 performance follow-up; 1–2 d hosted review; soak effort provisional | Ozone fixture capacity/topology, stable hosted runners, artifact service, provider quotas and customer 99.99% capacity remain external; do not lower the target |
| P9 — upgrade, rollback and compatibility | External release/deployment dependency | Not a W26 release task | 0% W26 migration evidence | W26 pins Ozone 2.2.1 and provider fixture versions for qualification only; release execution belongs to another stream | Supply compatibility notes, config/schema/object invariants and requalification commands for the release stream; do not own promotion or rollback automation here | 1–3 d W26 compatibility notes | Release stream, maintained provider versions, change window and customer deployment approval |
| P10 — security, privacy, tenancy and audit review | Published-tip local security review complete; hosted/customer security remains open | 86% | All prior scans remain zero-finding evidence within their scopes. Latest SQL publication diff scan `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` reviewed both changed provider files with complete coverage and zero reportable findings, including lease/revision CAS, rollback/commit behavior, provider-clock expiry, parameterization, fail-closed conflict classification and data boundaries. Hosted/provider TLS/IAM, customer Ozone security, provider clock authority, dependency/native provenance, 99.99%/recovery drills and production operations remain deferred. | Retain the sealed scan with the published revision, exercise provider-native security and artifact authorization in hosted CI, and obtain customer Ozone endpoint/IAM/certificate/rotation evidence; preserve the fail-closed contracts and no-secret policy | 2.5–7 d | Customer identity/tenancy model, secure Ozone endpoint/certificates, provider clock/expiry contract, provider-native allocation, compliance requirements, hosted CI and scanning infrastructure |
| P11 — end-to-end client, mount and platform qualification | Native/provider CI / cross-workstream | HTTP path and all implemented bounded-listing contracts added to Ozone CI; full matrix required | 46% | W26 covers Rust/Node/CLI and the shipped HTTP server/client path through the Ozone composition gate with scoped cleanup; built-in Rust providers enforce the directory bound before response materialization; the unstorage/N-API path has a provider callback, public `readdirBounded` API, Node error-shape test and TypeScript declaration; parity fixtures cover capable overflow/success and legacy fail-closed behavior; SQLite/PGlite plus the TiDB/RustFS and FoundationDB/RustFS composition tests assert provider-backed bounded success and `EOVERFLOW`; the feature-built FoundationDB and TiDB Node/N-API Ozone lanes now assert the same contract with scoped cleanup prefixes. TiDB compile/Clippy and FoundationDB check evidence pass locally, but live provider/Ozone execution is hosted-only. Native mount, providers without the callback, `MOUNTX_SOURCE` parity and every platform are not yet accepted | Review terminal Ozone evidence for HTTP and every bounded-listing marker and cleanup, add/retain bounded provider rows for key-value/JavaScript-owned metadata, run mountx parity and then exercise every advertised native/mount surface; retain platform/provider matrices, restart/recovery and negative capability evidence | 5–15 d depending on advertised platforms | macOS/Linux/Windows runners, privileged mount facilities, native workstreams, provider callback contracts, mountx source, signing and provider connectivity |
| P12 — release packaging, CI promotion, canary and rollback automation | External release stream | Not a W26 release task | 0% W26 release evidence | Product direction assigns releases to another stream; W26 hosted jobs provide qualification inputs only | Publish reproducible CI commands, version/image pins, evidence markers and compatibility notes for the release stream; no W26 canary claim | 0.5–2 d W26 handoff | CI/CD, artifact registry, signing keys, deployment platform and release owner |
| P13 — incident, failover and recovery rehearsal | External customer/Ozone operations plus CI fault contract | Not a W26 operator task | 0% W26 rehearsal evidence | W26 restart and cleanup tests are bounded qualification checks, not customer incident exercises | Add CI fault/recovery cases where controllable and document the operator scenarios customers must rehearse to meet 99.99% and 5-minute RTO | 1–3 d W26 fault contract | Customer on-call, Ozone operations, paging/incident tooling and maintenance windows |
| P14 — final W26 integration-readiness review | W26 implementation / hosted CI / handoff | NO-GO review documented; W26.15 remains open after terminal run `35683158821` | 39% | Terminal packet `35683158821` is audited: base Ozone passed; SQLite/R2 754.59, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17 IOPS all missed the hard target despite 1,200/1,200 lifecycle operations and zero timeout/cleanup failures; aggregate `106606577835` failed closed on missing hard pass markers. Publication-barrier implementation `c7f0e6d0`, bounded mutation collection `183660a4`, single-flight R2 upload coalescing `d05548e8`, SQL publication fast path `214b9a6b`, local compile/tests/Clippy and focused security scans remain green within scope; the next safe remediation must preserve the existing fencing/durability/security contracts. | Implement and verify the next performance chunk, dispatch a fresh one-revision packet, close W26.15 only with terminal pass markers for every provider and the full end-to-end surface, then attach provider/platform/security/performance evidence and hand off an explicit integration-ready or NO-GO decision | 1–2 d review after performance gate; 1.5–4 d W26.15 follow-up | All W26-owned CI gates plus external customer Ozone secure topology, capacity/SLO, DR and release-stream confirmations |

### Production rollout phases and provisional effort

The following plan is provisional engineering time, not a delivery promise. It
excludes provider provisioning, CI queues, approvals, maintenance windows and
other elapsed wall-clock gates.

| Phase | Gates | Provisional engineering effort | Exit |
| --- | --- | ---: | --- |
| Scope and support matrix | P0 | 0.5–1.5 d | Provider-specific CI assertions, client/platform matrix, 1,000-IOPS definition, 99.99% boundary and five-minute recovery-objective contract |
| Provider, security and failure CI | P2–P5, P7, P10 | 17–42 d | All feasible Ozone/provider lanes, secure endpoint checks, redaction, failure semantics, telemetry contract and security evidence |
| Performance and end-to-end CI | P8, P11, P14 | 10–27 d | Per-drive 1,000-IOPS workload plus Rust/Node/CLI/HTTP/native advertised surfaces and one-revision evidence audit |
| Customer/Ozone and release handoffs | P1, P6, P9, P12, P13 | 3–10 d W26 contract work | W26 supplies requirements and CI evidence; customer deployment, Ozone DR/backup, incident operations and release execution remain external |
| **Total provisional W26 engineering/contract range** | **P0–P14** | **31–82 d, plus external waits** | **Planning range only; customer deployment, Ozone DR and release execution are not W26 estimates** |

### Production evidence rules

- The W26 implementation percentage and the production percentage are
  separate. The 11 shipped implementation rows are complete, but W26.15 and
  the production track remain open until the hard performance gate and its
  current-tip packet close.
- A local demo, a green hosted fixture, or a provider restart test cannot be
  promoted to production evidence without the target topology, security
  posture, capacity envelope and operator/recovery context being recorded.
- Provider acceptance, native-platform acceptance, security review and release
  approval are independent gates. A passing implementation test does not close
  any of those external gates.
- Every production result must identify the exact revision, image/provider
  versions, topology, test start/end, terminal status, redacted markers and
  cleanup/rollback outcome. Queued, skipped, canceled or failed jobs remain
  non-evidence.
- Because no staging environment exists, W26 may claim only controlled CI and
  provider-fixture evidence. CI can qualify integration behavior and measured
  performance; it cannot by itself prove a customer's 99.99% availability,
  five-minute RPO/RTO or Ozone deployment topology.
- Missing credentials, infrastructure, certificates, provider access, native
  runners or approvers are recorded as blockers; they are not worked around by
  synthesizing local evidence or weakening the gate.

## Evidence boundaries

The following boundaries are intentional and remain part of the acceptance
record:

- A checked local implementation or local provider run is not hosted CI
  acceptance. Hosted results must be terminal, on the retained revision, and
  reviewed from the actual job output.
- SQLite, PGlite, single-node TiDB, and durable FoundationDB evidence cover
  different provider contracts. They do not imply that durable TiDB or every
  provider combination works.
- The durable FoundationDB proof is a real three-node composition with
  persistent Docker volumes and a node restart, but it is still a local
  loopback/nonsecure test rather than production auth/TLS, power-loss, or
  native-mount evidence.
- The single-node TiDB result is deliberately labeled a smoke/contract result,
  not replicated-durability acceptance.
- An active, queued, skipped, canceled, or failed hosted job is not a pass.
  The historical failed run `35581168122` and canceled reruns remain recorded
  as diagnosis only. Final run `35585066458` has terminal green W26 jobs on
  `9c098e5`: `ozone`, `ozone-compositions`, and `ozone-tidb`, with generic
  durable `tidb` also green. The separate RustFS/native lane is not required
  for W26's Ozone acceptance.
- Local Docker capacity is below the durable TiDB harness minimum. The local
  environment must not be used to manufacture a durable-TiDB result by
  overriding the capacity guard.
- The production rollout remains NO-GO. The non-secure loopback Ozone service,
  hosted fixture jobs, and demo behavior are not evidence of production TLS,
  authentication, replicated/power-loss durability, backup/restore, native
  mounting, capacity, security or operational readiness.
- The 1,000-IOPS target is a W26 CI qualification target, not a universal
  customer capacity guarantee. The 99.99% availability and five-minute RPO/RTO
  objectives depend on the customer's Ozone replication, backup, storage,
  monitoring and recovery design.
- The Ozone IOPS artifact now has one row per configured metadata provider over
  Ozone blocks. SQLite/R2 is the baseline row; PGlite/R2, TiDB/R2 and
  FoundationDB/R2 require their own endpoint/feature/topology prerequisites.
  A skipped row is an unavailable gate, never a provider pass or a substitute
  for another metadata backend.
- The current IOPS measurement is explicitly a three-operation filesystem
  lifecycle (`write + full read/verify + delete`) over the public Node
  split-provider path. It is a repeatable integration baseline, not a claim
  that one lifecycle equals every customer's physical-drive I/O profile.
- Block reconciliation is explicit and provider-scoped. R2/Ozone is the only
  current implementation that can enumerate and delete its immutable objects;
  SQLite, PGlite, TiDB and FoundationDB block providers retain the default
  `ENOTSUP` contract until they provide an equivalent safe enumerator. The
  grace period, writer lease, committed namespace roots and open-unlinked
  handles are safety inputs; shutdown success never implies cleanup.
- Bounded key enumeration is an optional provider promise, not a post-hoc slice.
  `KeyValueStore::get_keys_bounded` and unstorage's `getKeysBounded` must bound
  the backend operation before materializing its result; the legacy `getKeys`
  callback is intentionally not reused for remote bounded listings. Providers
  without the callback return `ENOTSUP`, and the N-API postlude preserves
  provider overflow as Node-style `EOVERFLOW`. The in-memory callback used by
  the local bridge test is a test oracle, not production provider evidence.

## Remaining-action checklist

- [x] Review workflow `35581168122` and record the terminal Ozone and mixed
  composition results plus the durable TiDB restart failure as historical
  diagnosis.
- [x] Move the dropped-COMMIT failure injection after durable restart/reopen,
  synchronize with the latest `origin/main`, and publish the bounded hosted
  gate; the first rerun was canceled by a concurrent push and is not treated
  as acceptance.
- [x] Review final run `35585066458`: terminal `ozone`,
  `ozone-compositions`, `ozone-tidb`, and generic `tidb` jobs all passed on
  the retained revision; keep the separate RustFS/native lane distinct.
- [x] Update the W26 row in `WORK_TRACKER.md` with the exact terminal hosted
  run and the evidence-backed acceptance markers.
- [x] Commit and push each completed chunk to `origin/main`; after every push,
  verify the remote revision and the resulting workflow state.
- [x] Close W26 when the tracker, hosted evidence, and provider-boundary
  notes agree. W26 is complete within the documented scope; no broader
  production or native-platform readiness claim is made.
- [x] Implement the lifecycle-safety slice: explicit scoped reconciliation,
  positive grace validation at every coordinator/API boundary, writer-lease
  fencing, committed/open-unlinked root protection, streamed R2
  prefix/block-ID validation, wrapper forwarding and
  N-API `ENOTSUP`/range-error behavior. Local locked tests and strict Clippy
  pass; hosted provider/retention/alert evidence remains open.
- [x] Add a fail-closed bounded directory-enumeration contract before HTTP
  materialization. Memory, host, chunked, versioned and persisted Rust paths,
  observability, CLI and native N-API wrappers implement or forward the bound;
  HTTP `/entries` and directory-file routes preserve the existing 413/close
  contract. Providers without a provider-side bound remain explicit
  `ENOTSUP`/pagination gates.
- [x] Add the provider-bounded key enumeration follow-up for key-value and
  JavaScript-owned consumers. `KeyValueStore::get_keys_bounded`, unstorage's
  `getKeysBounded`, N-API `Filesystem.readdirBounded`, generated TypeScript
  declarations and Node error-shape wrapping are implemented and locally
  tested; absent callbacks still fail closed. Hosted provider parity and
  `MOUNTX_SOURCE` suites remain open gates.
- [x] Add one-revision W26 Ozone evidence-packet aggregation. The CI lanes now
  retain policy/base/provider logs and provider IOPS JSON with fail-closed
  artifact uploads; `w26-ozone-evidence` downloads all four packets and
  verifies exact provider sets, matching clean source revisions, policy
  positive/negative markers, provider acceptance, Ozone fault/integration and
  cleanup markers. Local synthetic packet tests and workflow checks pass; the
  terminal aggregate job `106458415293` failed closed on missing provider pass
  markers after the hard IOPS target failed, so no packet is promoted.
- [x] Make the IOPS artifact verifier reject incomplete performance evidence.
  It now requires the fixed payload-size mapping, 100% lifecycle success,
  finite elapsed/operation statistics, zero timeout and cleanup failures, and
  exact operation/statistic sample counts. Local positive and negative
  artifact tests pass; hosted provider performance and customer capacity
  remain open.
- [x] Add the credential-free customer Ozone production-rollout contract. The
  validator and fixtures cover the Tier-1 service envelope, secure endpoint and
  external secret references, tenant scoping, durable topology, all four
  metadata providers, operations controls and customer-owned restore drills;
  local positive/negative, packet, syntax and YAML checks pass. This is
  declaration-only evidence; hosted/provider/customer gates remain open.
- [x] Make the one-revision packet fail closed on missing end-to-end surfaces.
  The packet now requires gateway health/ready/restart, SQLite/PGlite composition
  and bounded-listing, Rust/Node/remote-HTTP CLI, TiDB Rust plus N-API seed/reopen,
  and FoundationDB Rust/restart plus N-API seed/reopen markers. A synthetic
  missing-Node-CLI case fails as expected; hosted/provider/native/customer
  execution remains a separate gate.

### Production rollout checklist (open)

- [x] Record customer deployment ownership, all-feasible-provider intent,
  1,000 IOPS per drive, 99.99% reliability, five-minute RPO/RTO, end-to-end
  scope, external DR/release ownership and CI-only qualification.
- [ ] P0: obtain support-owner sign-off on the supported provider/platform
  matrix, provider-specific assertions, owners and approved non-goals. The
  credential-free service-envelope/provider-set contract is now implemented.
- [ ] P1: obtain the customer-supplied secure Ozone topology and deployment
  evidence against [`docs/w26-production-rollout.md`](w26-production-rollout.md);
  do not claim W26 staging or deployment ownership.
- [ ] P2–P5: close all feasible Ozone/provider CI lanes, secure endpoint tests,
  durability/error contracts and concurrent failover/fencing evidence.
- [ ] P6: obtain Ozone/customer backup, restore and timed-drill evidence against
  the documented five-minute RPO/RTO prerequisites; do not build a competing
  W26 backup system.
- [ ] P7/P10: close integration telemetry, redaction, threat-model, security
  CI and audit-boundary evidence.
- [ ] P7/P10 follow-up: implement provider-native bounded/paginated listing for
  key-value and JavaScript-owned providers, or retain their explicit fail-closed
  production contract with owner sign-off.
- [ ] P8/W26.15: close the per-drive 1,000-IOPS CI workload with latency,
  errors, resource and provider-specific results. The workload names every
  supported Ozone-backed metadata provider, has dedicated durable TiDB and
  FoundationDB invocations with hard thresholds and retained artifacts, and
  correctly failed closed on terminal run `35678993571` (exact code head
  `4b4fe43a`) after measuring SQLite/R2 802.37, PGlite/R2 614.90, TiDB/R2
  86.53 and FoundationDB/R2 360.14 IOPS. All rows completed 1,200/1,200
  lifecycle operations with zero timeout/cleanup failures. Subsequent
  published chunks include atomic whole-file write `96a25f17`, lazy atime/EOF,
  content-addressed caching, metadata batching, cancellation-safe queueing,
  concurrent-create rebasing, cleanup correction, lease-renewal caching
  (`786408b3`, published in `f10dbf22`), publication-barrier optimization,
  bounded mutation collection, single-flight R2 upload coalescing and the
  PGlite/TiDB conditional publication fast path `214b9a6b` (published in
  `origin/main` `1626d538`). Retain the next terminal artifacts before
  resolving or qualifying any remaining performance blocker. Do not lower the
  threshold or convert failed rows to skips.
- [ ] P9/P12/P13: hand compatibility, CI evidence, customer incident scenarios
  and release inputs to the owning streams.
- [ ] P11: run every advertised Rust/Node/CLI/HTTP/native surface end to end
  through Ozone, retaining cross-workstream native blockers.
- [ ] P14: audit one retained CI revision and record W26 integration-ready or
  NO-GO before another stream promotes a release.

## Provisional remaining effort and blockers

| Category | Estimate | Notes |
| --- | --- | --- |
| Hosted result inspection | 0.5–1.5 d engineering plus external queue time | Historical green packet `35585066458` remains the last accepted hosted result. Terminal run `35683158821` is diagnostic: base Ozone passed, all four provider rows completed 1,200/1,200 operations with zero timeouts/cleanup failures but missed 1,000 IOPS at 754.59/817.10/143.04/345.17, and aggregate job `106606577835` failed closed. The next exact-SHA producer/aggregate packet requires terminal review after the next safe remediation. |
| Strict IOPS qualification integrity | 1–1.5 h engineering plus 0.75–1.5 h security/hosted review | The benchmark fails closed on missing requested providers and terminal run `35635486040` exercised all four configured rows without a skip. Its failed provider metrics remain diagnostic, not acceptance. |
| IOPS artifact/profile evidence gate | 1.5–2.5 h engineering/tests plus 0.75–1.25 h security/hosted review | Generic, TiDB and FoundationDB wrappers reject targets below 1,000 or weakened 4 KiB/400/concurrency-64 settings and validate retained JSON before pass markers. The terminal artifacts proved metric integrity but correctly failed provider performance; a passing rerun remains open. |
| IOPS artifact retention gate | 0.25–0.5 h engineering/local verification plus 0.5–1 h security/hosted review | The three W26 IOPS uploads use `if-no-files-found: error`; failed producer artifacts from `35635486040` were retained with IDs/digests for diagnosis, while successful acceptance retention remains open. |
| Credential-free production-config policy | 1.5–2.5 h implementation plus 0.5–1.5 h security/handoff review | Local all-provider positive fixtures and the insecure negative fixture pass the offline gate. Hosted CI, customer endpoint/IAM/TLS/rotation and runtime readback are separate gates. |
| Durable TiDB/Ozone remediation | 0 h for W26 acceptance | The restart isolation fix is validated by the Ozone-backed durable acceptance marker. Separate production/native/provider expansion would be a new scope. |
| Tracker/ledger publication | 0.5–1 h for this chunk | Includes focused security-diff review, current-head CI inspection, concurrent `origin/main` reconciliation, `git diff --check`, commit, push and remote verification. |
| External CI waiting | Unbounded wall-clock; not engineering time | GitHub runner queue, workflow concurrency, and concurrent pushes have repeatedly canceled otherwise useful runs. |
| Native/provider acceptance | Separate gate | Linux hosted NAPI/PGlite, TiDB/PD/TiKV, FoundationDB, and Ozone container behavior cannot be fully inferred from the local arm64 run. |
| Scope and provider-matrix definition (P0–P2) | 4–11 d engineering | Product targets are recorded; provider-specific support, platform scope and CI assertions remain to be defined and qualified. |
| Security, failure and integration CI (P3–P7, P10) | 14–36 d engineering | Requires secure CI fixtures where available, fault controls, redacted telemetry and security review; customer deployment remains external. |
| Performance and end-to-end matrix (P8, P11, P14, W26.15) | 12–32 d engineering | The workload and packet are implemented, but terminal diagnostic run `35683158821` failed the hard provider target at SQLite/PGlite/TiDB/FoundationDB 754.59/817.10/143.04/345.17 IOPS despite 1,200/1,200 successful lifecycle operations and zero timeout/cleanup failures. The PGlite/TiDB conditional publication fast path `214b9a6b` is locally verified and now has hosted results, but all providers remain below target. The next design must preserve POSIX/fencing/recovery semantics, then requires a fresh exact-SHA packet, possible provider-latency work or production-like Ozone capacity qualification, stable hosted CI runners, native runners and a one-revision audit. |
| Customer/Ozone and release handoff (P1, P6, P9, P12, P13) | 3–10 d W26 contract work | Ozone backup/DR, customer operations, deployment and release execution are external and not estimated as W26 implementation. |
| External CI waiting | Unbounded wall-clock; not engineering time | CI queues, provider image startup, fixture credentials and runner/platform availability remain elapsed gates. |
| **W26-owned production-readiness total** | **31–82 d engineering/contract work plus external waits** | Provisional planning range; no customer deployment or release commitment is implied. |

## Session time log

Times below are rounded, provisional engineering estimates for this W26
continuation. External CI queue and container startup time are recorded
separately because they are elapsed wall-clock, not implementation effort.

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-21 — Ozone baseline and immutable/fault gate | Ran the pinned Ozone gateway harness on arm64, diagnosed the loaded fault-window timeout, widened the bounded timeout, and verified cleanup. | ~1.5 h | ~0.25 h Docker startup | W26.1/W26.2 local evidence passed. |
| 2026-09-21 — SQLite/PGlite and Node/Rust CLI composition | Installed the locked PGlite dependencies, ran SQLite/PGlite composition, Node SDK matrix, Node CLI, and Rust CLI live coverage; added/verified CI wiring. | ~2 h | ~0.5 h image/build startup | W26.3a and W26.4 local evidence passed. |
| 2026-09-21 — TiDB single-node contract | Ran the single-node TiDB/Ozone provider, fencing, chunked-object, ambiguous-commit, and cleanup checks. | ~1 h | ~0.5 h TiDB startup | Single-node contract passed; replicated acceptance deliberately left open. |
| 2026-09-21 — FoundationDB durable topology | Added platform-specific pinned image selection, transaction readiness probing, and a three-node durable topology; ran the Ozone composition across a node restart and verified owned-resource cleanup. | ~2 h | ~1 h Docker recovery/startup | W26.3c local durable evidence passed. |
| 2026-09-21 — Hosted TiDB/Ozone diagnosis and recovery attempts | Inspected the first hosted failure, reordered the bounded recovery sequence (`67ca490`), configured TiDB restart readiness (`c0aa081`), and added a 30-second graceful frontend shutdown (`63dbdbd`). The terminal run `35581168122` still failed at the first TiDB frontend restart in both TiDB lanes after direct/provider/seed evidence passed. | ~1.5 h | ~3 h hosted queue/startup/restart timeout | Failure boundary is now stable and recorded; next bounded attempt isolates dropped-COMMIT cleanup from restart acceptance. |
| 2026-09-21 — Ledger preparation | Captured the current W26 inventory, evidence boundaries, provisional estimates, blockers, and remaining actions in this document. | ~0.25 h | 0 h | Ledger ready for its publication chunk. |
| 2026-09-21 — Ledger refresh and restart isolation | Updated this ledger and `WORK_TRACKER.md` with workflow `35581168122` and separated the intentional commit-drop test from the durable restart sequence in `scripts/test-tidb.sh`; the committed fix is `ecc1106` and is retained on `210c9cd`. | ~0.5 h | ~0.5 h log retrieval/review | The first rerun `35582936271` was canceled by the concurrent W05 push; replacement run `35583109781` is the current hosted acceptance gate. |
| 2026-09-21 — Hosted rerun reconciliation | Verified the canceled isolation run, fetched concurrent `origin/main` (`210c9cd`), and refreshed this ledger/tracker with the replacement W26 job IDs. | ~0.25 h | ~0.25 h hosted scheduling | W26 remained open until a terminal retained revision was available; no canceled result was promoted to evidence. |
| 2026-09-21 — Final hosted W26 acceptance | Reviewed run `35585066458` on `9c098e5`: hosted Ozone, SQLite/PGlite composition, Ozone-backed durable TiDB, and generic durable TiDB all passed their terminal jobs and redacted success markers. | ~0.25 h | ~1 h hosted queue/retries | W26 acceptance is complete within scope; unrelated native/RustFS jobs remain separate gates. |
| 2026-09-21 — W26 closeout publication | Fast-forwarded the combined tree and updated this ledger plus `WORK_TRACKER.md` with the final run, job IDs, completion percentages, evidence, boundaries, estimates, and session record. | ~0.5 h | ~0.25 h log retrieval | Closeout commit `b7e2758` was pushed to `origin/main`; no implementation action remains for W26. |
| 2026-09-21 — Production rollout tracking expansion | Added the separate P0–P14 production gate matrix, current NO-GO decision, implementation versus hosted/provider/native boundaries, provisional phase estimates, external blockers, open checklist and rollout evidence rules. | ~0.75 h | ~0.25 h remote reconciliation/push | W26 qualification remains accepted; production readiness is explicitly open and must be requalified on the retained implementation revision. |
| 2026-09-21 — Product production-scope decisions | Recorded customer-deployed Ozone ownership, all-feasible-provider intent, 1,000 IOPS per drive, 99.99% reliability, five-minute RPO/RTO, end-to-end/security scope, customer/Ozone DR ownership, separate release stream and CI-only testing. | ~0.5 h | 0 h | Reframed P0–P14 around W26 CI qualification and explicit external deployment/DR/release gates; production remains NO-GO until the CI packet is complete. |
| 2026-09-21 — Ozone IOPS CI gate | Extended the dependency-light storage benchmark with payload-size override, lifecycle IOPS measurement and hard minimum threshold; wired the 1,000-IOPS split-PGlite/R2 workload and artifact retention into `ozone-compositions`. | ~1.25 h | 0 h local; hosted CI pending | Benchmark unit tests and shell syntax checks passed. The live local gate is blocked by missing PGlite/N-API prerequisites; no performance pass is claimed until a terminal hosted job is reviewed. |
| 2026-09-21 — Ozone FoundationDB provider lane | Added a dedicated `ozone-foundationdb` hosted job for the durable three-node FoundationDB metadata composition over the live Ozone gateway; updated the Ozone test README, tracker and this ledger with the terminal-marker and evidence boundary. | ~0.75 h | ~0.25 h remote reconciliation; hosted result pending | Workflow and shell syntax are ready. The provider gate remains pending until a retained revision reaches terminal `FOUNDATIONDB_TEST_PASS ... service_restart=pass`, Ozone integration and cleanup. |
| 2026-09-21 — Ozone endpoint security boundary | Added static R2/Ozone endpoint validation for scheme, authority, credentials, query/fragment and control characters; added redaction-aware parser tests and an explicit CI invocation. | ~0.75 h | 0 h local; hosted CI pending | CLI configuration tests and workflow syntax are the local gate. Secure Ozone TLS/authentication and customer secret lifecycle remain separate hosted/customer gates. |
| 2026-09-21 — Plaintext remote endpoint guard | Restricted R2/Ozone HTTP endpoints to loopback and Docker test authorities in both the public R2 runtime config and the CLI parser; added tests for remote HTTP rejection while preserving local Ozone/RustFS fixtures. | ~0.75 h | 0 h local; hosted CI pending | R2 and CLI package tests are the local gate. This does not claim authenticated TLS or customer certificate/rotation acceptance. |
| 2026-09-21 — Ozone HTTP client/server end-to-end lane | Added the shipped `mount-rs serve-http` remote test to the Ozone composition job with binary/full/range I/O, cross-drive token rejection, graceful shutdown/reopen and an exact-run-prefix list/delete/re-list cleanup check. | ~1 h | 0 h local; hosted CI pending | Locked HTTP integration target compiled, shell/Node checks passed, and the unsafe-prefix cleanup guard failed closed. A terminal hosted marker is still required; loopback CI does not prove customer TLS, availability or native mounts. |
| 2026-09-21 — Local HTTP/Ozone execution boundary | Checked the local prerequisites for the new composition path. PGlite dependencies and the N-API artifact are present, but Docker cannot access `/var/run/docker.sock` in this environment. | ~0.1 h | 0 h test attempt; hosted CI required | Local live Ozone execution is explicitly blocked by the Docker daemon permission boundary, so no local live pass is claimed. The locked package and static harness checks remain green. |
| 2026-09-21 — HTTP security and observability qualification | Ran locked `mount-rs-http`, OTLP-enabled HTTP, full-feature observability, and CLI observability suites. | ~0.75 h | 0 h local; hosted collector/security review pending | Local auth/isolation, request/range bounds, lifecycle cleanup, telemetry redaction, local collector delivery and exporter-failure isolation passed. The evidence does not close deployed dashboards, secure customer Ozone auth/rotation, or the standard scan. |
| 2026-09-21 — HTTP production boundary hardening | Enforced loopback-only HTTP binds in the server and static CLI config, added configurable active-connection and total-request/header time bounds, and added stalled-body and excess-connection integration tests. | ~1.5 h | 0 h local; hosted/provider/security review pending | HTTP unit suite passed 8 tests, integration suite 8 tests, OTLP integration 9 tests, CLI suite 44 unit/9 CLI/2 subprocess/1 native-artifact tests, and workspace strict Clippy passed. Secure customer TLS/Ozone auth, provider matrix, hosted CI and final scan remain open. |
| 2026-09-21 — HTTP directory response bounds | Added configurable directory-entry and serialized-response limits to both `/entries` and directory-file routes, with fail-closed integration coverage for entry-count and byte limits. | ~1 h | 0 h local; hosted/provider/security review pending | HTTP unit suite passed 8 tests, default integration suite 10 tests, OTLP integration 11 tests, CLI suite 44 unit/9 CLI/2 subprocess/1 native-artifact tests, and strict workspace Clippy passed. The current core readdir API remains vector-based, so provider-side pagination/streaming and hosted Ozone evidence remain open. |
| 2026-09-21 — Ozone metadata-provider IOPS matrix | Added explicit SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 benchmark providers, configuration-gated availability, provider-specific durability labels and an Ozone IOPS invocation that applies the 1,000 target to every configured row. | ~1.5 h | ~0.1 h native build; live Ozone credentials/provider topologies unavailable locally | Benchmark unit tests, no-credential skip output, Node syntax checks and shell syntax passed. A rebuilt current N-API artifact passed the chunked lifecycle test. Hosted Ozone IOPS and provider-specific rows remain pending; absent TiDB/FoundationDB topologies are intentionally not treated as passes. |
| 2026-09-21 — Scoped immutable-block reconciliation | Added the explicit `BlockStore` reconciliation contract, lease-protected `ChunkedFs` root collection, R2/Ozone prefix-scoped aged-object deletion, default `ENOTSUP` behavior, Rust wrapper forwarding, N-API `reconcileBlocks` report/range contract and operator documentation. | ~2.5 h | ~0.1 h shared-target build/test wait; hosted/provider/collector/security gates pending | R2 14/14 tests, ChunkedFs 14/14 tests including committed/open-unlinked roots, SDK 2/2, observability 4/4, strict affected-package Clippy, formatting, diff checks and rebuilt N-API chunked integration passed. No provider-wide or production-retention pass is claimed. |
| 2026-09-21 — Reconciliation safety follow-up | Enforced positive grace at the `ChunkedFs` coordinator and changed R2/Ozone cleanup to consume the provider listing as a stream, avoiding whole-list materialization. | ~0.75 h | ~0.1 h shared-target compile/test wait; hosted/provider/collector/security gates pending | Locked R2 14/14 and ChunkedFs 14/14 tests, strict affected-package Clippy, formatting and diff checks passed. A fresh security review must target the final published revision; no provider-wide or production-retention pass is claimed. |
| 2026-09-21 — Bounded directory materialization hardening | Added `FsDriver::readdir_bounded`, made HTTP directory routes request the bound before serialization, implemented bounded enumeration for MemoryFs, HostFs, ChunkedFs and VersionedView, forwarded it through persisted/observability/CLI/native wrappers, and kept JavaScript-owned callbacks fail-closed. | ~2–3 h | ~0.25 h shared-target build/test wait; hosted CI run `35606250115` was superseded and is not evidence | HTTP 8 unit + 12 integration, core 11, host 5, chunked 14, CLI 44, observability 4 and persistence 4 local tests/targets passed; affected workspace check and formatting passed. The provider-specific follow-up is recorded below. |
| 2026-09-21 — Provider-bounded key enumeration and N-API bridge | Added the optional key-value provider contract, unstorage `getKeysBounded` callback bridge, public `Filesystem.readdirBounded` API, generated TypeScript declaration and Node error-shape wrapping. Legacy providers and structural JavaScript drivers fail closed when they cannot enforce the bound before materialization. | ~2.5–3 h | ~0.1 h host-process permission for N-API packaging; current CI run `35608567514` is non-terminal | KV integration tests 13/13, N-API Rust tests 16/16, strict affected-package Clippy, formatting/diff checks, release N-API build, unstorage bridge, typecheck and chunked smoke passed. MOUNTX_SOURCE parity was explicitly skipped because the source is unset; hosted provider/security gates remain open. |
| 2026-09-22 — Bounded unstorage parity fixtures | Added explicit capable-provider overflow/success coverage and a legacy-provider fail-closed `ENOTSUP` check to the unstorage/N-API qualification tests. | ~0.25–0.5 h | 0 h local; `MOUNTX_SOURCE` remains an external/source-gated parity check | `node integrations/mount-rs-napi/test/unstorage.mjs` passed, including capable and legacy fixtures; syntax checks and `git diff --check` passed. The raw provider parity harness now covers bounded overflow/success, but the full oracle comparison remains skipped when `MOUNTX_SOURCE` is unset. |
| 2026-09-22 — Ozone provider-backed bounded listing qualification | Added real Ozone composition assertions for bounded directory success and `EOVERFLOW` across the SQLite/R2 and PGlite/R2 `ChunkedFs` lanes, plus the standalone harness lockfile entry required by the merged R2 dependency graph. | ~0.75–1 h | 0 h local service execution; hosted Ozone composition and provider startup remain external | Locked Ozone harness compile, test discovery, strict all-target Clippy, formatting, shell syntax and diff checks passed. The ignored live tests are visible and ready, but Docker/Ozone was not available locally, so no live provider pass is claimed. |
| 2026-09-22 — Exact bounded-provider limit correction | Corrected the KV adapter to pass the caller's exact `maxEntries` to the provider (the provider may return one extra key as its overflow signal) and added a regression that records limits `[2, 3]`. | ~0.5–0.75 h | 0 h local; hosted CI runs `35611505333`/`35611505169`/`35611505545` are non-terminal | KV integration tests 13/13, strict affected-package Clippy, formatting and diff checks passed. The fix is published on `73cdc93`; no hosted result is promoted. |
| 2026-09-22 — Current-tip security review and remote reconciliation | Fast-forwarded concurrent `origin/main` changes through `3fca802`, reviewed the SDK credential-redaction and FoundationDB workflow deltas, and completed the current-revision standard security scan. | ~1.25–1.75 h | ~0.25 h remote refresh; hosted CI runs `35612976567`/`35612976712`/`35612976618`/`35612976513` are non-terminal | Scan `70af8d4e-9fb3-4d2c-b688-54a9b6535739` completed with zero reportable local findings across 16 surfaces. It is an intentionally partial production packet: customer Ozone TLS/IAM/rotation, provider-native bounded allocation, dependency provenance, 99.99%/recovery drills and operations remain deferred. |
| 2026-09-22 — Durable-provider bounded-listing extension | Added provider-backed bounded success/overflow assertions to the TiDB/RustFS and FoundationDB/RustFS composition seed/reopen paths, then compiled and linted the affected targets. | ~1.5–2 h | ~0.5 h provider/native compile gate; no live services available locally | Commit `d1c9e44`, published at `44b01a7`: TiDB locked test compilation and strict Clippy passed; FoundationDB `cargo check --tests` passed, while native test-binary linking is blocked by missing `libfdb_c`. No hosted Ozone/provider marker is promoted. |
| 2026-09-22 — FoundationDB Ozone Node/N-API bounded surface | Added the feature-built Node/N-API bounded success/overflow assertions, made the test prefix inherit the owned Ozone scope, and enabled the Node lane in the dedicated durable Ozone workflow. | ~0.75–1 h | ~0.25 h hosted workflow startup; feature-enabled native execution remains hosted | Commit `b80c19c`, published at `0842474`: Node syntax and shell syntax checks passed; the local non-feature invocation skipped safely. The hosted job must emit `FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS` for seed/reopen plus restart and cleanup markers before this provider surface is accepted. |
| 2026-09-22 — TiDB Ozone Node/N-API bounded surface | Added the feature-built public Node/N-API TiDB seed/reopen test, bounded success/overflow markers, Ozone-scoped block prefix and the dedicated workflow build/invocation. | ~1–1.5 h | ~0.25 h hosted workflow startup; TiDB/N-API/Ozone execution remains hosted | Commit `ef6a876`, published at `43f8df2`: shell, JavaScript, YAML and diff checks passed; the local feature-disabled invocation skipped safely. The hosted job must emit `TIDB_NAPI_BOUNDED_READDIR_PASS` for seed/reopen plus the durable restart, Ozone integration and cleanup markers before the extension is accepted. |
| 2026-09-22 — Published-tip security scan and CI reconciliation | Ran the standard security scan against the published durable-provider revision and reconciled the current-head hosted workflow states. | ~1.25–1.75 h | ~0.5–1 h scan/hosted status wait | Scan `5ad61e60-20e3-4223-885a-d4b516d49bb1` completed with zero reportable findings across 16 W26-relevant surfaces, but semantic coverage is explicitly partial and independent workers did not return within bounded waits. Current `44b01a7` CI/Fault/W08 runs are canceled and Live Cloudflare R2 failed; none is evidence. |
| 2026-09-22 — Durable-provider Ozone IOPS qualification wiring | Added early positive-integer validation, direct TiDB Node benchmark execution, FoundationDB container-side benchmark execution with a writable owned artifact volume, retained JSON copy-out, provider-specific pass markers and always-upload workflow steps for dedicated durable-provider Ozone jobs. | ~1.5–2.25 h | ~0.25 h remote reconciliation; hosted provider startup and terminal artifacts pending | Code commit `de9d267`, published in reconciled tip `414a469`. Shell syntax, benchmark syntax/unit tests, YAML parse and diff checks passed. The current tip's CI run `35619958098` was canceled before jobs started and Live Cloudflare R2 run `35619958070` failed; no performance result is promoted. |
| 2026-09-22 — Focused security-diff review of IOPS harness | Reviewed the durable-provider IOPS shell/workflow surfaces for command injection, credential exposure, path escape and false pass markers; completed the prompt-driven diff scan. | ~0.75–1.25 h | 0 h local; hosted/customer control validation deferred | Scan `5fc5a943-07bb-4979-9f37-efd87a7f505e` completed with zero reportable findings. Coverage is explicitly partial: hosted credential/isolation, provider TLS/IAM/rotation, retained artifact access and live provider execution remain open gates. |
| 2026-09-22 — Credential-free Ozone production-config policy | Added `scripts/verify-w26-ozone-production-config.mjs` and SQLite/PGlite/TiDB/FoundationDB positive fixtures plus an insecure HTTP negative fixture; wired the policy into the `ozone` CI job without provider connections or credentials. | ~1.5–2.5 h | ~0.25 h remote reconciliation; hosted CI and customer security evidence pending | Local `node --check`, all four positive markers, negative rejection, YAML parse and diff checks passed. Commit `b8d8fb1` was merged with concurrent mainline changes and published at `4d8b5fd`; current CI `35622054417` is canceled and Live R2 `35622054352` failed, so no hosted result is promoted. |
| 2026-09-22 — Focused security-diff review of production-config policy | Reviewed parser/resource safety, inline-secret rejection, HTTPS/TiDB TLS enforcement, provider allowlists, CI secret exposure and the negative fixture; completed the prompt-driven diff scan. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `d74e3e86-2e0a-45cf-9819-e31f428eb5d4` completed with zero reportable findings. Coverage is explicit about external Ozone IAM/TLS/rotation, provider-native security, hosted execution and operational SLO/recovery gates. |
| 2026-09-22 — Expanded Ozone security negative paths | Added independent inline-secret, FoundationDB lease-authority and TiDB TLS-verification negative fixtures and explicit CI rejection assertions. | ~0.75–1.25 h | ~0.25 h remote reconciliation; hosted CI pending | Commit `7018b59` was merged with concurrent mainline changes and published at `6a1b94b`; four positive and four negative local policy checks, YAML parsing, syntax and diff checks passed. |
| 2026-09-22 — Focused security-diff review of expanded policy | Reviewed the expanded workflow rejection assertions, fixture credential-like values and environment-only TiDB TLS override; completed the prompt-driven diff scan. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `7addeeb5-4601-4951-aca9-becffb9bd4b9` completed with zero reportable findings. Changed-fixture source inventory was empty because only workflow/JSON changed, while all changed artifacts were manually inspected; hosted/provider coverage remains deferred. |
| 2026-09-22 — Strict provider-configuration IOPS qualification | Added `--require-configured`, provider-scoped generic Ozone IOPS selection and strict TiDB/FoundationDB invocations so a skipped requested provider cannot produce a qualification pass. | ~1–1.5 h | ~0.25 h remote reconciliation; hosted provider execution pending | Commit `d46e091` was merged with concurrent mainline changes and published at `74fe4c7`. Benchmark unit tests, missing-provider regression, Node/shell syntax, YAML parsing and diff checks passed locally. |
| 2026-09-22 — Focused security-diff review of strict IOPS qualification | Reviewed strict status aggregation, missing-configuration diagnostics, provider-list environment flow, shell command construction, artifact paths and the workflow environment. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `60269206-bb22-4b78-aaf7-f05d16ffcca0` completed with zero reportable findings across five source surfaces; changed workflow/documentation files were also manually reviewed. Hosted runner isolation, provider TLS/IAM/rotation and live performance remain deferred. |
| 2026-09-22 — IOPS artifact/profile integrity gate | Added the credential-free artifact verifier, fixed 4 KiB/400-iteration/concurrency-64 qualification profile, target floor, exact provider-set check, cleanup/lifecycle assertions and wrapper validation before pass markers. | ~1.5–2.5 h | ~0.25 h remote reconciliation; hosted provider execution pending | Commit `66f3670` was merged with concurrent mainline changes and published at `00d2b80`. Benchmark unit tests, verifier negative cases, Node/shell syntax and diff checks passed locally; no live Ozone/provider result is claimed. |
| 2026-09-22 — Focused security-diff review of IOPS artifact/profile gate | Reviewed untrusted JSON handling, regular-file/symlink policy, exact provider/profile checks, shell environment flow, output paths, marker ordering and secret exposure. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `1d97028f-e153-4b49-9fac-c3a8c1fc1117` completed with zero reportable findings across five changed source surfaces. Hosted artifact access, provider TLS/IAM/rotation, customer capacity and SLO/recovery controls remain deferred. |
| 2026-09-22 — Fail-closed W26 IOPS artifact retention | Changed the generic, TiDB and FoundationDB Ozone IOPS upload steps to fail when an expected JSON artifact is absent. | ~0.25–0.5 h | ~0.25 h remote reconciliation; hosted run pending | Commit `c0f8370` was merged with concurrent mainline changes and published at `12ba117`. CI YAML parsing, shell syntax, benchmark unit tests and diff checks passed locally; no hosted artifact result is promoted. |
| 2026-09-22 — Focused security-diff review of artifact retention | Reviewed workflow path scope, upload behavior, secret exposure, evidence-integrity bypasses and interaction with the `always()` upload steps. | ~0.5–1 h | 0 h hosted; artifact service/customer controls deferred | Scan `18010cad-ed69-4da3-b0a9-57163091e878` completed with zero reportable findings. Hosted artifact authorization/retention and provider/customer controls remain deferred. |
| 2026-09-22 — Mainline reconciliation after security-ledger publication | Merged concurrent origin/main changes after publishing the security chunk and refreshed the exact hosted workflow state for the merged revision. | ~0.25 h | ~0.25 h remote fetch/merge and hosted status lookup | Merge tip `9b3b90b` is verified on `origin/main`; CI `35624670863` and W08 release runs `35624670789`/`35624670708` are pending, while Fault injection `35624670707` is queued. No current-tip W26 result is promoted. |
| 2026-09-22 — Concurrent FUSE/mainline reconciliation | Merged the next concurrent mainline update after the ledger publication and refreshed hosted state again so the ledger remains tied to the actual remote tip. | ~0.25 h | ~0.25 h remote fetch/merge and hosted status lookup | Merge tip `3681f27` is verified on `origin/main`; CI `35624846453` and W08 release runs `35624846413`/`35624846541` are pending, while Fault injection `35624846354` is in progress. No current-tip W26 result is promoted. |
| 2026-09-22 — One-revision W26 Ozone evidence packet | Added the aggregate packet verifier, retained policy/base/provider logs, provider JSON artifact paths, explicit negative-policy markers and the fail-closed `w26-ozone-evidence` CI job. | ~2–3 h | ~0.5 h remote reconciliation; current CI `35630094815` pending and Fault injection `35630094834` in progress | Local benchmark/evidence unit tests, Node/shell syntax, YAML parse and diff checks passed. Code commit `08f4530` was merged with concurrent mainline changes and published at `097ed00`; the aggregate hosted packet is not yet terminal evidence. |
| 2026-09-22 — Focused security-diff review of evidence packet | Reviewed retained-log handling, artifact substitution/cross-revision checks, marker aggregation, shell pipelines and secret exposure. | ~0.75–1.25 h | 0 h hosted; provider/customer controls deferred | Scan `c67ae8e0-99af-4ade-8284-a612d599b5e4` completed with zero reportable findings across the changed verifier/test surfaces. Hosted runner/artifact authorization, provider TLS/IAM and customer Ozone controls remain external. |
| 2026-09-22 — W26 ledger publication for evidence packet | Refreshed the current remote/hosted status, updated W26.11, P8/P10 percentages, production NO-GO boundaries, remaining actions, estimates and session log. | ~0.5–1 h | ~0.25 h remote fetch/merge/push | Documentation is being published as a separate chunk after the implementation commit; no hosted result is promoted from pending/in-progress state. |
| 2026-09-22 — IOPS metric and payload-map integrity | Extended the W26 artifact verifier and synthetic tests to reject incomplete lifecycle counters, non-finite percentiles, timeout/cleanup failures and payload-size mismatches. | ~0.75–1.25 h | ~0.25 h remote reconciliation; hosted provider result pending | Commit `e875ba6` was merged with concurrent mainline changes and published at `1775895`. Benchmark/evidence unit tests, Node/shell syntax, YAML parse and diff checks passed; no hosted performance result is promoted. |
| 2026-09-22 — Focused security-diff review of metric integrity | Reviewed untrusted numeric fields, false pass conditions, sample-count consistency and artifact-content handling. | ~0.75–1.25 h | 0 h hosted; provider/customer controls deferred | Scan `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564` completed with zero reportable findings across the verifier/test surfaces. |
| 2026-09-22 — Credential-free Ozone production-rollout contract | Added the machine-checked customer handoff contract for the Tier-1 envelope, secure Ozone topology, tenant scope, all four metadata providers, operations controls and customer-owned backup/restore boundary; added positive, weak-RTO and inline-secret fixtures, CI policy markers and aggregate packet requirements. | ~1.5–2.5 h | ~0.25 h remote reconciliation; hosted/provider/customer gates pending | Local valid/negative contract checks, benchmark/evidence tests, Node syntax, YAML parse and diff checks passed. Commit `8d2cfbb` was reconciled and published at `0398d94`; the validator emits declaration-only evidence and does not connect to Ozone. |
| 2026-09-22 — Focused security-diff review of rollout contract | Reviewed untrusted JSON handling, exact external secret references, tenant prefix constraints, topology/SLO enforcement, CI marker flow and the declaration-only boundary. | ~0.75–1.25 h | 0 h local; hosted/customer controls deferred | Scan `33c09a35-77f5-417c-862c-e5848185f50e` completed with zero reportable findings. Coverage explicitly defers customer IAM/certificates/rotation, hosted provider execution, artifact authorization and measured availability/RPO/RTO. |
| 2026-09-22 — Complete end-to-end packet surface enforcement | Expanded the aggregate packet to require gateway lifecycle, SQLite/PGlite bounded composition, Rust/Node/remote-HTTP CLI, TiDB N-API seed/reopen, FoundationDB N-API seed/reopen and provider restart markers; added a fail-closed synthetic missing-marker test. | ~0.75–1.25 h | ~0.25 h local test/security-scan time; hosted provider execution pending | Local benchmark/evidence tests, Node/shell syntax, YAML parse and diff checks passed. No hosted/provider/native/customer result is promoted. |
| 2026-09-22 — Focused security review of complete-surface packet | Reviewed untrusted retained logs/artifacts, marker substitution/omission, source-revision binding, cross-provider evidence mixing and secret exposure in the validator/test changes. | ~0.5–1 h | 0 h hosted; customer/provider controls deferred | Scan `e4ce1aab-c6cd-44e4-b20d-3130ca357412` completed with zero reportable findings; delegated workers were unavailable and the parent reviewed both changed files. |
| 2026-09-22 — Complete-surface packet publication | Committed the validator/test chunk as `20a06b8`, reconciled concurrent `origin/main` changes and pushed the merged tip `0e0454d`. | ~0.25–0.5 h | ~0.25 h remote fetch/merge/push; post-push CI pending | Other threads can now build on the expanded packet contract; a terminal same-revision aggregate run is still required. |
| 2026-09-22 — Rollout-contract documentation publication | Added `docs/w26-production-rollout.md`, updated W26.13, refreshed the production-gate contract subledger, estimates, checklist and current NO-GO boundaries. | ~0.75–1.25 h | ~0.25 h remote reconciliation/push; post-push CI pending | Documentation is being published as a separate chunk after the implementation commit; no declaration, pending job or canceled job is promoted to production evidence. |
| 2026-09-22 — Full locked workspace regression gate | Re-ran `./scripts/cargo-shared test --workspace --all-targets --locked` with the repository-shared Cargo target after concurrent mainline changes. | ~0.25–0.5 h | 0 h hosted; local shared-target permission required | Exit code 0. All runnable tests passed; explicitly environment-gated native/provider tests remained ignored and are recorded as external gates rather than promoted to passes. |
| 2026-09-22 — Revision-bound hosted W26 qualification dispatch | Inspected manual workflow-dispatch run `35635486040` on `cfe7e29e001dc01f2fa430a54bc8981b44db0b05` and compared the W26 verifier/benchmark/Ozone workflow paths with current `origin/main` `ce7b365a`. | ~0.25–0.5 h | In progress; GitHub-hosted runner/provider startup is external | `ozone` `106451808629`, `ozone-compositions` `106451808739` and `ozone-tidb` `106451809745` were in progress; `ozone-foundationdb` `106451808796` was queued. No packet result is promoted until all producer jobs and `w26-ozone-evidence` are terminally successful on one revision. |
| 2026-09-22 — Complete-surface README and ledger refresh | Documented the aggregate packet's complete end-to-end marker contract and updated the ledger with the full workspace test result, current remote tip and active run IDs. | ~0.5–1 h | ~0.25 h remote reconciliation/push; hosted run remains external | This documentation chunk is being committed and pushed separately so other threads can consume the current evidence boundary; production remains NO-GO. |
| 2026-09-22 — Terminal hosted Ozone packet review and performance diagnosis | Retrieved the terminal producer and aggregate logs/artifacts for run `35635486040`, reconciled the exact provider metrics, and traced the common throughput ceiling to `ChunkedFs`'s volume-wide async gate spanning remote block I/O and whole-namespace publication. | ~1.25–2 h | ~0.5 h hosted log/artifact retrieval and provider startup already elapsed | All four configured rows completed 1,200/1,200 lifecycle operations with zero timeouts/cleanup failures but measured 61.97/63.56/14.14/23.07 IOPS; the aggregate failed closed. W26.15 is now the explicit performance-remediation/qualification item; the hard threshold is unchanged. |
| 2026-09-22 — Production ledger correction and tracker expansion | Updated this ledger and `WORK_TRACKER.md` so the terminal failure, exact artifacts, W26.15 remediation path, provisional estimates, external gates and session time are current. | ~0.75–1.25 h | ~0.25 h remote reconciliation/push | Documentation-only chunk; `git diff --check`, benchmark/unit evidence and the prior full locked workspace test remain the supporting local evidence. A commit and remote verification follow this edit. |
| 2026-09-22 — Optimistic ChunkedFs write-path remediation | Implemented optimistic immutable-block write/revision publication with serialized conflict fallback and a lifecycle barrier so shutdown cannot fence an in-flight write before commit. | ~2–4 h | ~0.5 h local regression/full-workspace verification; ~0.25 h remote reconciliation/push | Implementation commit `a2075af0` was reconciled and published at `cd6dea28`; the first terminal hosted qualification remained below target and motivated the read-path follow-up. No threshold was lowered. |
| 2026-09-22 — Optimistic ChunkedFs read-path remediation | Moved remote block reads outside the volume-wide metadata gate; atime publication now uses a revision/base check and safe orphan handling, while the lifecycle barrier covers shutdown. Added blocked-read gate-release/shutdown and stale-read-versus-concurrent-write regressions. | ~1.5–3 h | 8 concurrency tests, 14 chunked unit tests, full locked workspace tests, strict workspace Clippy, formatting and diff checks passed; hosted result later failed the hard target | Implementation commit `c4378dc1` was reconciled with concurrent mainline changes and pushed in merged tip `88b707ba`. The dispatched run `35641941218` later became terminal for W26 and is recorded above as diagnostic-only performance evidence. |
| 2026-09-22 — Focused security-diff review of optimistic read/write path | Reviewed lease fencing, revision CAS, immutable ordering, shutdown/cancellation, stale publication, orphan cleanup and confidentiality boundaries for `integrations/mount-rs-chunked/src/lib.rs`. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `e7c03690-856b-42ae-8d47-8af0dc788a54` completed with zero reportable findings. Coverage is partial by design and explicitly defers hosted/provider/customer TLS/IAM, 1,000-IOPS capacity, 99.99% reliability, RPO/RTO, Ozone backup/DR and release gates. |
| 2026-09-22 — Replacement hosted W26 packet dispatch | Dispatched CI workflow `35641941218` against tested W26 code head `88b707ba` after the write/read concurrency remediation. | ~0.25 h | W26 producer and aggregate jobs later became terminal; the overall workflow remains active only for unrelated native jobs | Review the retained provider artifacts and aggregate result; preserve the hard 1,000-IOPS gate and update this ledger only from terminal evidence. |
| 2026-09-22 — Published-tip full Rust regression gate | Re-ran the full locked workspace tests and strict Clippy after the read-path chunk and publication reconciliation. | ~0.5–1 h | `./scripts/cargo-shared test --workspace --all-targets --locked` exited 0; `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings` exited 0; native/provider tests that require unavailable environments remain explicit skips | Hosted provider/native/customer gates remain open; no local test result is promoted to Ozone production acceptance. |
| 2026-09-22 — Terminal replacement hosted W26 packet review | Retrieved the exact artifacts for run `35641941218` and reviewed jobs `106473112763`, `106473112920`, `106473112915`, `106473112409` and aggregate `106477164582`. | ~0.75–1.25 h | Hosted artifact retrieval and unrelated workflow completion remain external; parent workflow is still `in_progress` for native jobs | All four provider rows recorded 1,200/1,200 successful lifecycle operations with zero timeout/cleanup failures, but measured 85.70/94.04/14.50/30.46 IOPS and failed `IOPS_TARGET_NOT_MET`; aggregate failed closed. W26.15 remains open. |
| 2026-09-22 — W26 terminal-result ledger refresh | Updated the snapshot, diagnosis, provider work items, P8/W26.15 checklist, estimates and current-status override with the replacement run's exact terminal evidence and current `origin/main` base `76450474`. | ~0.75–1.25 h | ~0.25 h concurrent-mainline reconciliation and status lookup; no staging environment exists | The first overlap remediation is a measured improvement but not a qualification pass. The next chunk must address metadata publication/provider latency or obtain a production-like Ozone capacity qualification; threshold remains 1,000. |
| 2026-09-22 — Atomic whole-file write implementation | Added the public `FsDriver::write_file` contract, preserved the generic open/write/close fallback, routed N-API `writeFile` through it, and added the `ChunkedFs` optimistic block-stage/flush/one-publication path with serialized conflict fallback and reopen/content regression. | ~2–3 h | ~0.75 h shared-target wait; no hosted result yet | Commit `96a25f17` was reconciled with concurrent mainline work and pushed at `92f900be`. Focused ChunkedFs tests passed 15/15, N-API check and storage benchmark units passed, full locked workspace tests and strict workspace Clippy exited 0, and formatting/diff checks passed. |
| 2026-09-22 — Atomic-write security and publication checkpoint | Completed the prompt-driven security diff review, committed the code, fetched/merged concurrent `origin/main`, pushed `origin/main` and verified the exact remote revision. | ~0.5–0.75 h | ~0.25 h remote fetch/merge/push; hosted qualification pending | Scan `11fa7e54-bb66-488d-a369-c1a7ac46da81` completed with zero reportable findings across the three changed source files; hosted/provider/customer security and performance remain deferred. Remote verification returned `92f900bea92e89002de5696aa9cb2db473e7b1ae` for both `HEAD` and `origin/main`. |
| 2026-09-22 — Atomic-write ledger publication | Refreshed this ledger and `WORK_TRACKER.md` with the new implementation commit, exact local gates, security receipt, current NO-GO decision, provisional estimates, external blockers and the required next hosted packet. | ~0.5–1 h | ~0.25 h remote reconciliation/push; CI queue/provider startup external | Documentation is published separately so other threads can see the new workstream state; no current-tip hosted result is promoted until all W26 producer and aggregate jobs are terminal. |
| 2026-09-22 — Current-tip atomic-write hosted packet review | Retrieved artifacts for run `35649202405`, confirmed terminal W26 jobs `106497104104`, `106497104006`, `106497104443`, `106497103955` and aggregate `106501216372`, and inspected the aggregate failure log. | ~0.75–1.25 h | Hosted runner/provider startup and artifact service were external elapsed gates | Base Ozone passed; SQLite/PGlite/TiDB/FoundationDB completed 1,200/1,200 operations with zero timeout/cleanup failures but measured 91.94/95.82/16.10/34.97 IOPS; the aggregate failed closed on the missing `OZONE_IOPS_PASS` marker. W26.15 remains open. |
| 2026-09-22 — Current-tip hosted evidence ledger publication | Updated the snapshot, hosted diagnosis, every affected provider/work item, P8/P14, provisional estimates and session time with run `35649202405` and artifact IDs; preserved the NO-GO and external customer/Ozone gates. | ~0.5–1 h | ~0.25 h concurrent-mainline reconciliation/push; no staging environment | Documentation chunk is ready to commit/push before the next implementation chunk; no failed provider result is promoted. |
| 2026-09-22 — Lazy-atime and EOF-read performance remediation | Added coordinator-local pending-atime state, stat overlay, EOF early return and explicit sync/shutdown flush so ordinary reads do not publish a remote namespace revision per operation. Added the atime-coalescing/EOF regression. | ~1.5–3 h | ~4.5 h shared-target rebuild/test wait; no hosted result yet | Commit `d1bc8fb9` passed formatting, the full locked workspace test suite and strict workspace Clippy; the changed ChunkedFs suite passed 16/16. The intentional atime crash boundary remains documented and does not change data durability. |
| 2026-09-22 — Lazy-atime focused security-diff review | Reviewed pending-atime ownership, stat visibility, namespace snapshot/publication, lease/revision fencing, immutable ordering, orphan handling, sync/shutdown durability and EOF/error paths. | ~0.75–1.25 h | 0 h hosted; provider/customer security deferred | Scan `5c0c59fb-a2c6-4e03-be6c-c334ca7cf0e7` completed and sealed with zero reportable findings. Coverage is partial by design and explicitly defers the atime process-crash boundary plus hosted/provider/customer TLS/IAM, capacity, SLO/RPO/RTO, backup/DR, native and release gates. |
| 2026-09-22 — Lazy-atime implementation publication | Committed, fetched concurrent mainline changes, merged two advancing remote tips after one non-fast-forward rejection, and pushed the implementation chunk to `origin/main`. | ~0.5–0.75 h | ~0.5 h remote fetch/merge/push; hosted CI pending | Implementation commit `d1bc8fb9` is included in merged remote tip `6429c7ba`; `HEAD` and `origin/main` match exactly. No hosted result is promoted before a fresh one-revision packet. |
| 2026-09-22 — Ledger refresh before current-tip qualification | Refreshed this ledger and `WORK_TRACKER.md` with the published lazy-atime/EOF code, exact local gates, security receipt, updated W26.15/P8/P10/P14 percentages, provisional estimates, session time and external blockers. | ~0.5–1 h | ~0.25 h remote reconciliation/push; hosted runner/provider startup external | Documentation is the next separately published chunk; the next CI packet must run against `6429c7ba` and remain NO-GO until every provider and aggregate gate is terminally successful. |
| 2026-09-22 — Content-addressed R2 block/cache chunk | Changed immutable R2 block IDs to content-addressed keys and added a bounded shared process-local cache to avoid repeated transfer of identical benchmark blocks while preserving block verification and provider cleanup boundaries. | ~1.5–3 h | Focused storage/ChunkedFs tests, full locked workspace tests and strict Clippy passed before the following batching chunk; hosted qualification remained open | Implementation commit was reconciled and published in remote tip `e11b7826aa866243c5d8d378bb9438b3ed74e81b`; no hosted result is promoted from this chunk alone. |
| 2026-09-22 — Metadata mutation batching chunk | Batched eligible whole-file and unlink metadata mutations into one lease-fenced namespace publication while retaining conflict fallback and the durable success acknowledgement boundary. | ~2–4 h | Focused concurrent mutation test proved four whole-file mutations share one metadata publication; full locked workspace tests, strict Clippy and diff checks passed | Commit `c4e9a253` was reconciled and published through remote tip `a1cb4ca92bd0c1b619c56231002ee182852b01a8`; hosted Ozone performance and provider security remain open. |
| 2026-09-22 — Mutation queue cancellation hardening | Bounded pending mutation admission at 1,024, added runner-drop cleanup that replies `EIO`, and skipped queued mutations whose reply channel closed before namespace application. | ~1–2 h | Focused ChunkedFs suite passed 18/18, including canceled-runner recovery; full locked workspace tests, strict Clippy and diff checks passed. Security scan `6d5a7665-d8cd-46b7-a9bd-73fec54f5431` completed with zero reportable findings and explicit hosted/customer exclusions. | Commit `d152fa1a` was published after concurrent-mainline reconciliation; current exact `origin/main` is `5d53f08a289c0ba0662c29e6070f9519b47e19a3`. The strict 1,000-IOPS gate is still open. |
| 2026-09-22 — Current-tip manual hosted qualification dispatch | Dispatched the non-canceling CI workflow for the reconciled current mainline after a push-triggered run was superseded by concurrent origin advances. | ~0.25 h | Manual run `35661836627` targets exact SHA `5d53f08a289c0ba0662c29e6070f9519b47e19a3`; W26 producer jobs are queued as `106538643680`, `106538643693`, `106538643743` and `106538643926`. | Wait for terminal producer and aggregate results, download all retained artifacts, and keep production **NO-GO** unless every configured provider and the complete end-to-end packet passes. |
| 2026-09-22 — Ozone content-addressed test-contract correction | Corrected `tests/ozone/src/lib.rs` so identical immutable bytes reuse their content-addressed ID and different bytes receive a different ID; retained range-read, missing-object, conditional-create, stale-ETag, fault/restart and cleanup assertions. | ~0.5–1 h | Ozone test crate compile-check passed; security scan `46d4cf32-9d57-4f7e-975f-2616ddc53dc5` completed with zero reportable findings. The prior FoundationDB producer failure was diagnosed as this stale assertion before benchmarking. | Commit `edb6a43e` was reconciled and published in current remote tip `07e5255a86d82805864a0dea36fbd13cb63b7944`; a fresh full provider packet is required. |
| 2026-09-22 — Corrected-tip manual hosted qualification dispatch | Dispatched a new non-canceling CI matrix after the stale Ozone contract test was fixed and pushed. | ~0.25 h | Manual run `35663517544` targets exact SHA `07e5255a86d82805864a0dea36fbd13cb63b7944`; producer jobs are queued as `106543976811` (`ozone`), `106543976622` (`ozone-compositions`), `106543976840` (`ozone-tidb`) and `106543976753` (`ozone-foundationdb`). | Wait for terminal producer and aggregate results; keep production **NO-GO** unless every configured provider, all end-to-end markers and the hard IOPS threshold pass. |
| 2026-09-22 — Content-addressed cleanup correction | Updated the real-Ozone contract cleanup to delete identical content-addressed IDs once while retaining every distinct concurrent ID. | ~0.25–0.5 h | Ozone test crate `cargo check --tests --locked` passed; no local live Ozone service was available | The correction is commit `0282df64`, published through reconciled `origin/main` tip `229a9cd5`; it addresses the FoundationDB rerun's post-contract `ENOENT` cleanup failure without weakening the contract assertions. |
| 2026-09-22 — Cleanup correction security review | Completed the focused prompt-driven security diff review for the cleanup deduplication. | ~0.5–0.75 h | 0 h hosted; customer/provider controls deferred | Scan `85c31e86-1fc8-4a6d-bb95-ef2d956b9ed8` completed with zero reportable findings. Coverage confirmed identical-ID deduplication, distinct-ID cleanup, owned test scope and preservation of immutable/collision/conditional/missing/restart/prefix safeguards; hosted Ozone TLS/IAM, capacity, SLO/RPO/RTO, backup/DR, native and release gates remain external. |
| 2026-09-22 — Cleanup correction publication | Committed the cleanup fix, reconciled concurrent `origin/main` changes after a non-fast-forward push rejection, and verified remote alignment. | ~0.5–0.75 h | ~0.5 h remote fetch/merge/push | Commit `0282df64` is included in `229a9cd5`; `HEAD` and `origin/main` match, and the worktree is clean before hosted dispatch. |
| 2026-09-22 — Concurrent whole-file create batching fix | Rebased eligible new-file mutations onto unique current namespace inodes within one fenced publication while preserving stale-existing-file conflict behavior; added a four-concurrent-create one-publication regression. | ~1–2 h | Focused ChunkedFs tests, full 19-test ChunkedFs suite, full locked workspace tests, strict workspace Clippy and diff checks passed. Commit `d3d6299e` was published through reconciled tip `059f801e` and is included in current `origin/main` descendants. | Hosted provider performance and full packet remain open; keep the fixed 1,000-IOPS target and existing fencing/CAS semantics. |
| 2026-09-22 — Concurrent-create performance security review | Completed the focused prompt-driven security review for the mutation rebasing change. | ~0.5–0.75 h | Scan `b8f6e846-e497-4ae6-b972-7ae3701ec722` completed with zero reportable findings across the changed ChunkedFs surface; hosted provider/customer controls remain deferred. | Retain explicit hosted Ozone TLS/IAM, capacity, SLO/RPO/RTO, native and release boundaries. |
| 2026-09-22 — Concurrent-create publication and shared-mainline reconciliation | Published the code chunk and reconciled later concurrent mainline commits so the shared branch remains the build-on base. | ~0.5–0.75 h | Code commit `d3d6299e` is included through `059f801e`; this checkout was fast-forwarded to current `origin/main` `3c884bd8`, which also contains concurrent 9P parity changes. | Keep documentation commits based on the newest shared tip and push each subsequent ledger/tracker chunk. |
| 2026-09-22 — Concurrent-create hosted qualification dispatch | Dispatched the current W26 matrix after publishing the performance chunk. | ~0.25 h | Hosted run `35665734382` selected exact SHA `8c53d2c926137fba6034bf0edcb662999c71d426`; jobs at latest poll: `106550958697` FoundationDB queued, `106550958843` compositions queued, `106550958877` TiDB in progress and `106550958998` base Ozone queued; aggregate job not yet present. | Await terminal producer and aggregate output; production remains **NO-GO** unless every configured provider, all end-to-end markers and the hard IOPS threshold pass on this one revision. |
| 2026-09-22 — Concurrent-origin reconciliation after ledger publication | Reconciled the documentation chunk with later mainline commits after the first push was rejected as non-fast-forward, then pushed the merge for other workstreams. | ~0.5–0.75 h | Documentation commit `0a2bc972` is included in pushed merge tip `76650bd1`; `HEAD` and `origin/main` match. The latest hosted poll records TiDB job `106550958877` failed, FoundationDB `106550958697` in progress, compositions `106550958843` and base Ozone `106550958998` queued, with no aggregate job. | Inspect the retained TiDB failure and wait for the remaining exact-SHA producer/aggregate results; no failed, queued or in-progress result is acceptance evidence. |
| 2026-09-22 — Provider lease-renewal fast path implementation | Added a cached lease-renewal fast path for hot operations and forced provider validation at metadata/stat/sync/shutdown/reconciliation boundaries, preserving provider fencing and fail-closed behavior. | ~1.5–2.5 h | ~0.5 h shared-target rebuild/test wait | Focused ChunkedFs 19/19, full locked workspace tests, strict workspace Clippy, formatting and diff checks passed. The change is implementation evidence only until a current-tip hosted provider packet reports performance and clock/expiry behavior. |
| 2026-09-22 — Lease-renewal security review | Completed the final focused security diff scan after adding the forced reconciliation validation guard that closed the earlier stale-cleanup candidate. | ~0.75–1.25 h | 0 h hosted; provider/customer security deferred | Scan `4265d7aa-1425-46dc-8620-b37a60ebf97a` sealed with complete coverage and zero reportable findings. Provider clock authority, customer Ozone TLS/IAM, capacity, SLO/RPO/RTO, native and release gates remain external. |
| 2026-09-22 — Lease-renewal implementation publication | Reconciled concurrent origin updates after one non-fast-forward rejection, pushed the tested implementation and verified the clean remote alignment. | ~0.5–0.75 h | ~0.5 h fetch/merge/push; concurrent-mainline elapsed time | Commit `786408b3` is included in remote tip `f10dbf2257364b4ca5acf2ec2d151b5258883dcb`; local `HEAD` and `origin/main` match exactly and the worktree is clean. |
| 2026-09-22 — Exact-SHA hosted lease-renewal qualification dispatch | Dispatched a fresh non-canceling hosted matrix after publishing the implementation chunk. | ~0.25 h | External wait ongoing; run `35669685204` selected exact SHA `f10dbf2257364b4ca5acf2ec2d151b5258883dcb`; TiDB `106563125141` is in progress, compositions `106563124842`, FoundationDB `106563125176` and base Ozone `106563125215` are queued, with no aggregate job yet | Await terminal producer and aggregate output, retrieve artifacts/metrics, and keep production **NO-GO** unless every configured provider and the full end-to-end packet pass on this one revision. |
| 2026-09-22 — Lease-renewal ledger refresh | Updated this ledger and `WORK_TRACKER.md` with the implementation commit, exact local gates, sealed security receipt, current hosted run, provisional estimates and explicit provider/customer blockers. | ~0.75–1 h | ~0.25 h documentation commit/push and CI queue | This documentation chunk is published separately so other threads have the current build-on state; no hosted pending state is promoted to acceptance. |
| 2026-09-22 — Post-publication shared-tip reconciliation | Merged later concurrent transport/workstream commits after the ledger push and refreshed the current-tip references without changing the exact hosted test SHA. | ~0.1–0.25 h | ~0.25 h remote fetch/merge/push | Current shared tip is `c2670fe3967c01d9f21332ebe1e1b930c30eaf56`; the hosted packet remains correctly tied to W26 implementation SHA `f10dbf2257364b4ca5acf2ec2d151b5258883dcb`. |
| 2026-09-22 — Terminal lease-renewal hosted packet review | Rechecked run `35669685204`, downloaded the retained base/composition/TiDB/FoundationDB artifacts, inspected provider JSON and producer/aggregate logs, and separated functional marker success from the hard performance failure. | ~0.75–1.25 h | ~0.5–1 h hosted artifact retrieval and unrelated workflow completion | SQLite/R2 631.16, PGlite/R2 552.65, TiDB/R2 120.54 and FoundationDB/R2 178.98 IOPS; all rows had 1,200/1,200 successful lifecycle operations with zero timeout/cleanup failures, but every row failed `IOPS_TARGET_NOT_MET` and aggregate `106566312898` failed closed. W26.15 remains open. |
| 2026-09-22 — Terminal-result ledger correction | Updated the snapshot, terminal diagnosis, work-item override, production gate references, estimates and current-status boundary from pending to terminal diagnostic state. | ~0.5–0.75 h | ~0.25 h current `origin/main` reconciliation; no staging environment | The exact hosted SHA remains `f10dbf22`, current shared tip is `aa08d680`, and production remains **NO-GO**; the next chunk is safe publication-path performance work. |
| 2026-09-22 — Provider publication-barrier implementation | Added a conservative opt-in `MetadataStore::publish_includes_flush_barrier()` capability and used it to skip only the redundant post-publish metadata probe for SQLite, PGlite, TiDB and FoundationDB; custom providers remain false by default and explicit `syncfs` still flushes. | ~1.5–2.5 h | ~0.75–1.25 h shared Cargo rebuild/test time; provider commit semantics are a hosted qualification dependency | Commit `c7f0e6d0` passed the focused ChunkedFs suite 20/20, full locked workspace/all-target tests, strict workspace Clippy, formatting and diff checks. The code is included in hosted revision `fbc8346d292147bba006cfdcbc3b8f8b7127e2e7`; this is implementation evidence, not a performance pass. |
| 2026-09-22 — Publication-barrier security and hosted dispatch | Reviewed the nine-file implementation diff, then dispatched a fresh exact-SHA Ozone qualification run. | ~0.75–1.25 h | Hosted CI queue/provider startup and artifact retention are external elapsed gates | Security scan `e44de27d-96d8-4330-a831-b995d6458a6c` completed with complete coverage and zero reportable findings. Run `35672928117` selected exact SHA `fbc8346d292147bba006cfdcbc3b8f8b7127e2e7`; latest poll had TiDB in progress, compositions/FoundationDB/base queued and no aggregate. No pending state is acceptance evidence. |
| 2026-09-22 — Publication-barrier ledger/tracker refresh | Updated the W26 ledger and `WORK_TRACKER.md` with the new capability, exact local/security evidence, the terminal 356696 baseline, current run 356729 state, provisional estimates and production blockers. | ~0.5–0.75 h | ~0.25–0.5 h concurrent-mainline reconciliation and push; no staging environment | This documentation chunk is being published to the shared `origin/main` build-on base; production remains **NO-GO** until a terminal one-revision all-provider/end-to-end packet passes the unchanged 1,000-IOPS gate and the external security/SLO/RPO/RTO/DR/release boundaries are satisfied. |
| 2026-09-22 — Bounded mutation collection-window implementation | Widened mutation-runner collection from one cooperative scheduler yield to a fixed eight-round bounded window so concurrently prepared remote block operations can coalesce before metadata publication without a timer, provider-controlled delay or change to fencing/CAS. | ~0.75–1.25 h implementation; estimates remain provisional | ~0.75–1.25 h shared-target rebuild/test time | Commit `183660a4` passed focused ChunkedFs 20/20, full locked workspace/all-target tests, strict workspace Clippy with `-D warnings`, formatting and `git diff --check`. Security scan `47641152-6539-48e1-96b6-bf2201033486` completed with complete changed-file coverage and zero reportable findings. This is a local implementation/security result, not a hosted IOPS pass. |
| 2026-09-22 — Bounded-window publication and hosted qualification dispatch | Reconciled concurrent mainline changes, pushed the tested implementation chunk for other threads, and dispatched the next exact-SHA Ozone qualification matrix. | ~0.5–0.75 h publication/reconciliation; estimates remain provisional | Concurrent origin advances caused one non-fast-forward retry; hosted runner/provider startup is external elapsed time | Merge tip `a81a0827` is verified on `origin/main`; run `35674425514` targets that exact SHA with W26 producer jobs `106577785881`, `106577785981`, `106577785755` and `106577785903` queued and no aggregate yet. Production remains **NO-GO** until a terminal one-revision packet passes every configured provider and end-to-end marker. |
| 2026-09-22 — Current-tip ledger/tracker refresh | Updated every current W26 summary/override, terminal provider metrics, implementation/security evidence, remaining actions, provisional estimates, external blockers and the session time log after run `35672928117` became terminal and run `35674425514` was dispatched. | ~0.5–0.75 h documentation; estimate remains provisional | ~0.25–0.5 h remote status/reconciliation; no staging environment | Documentation is being published as a separate chunk on top of `a81a0827` so other threads have the current build-on state; no pending or failed hosted packet is promoted to production evidence. |
| 2026-09-22 — R2 single-flight upload-coalescing implementation | Added process-local content-addressed leader/follower coordination so concurrent identical R2 block uploads share one conditional create, with cache-before-completion ordering and cancellation-safe follower release. | ~1.5–2.5 h implementation; estimate remains provisional | ~0.75–1.25 h shared-target rebuild/test time | Commit `d05548e8` passed 19 R2 unit tests, 2 HTTP interop tests, full locked workspace tests, strict workspace Clippy, formatting and diff checks. This is a correctness-preserving local performance chunk; hosted qualification remains required. |
| 2026-09-22 — R2 single-flight security review | Reviewed the two-file dependency/source diff for collision verification, prefix/conditional-create preservation, cancellation/error propagation, cache ordering and secret/data-boundary regressions. | ~0.75–1.25 h security review; estimate remains provisional | 0 h hosted; provider/customer controls deferred | Scan `739f4f5b-8cc4-4154-93f7-9e486745eab1` completed with complete changed-file coverage and zero reportable findings. Provider behavior outside the local diff remains an external hosted gate. |
| 2026-09-22 — R2 chunk publication and concurrent-mainline reconciliation | Published the implementation for other threads, reconciled unrelated remote 9P/WebDAV changes, and verified the shared tip before the documentation update. | ~0.75–1.25 h publication/reconciliation; estimate remains provisional | ~0.5–1 h remote fetch/merge/push; concurrent mainline changes are external elapsed time | `d05548e8` is included in current `origin/main` `412c422e`; the later W26 hosted run `35676336466` remains tied to exact tested SHA `503f3f75`. |
| 2026-09-22 — Terminal hosted packet review and artifact reconciliation | Retrieved and reviewed the terminal `35674425514` producer/aggregate packet, including retained JSON artifacts and exact per-provider lifecycle/IOPS metrics. | ~0.5–1 h evidence review; estimate remains provisional | ~0.5–1 h hosted artifact retrieval and provider workflow completion | SQLite/R2 637.01, PGlite/R2 457.65, TiDB/R2 147.31 and FoundationDB/R2 126.50 IOPS; every row completed 1,200/1,200 operations with zero timeouts/cleanup failures but failed the hard threshold; aggregate `106579811902` failed closed. |
| 2026-09-22 — Current-tip W26 ledger/tracker reconciliation | Refreshed every current snapshot, work-item row, production gate, remaining action, provisional estimate, external blocker and session-log entry for the single-flight chunk and terminal packet. | ~0.75–1.25 h documentation; estimate remains provisional | ~0.25–0.5 h remote status/reconciliation; no staging environment | Current shared tip is `412c422e`; production remains **NO-GO**. Run `35676336466` on exact SHA `503f3f75` is the next hosted packet and is not yet acceptance evidence. |
| 2026-09-22 — Fresh hosted packet and FoundationDB lockfile diagnosis | Retrieved the current run `35677828440` artifacts and reviewed exact producer boundaries on SHA `91866234`: SQLite/R2 passed 1,220.33 IOPS, PGlite/R2 failed 522.34, TiDB/R2 failed 145.74, and FoundationDB stopped before benchmark because its test lockfile lacked `tokio`. | ~0.75–1.25 h evidence review | ~1–1.5 h hosted queue/provider startup and artifact download | The packet remains diagnostic; no aggregate or all-provider acceptance is promoted, and production remains **NO-GO**. |
| 2026-09-22 — FoundationDB test lockfile correction | Regenerated `tests/foundationdb/Cargo.lock`, verified the one-entry diff and reran the manifest with `--locked`. | ~0.25–0.5 h implementation/verification | ~0.25 h shared Cargo check | Commit `8428a5ef` is published through merged `origin/main` tip `ca13a33f`; the next hosted run must verify the preflight reaches the durable FoundationDB benchmark. |
| 2026-09-22 — Lockfile publication and build-on handoff | Reconciled the lockfile commit with concurrent mainline changes after a non-fast-forward push race, then pushed the merged result for other threads. | ~0.25–0.5 h publication/reconciliation | ~0.5 h fetch/merge/push; concurrent origin movement is external elapsed time | `origin/main` contains the Ozone lockfile fix `0cef5d44`, FoundationDB lockfile fix `8428a5ef` and merged tip `ca13a33f`; a new exact-SHA qualification is still required. |
| 2026-09-22 — Lockfile-verified hosted qualification dispatch | Dispatched the full hosted Ozone/provider matrix after publishing the FoundationDB lockfile correction and ledger. | ~0.1–0.25 h dispatch/status capture | Hosted queue, provider startup and artifact retention are external elapsed time | Run `35678993571` selected exact SHA `4b4fe43a`; W26 producer jobs `106591586312`, `106591586421`, `106591586086` and `106591586325` were queued initially. Production remained **NO-GO** pending a terminal all-provider packet. |
| 2026-09-22 — Lockfile-verified terminal W26 packet review | Retrieved the exact artifacts for run `35678993571` and reconciled the base, four producer jobs and aggregate. | ~0.75–1.25 h evidence review | ~1.5–2.5 h hosted queue, durable provider startup and artifact retrieval | Base Ozone passed; SQLite/R2 `802.37`, PGlite/R2 `614.90`, TiDB/R2 `86.53` and FoundationDB/R2 `360.14` IOPS all failed the hard target with 1,200/1,200 lifecycle operations and zero timeout/cleanup failures; aggregate `106595737360` failed closed. W26.15 remains open and production remains **NO-GO**. |
| 2026-09-22 — PGlite/TiDB conditional publication fast path | Replaced the successful-path metadata preflight SELECT with a fenced conditional CAS UPDATE; retained locked classification for stale lease, revision conflict, missing row and unexplained zero-row outcomes. | ~1.5–3 h implementation/design; estimate remains provisional | ~0.25–0.75 h shared-target compile/test rebuild | Commit `214b9a6b` is published in merged `origin/main` tip `47547d36`. PGlite and TiDB unit gates pass; transaction, provider-clock, parameterization and fail-closed behavior remain explicit. Hosted IOPS evidence is still required. |
| 2026-09-22 — SQL publication fast-path verification and security review | Ran the full locked workspace tests, strict Clippy, formatting/diff checks and the bounded two-file security diff review. | ~0.75–1.25 h verification/security review; estimate remains provisional | 0 h hosted; live PGlite/TiDB integration services remain external | Full workspace tests and `-D warnings` Clippy exited 0; PGlite/TiDB focused unit tests passed; security scan `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` completed with complete changed-file coverage and zero reportable findings. Live provider semantics and hosted throughput remain unclosed. |
| 2026-09-22 — SQL publication fast-path publication | Committed, fetched and reconciled concurrent mainline changes after one non-fast-forward push race, then verified the shared tip. | ~0.5–1 h publication/reconciliation; estimate remains provisional | ~0.5–1 h remote fetch/merge/push; concurrent origin movement is external elapsed time | `214b9a6b` is included in `origin/main` `47547d36`; `HEAD` and `origin/main` match. The next complete Ozone/provider packet must select this exact published revision, and production remains **NO-GO**. |
| 2026-09-22 — Current shared-tip reconciliation | Reconciled the subsequent concurrent mainline FoundationDB/N-API changes after the ledger push and refreshed current-tip references before hosted dispatch. | ~0.25–0.5 h documentation/reconciliation; estimate remains provisional | ~0.25–0.5 h remote fetch/merge/push; concurrent origin movement is external elapsed time | The SQL publication fast path remains included at `214b9a6b`; current `HEAD` and `origin/main` are `4a442b23`. The next full Ozone/provider packet must select this exact revision; production remains **NO-GO**. |
| 2026-09-22 — Current-tip ledger correction before hosted dispatch | Corrected all authoritative W26 ledger and tracker references after concurrent mainline movement, preserving historical run records and the exact one-revision qualification rule. | ~0.25–0.5 h documentation/reconciliation; estimate remains provisional | ~0.25–0.5 h diff validation and remote publication; concurrent origin movement is external elapsed time | `HEAD` and `origin/main` were `4a442b23` before this documentation chunk; the next hosted packet must use the final published SHA after this chunk is reconciled. Production remains **NO-GO**. |
| 2026-09-22 — Concurrent-mainline push reconciliation | Merged the newly arrived W08/9P/WebDAV mainline work into the W26 documentation chunk and pushed the combined result for other threads. | ~0.25–0.5 h publication/reconciliation; estimate remains provisional | ~0.25–0.5 h remote fetch/merge/push; concurrent origin movement is external elapsed time | `HEAD` and `origin/main` now match at `38bde9d1`; the ledger is being refreshed to that final shared base before hosted dispatch. Production remains **NO-GO**. |
| 2026-09-22 — Full Ozone qualification dispatch on final pushed base | Dispatched the non-canceling manual CI workflow after the final concurrent-mainline reconciliation and captured the exact run/provider job IDs. | ~0.1–0.25 h dispatch and status capture; estimate remains provisional | Hosted runner queue, provider startup, workflow completion and artifact retention are external elapsed time | Run `35683158821` targets exact SHA `14dbf2c61b5d86606b165692e0ba1e0e7af545dc`; jobs are base Ozone `106604292731`, compositions `106604292790`, TiDB `106604293431` and FoundationDB `106604292803`, with aggregate pending. In-progress state is not acceptance evidence; production remains **NO-GO**. |
| 2026-09-22 — Terminal review of full Ozone qualification run | Retrieved the four retained producer artifacts, exact digests, provider JSON and aggregate log after the W26 jobs completed. | ~0.75–1.25 h evidence review; estimate remains provisional | ~0.5–1 h hosted artifact retrieval; unrelated full-CI jobs may remain in progress but do not alter the terminal W26 aggregate | Base Ozone job `106604292731` passed; composition `106604292790` measured SQLite/R2 `754.59` and PGlite/R2 `817.10`, TiDB `106604293431` measured `143.04`, FoundationDB `106604292803` measured `345.17`; every row completed 1,200/1,200 operations with zero timeout/cleanup failures. Aggregate `106606577835` failed closed on missing composition `OZONE_IOPS_PASS`; W26.15 remains open and production remains **NO-GO**. |
| 2026-09-22 — Post-publication origin/main reconciliation | Committed the terminal evidence ledger, merged concurrent mainline changes and pushed the reconciled documentation chunk for other threads. | ~0.25–0.5 h publication/reconciliation; estimate remains provisional | ~0.25–0.5 h remote fetch/merge/push; concurrent origin movement is external elapsed time | `HEAD` and `origin/main` match at `69c684c70758b26b8600ed5b1131cef23e5e13f3`. The terminal packet remains tied to tested SHA `14dbf2c61b5d86606b165692e0ba1e0e7af545dc`; W26 remains **NO-GO** until the next safe performance chunk and fresh exact-SHA packet. |
| 2026-09-22 — Concurrent-mainline reconciliation after ledger publication | Merged the next unrelated mainline changes after the W26 ledger push and refreshed the shared build-on boundary. | ~0.25–0.5 h publication/reconciliation; estimate remains provisional | ~0.25–0.5 h remote fetch/merge/push; concurrent origin movement is external elapsed time | `HEAD` and `origin/main` match at `1626d5381625d81696fa28624342f07087593760`. The terminal packet remains tied to tested SHA `14dbf2c61b5d86606b165692e0ba1e0e7af545dc`; W26 remains **NO-GO** until the next safe performance chunk and fresh exact-SHA packet. |
| 2026-09-22 — TiDB session-setup performance chunk | Moved TiDB pessimistic-mode setup and verification to private-pool connection creation, disabled redundant reset round trips, and retained transaction guards, fail-closed mode validation, parameterized SQL and commit/rollback semantics. | ~1.5–2.5 h implementation/design; estimate remains provisional | ~0.75–1.25 h shared-target builds/tests and remote reconciliation; hosted provider/Ozone latency is external | Commit `e41bed05` passed focused TiDB tests, strict provider Clippy, full locked workspace tests, strict workspace Clippy, formatting and diff checks. Security scan `c4fc0012-b0e2-421c-9db8-ca3edfce730c` covered the changed file with zero reportable findings. It was merged with concurrent mainline work and published at `9293b1f6`; the exact revision still requires a fresh hosted W26 matrix. |
| 2026-09-22 — Final shared-tip documentation reconciliation | Refreshed current-tip references after unrelated mainline work advanced the remote following the ledger push. | ~0.25–0.5 h documentation/reconciliation; estimate remains provisional | ~0.25–0.5 h fetch/merge/push; concurrent mainline movement is external elapsed time | `HEAD` and `origin/main` match at `83e0d3b7`; W26 code `e41bed05` and ledger evidence remain present. The next hosted matrix must select this exact shared revision or a later exact pushed revision; production remains **NO-GO**. |

## Current status addendum — hosted runs `35677828440` / `35678993571` and lockfile-gate correction

This addendum supersedes older paragraphs that describe run `35676336466` as
pending. Historical rows remain unchanged for auditability; the tables above
and this addendum are authoritative for the current state.

Run `35677828440` selected exact hosted revision
`918662349cd24b37e65433b835987b8234e89e87`, before the FoundationDB test
lockfile correction. It is diagnostic evidence only:

| Work item / job | Status and evidence | Completion interpretation | Remaining action | Provisional engineering estimate | External blocker / gate |
| --- | --- | --- | --- | --- | --- |
| W26.1–W26.2 / `ozone` `106587969438` | **PASS**: gateway policy, real Ozone block contract, restart/reopen and cleanup completed | 100% implementation and current hosted base gate | Preserve exact marker and artifact in the next packet | 0–1 h review | Hosted Ozone image, runner and customer topology remain external |
| W26.3a / `ozone-compositions` `106591586421` | **FAIL / diagnostic**: SQLite/R2 `802.37` IOPS, elapsed `1,495.56 ms`; PGlite/R2 `614.90` IOPS, elapsed `1,951.55 ms`; each completed 400 writes, 400 reads, 400 verified reads and 400 deletes (1,200/1,200), with zero timeouts and zero cleanup failures. Artifact `w26-ozone-compositions-evidence`, ID `10674341635` | Both composition providers remain below the 1,000-IOPS gate on the lockfile-verified revision; no composition acceptance marker was emitted | Continue correctness-preserving metadata/publication performance work; retain the fixed target and rerun on the next published exact SHA | ~1–4 d performance/design; 0.5–1.5 d hosted review | Ozone capacity/topology and PGlite runtime/provider latency are external inputs, but the shared publication path remains W26-owned |
| W26.3d / `ozone-tidb` `106591586086` | **FAIL / diagnostic**: TiDB/R2 `86.53` IOPS, elapsed `13,868.08 ms`; 1,200/1,200 lifecycle operations, zero timeouts and zero cleanup failures. Artifact `w26-ozone-tidb-evidence`, ID `10674350992` | Durable TiDB/restart setup reached the benchmark, but the hard throughput gate remains open by a wide margin | Preserve durable restart/fencing markers and continue correctness-preserving metadata/publication performance work | ~1–4 d shared performance/design; 0.75–1.5 d hosted review | Hosted TiDB/PD/TiKV, Ozone capacity/topology and customer secure durability remain external |
| W26.3c / `ozone-foundationdb` `106591586325` | **FAIL / diagnostic**: FoundationDB/R2 `360.14` IOPS, elapsed `3,332.00 ms`; 1,200/1,200 lifecycle operations, zero timeouts and zero cleanup failures. Artifact `w26-ozone-foundationdb-evidence`, ID `10674436409` | The FoundationDB lockfile correction allowed the durable benchmark to run, but the hard throughput gate remains open | Preserve durable restart/fencing/bounded-listing/cleanup markers and continue correctness-preserving metadata/publication performance work | ~1–4 d shared performance/design; 0.75–1.5 h hosted review | Hosted FoundationDB image/client and Ozone topology remain external; the preflight lockfile gate is fixed |
| W26.4b / aggregate `106595737360` | **FAIL / diagnostic** after all four producer rows completed; aggregate rejected the absent provider pass markers | One-revision evidence packet remains open; no terminal acceptance result exists | Preserve fail-closed aggregation and rerun only after the next performance chunk | ~0.5–1.5 h review | Workflow scheduling and concurrent jobs are external; queued/failed aggregate is never acceptance |
| W26.7–W26.10 | Implementation and verifier controls remain green; the run exercised strict configuration and retained diagnostic artifacts | 100% implementation; hosted qualification remains open | Keep no-skip, exact-profile, artifact-integrity and retention checks unchanged | ~0.5–1.5 d review | Hosted artifact service, provider credentials/topology and customer TLS/IAM are external |
| W26.14 | Evidence-surface implementation remains complete, but the current packet is not terminally accepted | 100% implementation; hosted end-to-end qualification open | Require gateway, composition, TiDB, FoundationDB, native/end-to-end and aggregate markers on one revision | ~0.75–1.5 d hosted review | Native runners, provider fixtures, secure customer runtime and CI orchestration remain external |
| W26.15 | **OPEN / NO-GO**: terminal run `35678993571` measured SQLite/R2 `802.37`, PGlite/R2 `614.90`, TiDB/R2 `86.53` and FoundationDB/R2 `360.14` IOPS; all four completed 1,200/1,200 operations with zero timeout/cleanup failures, but aggregate `106595737360` failed closed | 94% W26-owned implementation / 42% hosted qualification | The PGlite/TiDB conditional publication fast path `214b9a6b` is included in current `4a442b23` with local tests, Clippy and zero-finding security review; dispatch a fresh exact-SHA packet on that tip, then continue provider-specific work if the hard target remains open; never lower the target | ~1.5–4 d implementation/design plus 0.75–1.5 d review per rerun | Ozone/customer capacity, provider latency, hosted runner stability and artifact retention are external gates |

The separate lockfile correction chunk is published as commit `8428a5ef`
(`fix(w26): refresh foundationdb test lockfile`) and adds only the missing
`"tokio"` dependency to `tests/foundationdb/Cargo.lock`. Local
`./scripts/cargo-shared check --manifest-path tests/foundationdb/Cargo.toml
--locked` passed. The earlier Ozone test lockfile correction is `0cef5d44`.
The fixes were reconciled with concurrent mainline changes and pushed to
`origin/main`; the lockfile-verified run below confirms FoundationDB reached
its benchmark. The next hosted packet must select the next performance-change
revision.

The hard production decision remains **NO-GO**. The fresh run demonstrates
that the SQLite/R2 path has exceeded 1,000 IOPS in an earlier sample, but it
does not close the all-provider gate: terminal run `35678993571` measured
802.37/614.90/86.53/360.14 IOPS for SQLite/PGlite/TiDB/FoundationDB and the
aggregate failed closed. The
1,000-IOPS threshold, 99.99% reliability objective, five-minute RPO/RTO
boundary, security requirements, CI-only qualification boundary and
customer-owned Ozone backup/DR and release boundaries are unchanged.

The latest qualification is terminally diagnostic: run `35678993571` selected
exact SHA `4b4fe43ab08a7e5446bccb2198517d4847159b6b`, with W26 producer jobs
`106591586312` (`ozone`), `106591586421` (`ozone-compositions`), `106591586086`
(`ozone-tidb`) and `106591586325` (`ozone-foundationdb`) complete, and aggregate
`106595737360` failed closed. This is not an acceptance result; the next run
must follow a correctness-preserving performance chunk.

## Hosted qualification dispatch record — run `35683158821`

Manual workflow `35683158821` was dispatched on 2026-09-22 against the exact
published revision `14dbf2c61b5d86606b165692e0ba1e0e7af545dc`. It used the
manual workflow concurrency group, so later ordinary pushes do not cancel
this qualification packet. At capture time the producer jobs were:

| Producer | Job ID | Capture state | Acceptance meaning |
| --- | ---: | --- | --- |
| Ozone base | `106604292731` | completed / passed | Emitted gateway, policy, recovery and cleanup markers; artifact `10675672094`, digest `sha256:a07e83aa98d0a144b22a35c8ba9e4bda5f90c413f682d50da70bf9b7e4cc2f3a` |
| Ozone compositions | `106604292790` | completed / failed hard IOPS | SQLite/R2 754.59 and PGlite/R2 817.10 IOPS; artifact `10676355562`, digest `sha256:c854fdf5f36266c4e0d7a1ce67e46fd24ffedede9d816b94c3b5f4755e232ff6` |
| Ozone TiDB | `106604293431` | completed / failed hard IOPS | TiDB/R2 143.04 IOPS; artifact `10675867797`, digest `sha256:1a1d715597c227f4f7927eed56e60ad34514357048989389f80a649686c6bed6` |
| Ozone FoundationDB | `106604292803` | completed / failed hard IOPS | FoundationDB/R2 345.17 IOPS; artifact `10675443241`, digest `sha256:6b882e9612f17c80190919887ec37d3a43ff50f70cf5946f8f081f8cc4e6f03d` |
| W26 aggregate | `106606577835` | completed / failed closed | Rejected the packet because composition lacked `OZONE_IOPS_PASS`; exact revision/provider and retained-artifact verification ran before the fail-closed result |

This terminal packet is diagnostic, not acceptance evidence. All four provider
rows completed 400 writes, 400 reads, 400 verified reads and 400 deletes, for
1,200/1,200 successful operations per row, with zero timeout and cleanup
failures. The hard target remains unchanged at 1,000 IOPS; no provider emitted
`OZONE_IOPS_PASS`, and the aggregate correctly failed closed. Hosted
runner/provider startup, Ozone fixture capacity, artifact retention and
customer secure topology remain external gates.

### Terminal metrics from run `35683158821`

| Provider row | IOPS | p95 write/read/delete (ms) | Lifecycle evidence | Result |
| --- | ---: | --- | --- | --- |
| SQLite/R2 | 754.59 | 231.87 / 39.34 / 32.15 | 1,200/1,200; timeouts 0; cleanup failures 0 | `IOPS_TARGET_NOT_MET` |
| PGlite/R2 | 817.10 | 293.16 / 17.06 / 7.66 | 1,200/1,200; timeouts 0; cleanup failures 0 | `IOPS_TARGET_NOT_MET` |
| TiDB/R2 | 143.04 | 1,622.13 / 770.24 / 158.39 | 1,200/1,200; timeouts 0; cleanup failures 0 | `IOPS_TARGET_NOT_MET` |
| FoundationDB/R2 | 345.17 | 534.95 / 126.15 / 49.70 | 1,200/1,200; timeouts 0; cleanup failures 0 | `IOPS_TARGET_NOT_MET` |

The base log retained `OZONE_HEALTHY`, `OZONE_READY`, block-contract,
fault-window, bounded gateway-failure, restart/reopen, integration and
cleanup passes. The composition log retained SQLite/PGlite bounded-listing,
composition, Node SDK (`pass=7 skip=1 fail=0`), CLI and remote-HTTP passes,
then failed only the hard IOPS marker. TiDB and FoundationDB retained Ozone
health/restart/reopen and cleanup markers; their RustFS composition markers
failed after the hard IOPS result. The aggregate log recorded
`W26_OZONE_EVIDENCE_PACKET_FAIL` for missing composition `OZONE_IOPS_PASS`.

## Current terminal override — run `35683158821`

This section supersedes older current-status paragraphs that name
`35678993571` as the latest terminal packet. Run `35683158821` selected exact
revision `14dbf2c61b5d86606b165692e0ba1e0e7af545dc`; its W26 aggregate
`106606577835` failed closed because the composition log lacked the required
`OZONE_IOPS_PASS` marker. The hard provider metrics were SQLite/R2 `754.59`,
PGlite/R2 `817.10`, TiDB/R2 `143.04` and FoundationDB/R2 `345.17` IOPS. Each
row completed 1,200/1,200 lifecycle operations with zero timeout and cleanup
failures. Retained producer artifacts are composition `10676355562`, TiDB
`10675867797`, FoundationDB `10675443241` and base `10675672094`; their
digests and marker evidence are recorded above. Production remains **NO-GO**;
the next work is a correctness-preserving performance chunk, followed by a
fresh exact-SHA hosted packet.

## Publication record

Authoritative current update (2026-09-22): `origin/main` is
`83e0d3b7e11cf272d71c204fada5ea827d4a9f29`, and this checkout is aligned
with that shared tip after the TiDB performance-chunk publication and a
subsequent unrelated mainline reconciliation.
The W26 metadata batching commit `c4e9a253` is included through the published
reconciliation tip `a1cb4ca92bd0c1b619c56231002ee182852b01a8`; queue hardening
commit `d152fa1a`, Ozone test-contract fix `edb6a43e` and cleanup correction
`0282df64` are included through the current concurrent-mainline tip; concurrent
create rebasing commit `d3d6299e` is included through `059f801e` and its
descendants; lease-renewal caching/forced validation is `786408b3`; bounded
mutation collection is `183660a4`. The 20-test focused ChunkedFs suite, full
locked workspace tests, strict workspace Clippy and diff checks are green.
Security scans
`6d5a7665-d8cd-46b7-a9bd-73fec54f5431` and
`46d4cf32-9d57-4f7e-975f-2616ddc53dc5` and
`85c31e86-1fc8-4a6d-bb95-ef2d956b9ed8`, `b8f6e846-e497-4ae6-b972-7ae3701ec722`,
`4265d7aa-1425-46dc-8620-b37a60ebf97a`,
`e44de27d-96d8-4330-a831-b995d6458a6c` and
`47641152-6539-48e1-96b6-bf2201033486` and
`739f4f5b-8cc4-4154-93f7-9e486745eab1` and
`5a8fcd70-71d4-461b-bb2a-dec8461c22bc` and
`c4fc0012-b0e2-421c-9db8-ca3edfce730c` are complete with zero reportable
findings within their local scopes. The latest SQL publication scan covered
both changed provider files with complete coverage; the TiDB session-setup
scan covered its changed provider file with complete coverage. The TiDB
session-setup optimization is `e41bed05` and is included in current shared
tip `83e0d3b7`. Terminal W26 producer/aggregate
packet `35678993571` on `4b4fe43a` failed the strict IOPS gate; it is terminal
for every W26 producer and aggregate `106595737360`. All four rows completed
their 1,200-operation lifecycle with zero timeout/cleanup failures, but no
provider emitted `OZONE_IOPS_PASS`. No failed, queued, in-progress or canceled
job is production acceptance evidence.

This document is intentionally updated alongside `WORK_TRACKER.md`. The
ledger's percentages and estimates are snapshots; the tracker checkbox and
terminal hosted evidence remain authoritative for whether W26 is actually
complete.

Current-status override: historical session rows below preserve what was known
at the time they were written. The authoritative current state is the snapshot
and terminal diagnoses above: runs `35635486040`, `35641941218`,
`35649202405`, `35655276021`, `35669685204`, `35672928117` and `35674425514`
are failed diagnostic packets, W26.15 is open, and run `35678993571` is now the
latest terminal diagnostic packet on exact SHA `4b4fe43a`. Base Ozone passed,
all four provider rows failed only the hard IOPS target, and aggregate
`106595737360` failed closed. No queued, in-progress, canceled or failed job is
promoted to acceptance. Aggregate `106522172018` is an earlier failed
diagnostic packet. Current shared tip `1626d538` contains the
publication-barrier implementation (`c7f0e6d0`), bounded mutation collection
(`183660a4`), single-flight R2 upload coalescing (`d05548e8`) plus the
lazy-atime/EOF, batching/cache, queue-safety, concurrent-create and
lease-renewal validation work and the PGlite/TiDB conditional publication fast
path (`214b9a6b`) with local test, Clippy, compile-check and focused security
evidence; it still requires a fresh terminal hosted qualification before
W26.15 can move.

Latest publication override (2026-09-22): the paragraphs above that mention
run `35676336466` as pending are historical and are superseded by the current
status addendum. Run `35677828440` tested exact SHA `91866234`; its base Ozone
job passed, SQLite/R2 measured `1,220.33` IOPS, PGlite/R2 `522.34`, TiDB/R2
`145.74`, and FoundationDB stopped at the missing-`tokio` `--locked`
lockfile gate. The FoundationDB correction is commit `8428a5ef`, published
with the earlier Ozone correction `0cef5d44` at merged `origin/main` tip
`ca13a33f`. A new exact-SHA run on the fully lockfile-verified documentation
tip is required; no result from `35677828440` closes W26.15.

Terminal-packet override (2026-09-22): run `35678993571` selected exact SHA
`4b4fe43ab08a7e5446bccb2198517d4847159b6b`. Base Ozone job `106591586312`
passed. Composition job `106591586421` retained artifact `10674341635` and
measured SQLite/R2 `802.37` and PGlite/R2 `614.90` IOPS. TiDB job
`106591586086` retained artifact `10674350992` and measured `86.53` IOPS.
FoundationDB job `106591586325` retained artifact `10674436409` and measured
`360.14` IOPS after the lockfile correction. Every benchmark row completed
400 writes, 400 reads, 400 verified reads and 400 deletes, for 1,200/1,200
successful operations, with zero timeout and cleanup failures. Aggregate
`106595737360` failed closed because no provider emitted the hard pass marker.
This is the current terminal diagnostic packet, not acceptance evidence; the
next code/qualification chunk must address the shared metadata-publication
latency without weakening fencing, durability ordering, security checks or the
1,000-IOPS threshold.

Current implementation override (2026-09-22): the next W26-owned performance
chunk is commit `214b9a6b` (`perf(w26): fast-path fenced metadata publication`).
PGlite and TiDB now attempt the fully parameterized lease/fence/revision CAS
update first; only a zero-row CAS performs the locked read needed to classify a
stale lease, revision conflict, missing row or unexplained outcome. This keeps
transaction rollback/commit boundaries, provider-clock expiry checks and
fail-closed semantics intact. PGlite/TiDB unit tests, the full locked workspace
test suite and strict Clippy passed; formatting and `git diff --check` passed;
security scan `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` covered both changed files
with zero reportable findings. The implementation is published in merged
`origin/main` tip `1626d538`; terminal manual run `35683158821` exercised
revision `14dbf2c6` and measured 754.59/817.10/143.04/345.17 IOPS, so
production remains **NO-GO** pending the next remediation and exact-SHA run.

The authoritative W26 hosted packet is run `35585066458` on tested revision
`9c098e5`; the documentation closeout was subsequently rebased and pushed as
`b7e2758` over unrelated mainline changes. Later unrelated pushes do not turn
the terminal W26 provider jobs into queued or canceled evidence.

The production rollout track is intentionally separate from that packet. Its
current decision is **NO-GO** with 0 of 15 P0–P14 gates terminally accepted;
the open gate ledger above is the source of truth for production work, estimates
and blockers. Product direction now makes W26 a customer-deployed integration
qualification stream: its next acceptance target is a complete, secure,
all-feasible-provider, end-to-end CI packet, not a customer deployment.

The complete-surface packet follow-up was committed as `20a06b8` and published
after reconciling concurrent `origin/main` changes at `0e0454d`. The aggregate
verifier now requires every currently wired gateway, composition, Rust/Node/CLI,
remote-HTTP, TiDB N-API and FoundationDB N-API/restart marker in addition to
the provider artifacts and policy/recovery markers. Local tests and syntax
checks pass; focused security scan `e4ce1aab-c6cd-44e4-b20d-3130ca357412`
reported zero findings with hosted artifact authorization, provider execution,
customer secure runtime and measured SLO/RPO/RTO explicitly deferred. The
production decision remains **NO-GO**.

The current full locked workspace regression command exited 0 on the shared
Cargo target. The manual revision-bound hosted qualification run
`35635486040` is running on `cfe7e29e001dc01f2fa430a54bc8981b44db0b05`;
`ozone` `106451808629`, `ozone-compositions` `106451808739` and `ozone-tidb`
`106451809745` were in progress at the last review, while
`ozone-foundationdb` `106451808796` was queued. Current `origin/main`
`ce7b365a` has no W26-path changes relative to that run, but the run remains
non-terminal and therefore non-evidence until the producer jobs and aggregate
packet verifier finish successfully.

That older status paragraph is historical. The current W26.15 implementation
is commit `c4378dc1`, published after concurrent-mainline reconciliation at
`88b707ba`: immutable block reads and writes overlap outside the volume-wide
metadata gate, publication remains revision/base checked with serialized
conflict fallback, and the lifecycle read barrier prevents shutdown from
fencing an in-flight optimistic operation. Local full-workspace tests, strict
Clippy and the focused blocked-read/shutdown regression pass. Security scan
`e7c03690-856b-42ae-8d47-8af0dc788a54` completed with zero reportable local
findings and explicit hosted/customer exclusions. Replacement workflow
`35641941218` later became terminal for W26 and is recorded above as a failed
diagnostic packet; it is not acceptance evidence.

The HTTP security-boundary chunk was committed as `f74ffce`, merged with
concurrent mainline changes, and pushed as `b997696`. The current-head hosted
workflow is tracked separately above; a queued or in-progress run is not
promoted to production evidence.

The Ozone provider-matrix/IOPS chunk was committed as `333c764`, merged with
concurrent mainline changes, and pushed as `a0b2fae`. Its local evidence is
recorded above; the hosted workflow created from `a0b2fae` must reach terminal
success before the provider or 1,000-IOPS gates move beyond pending.

The scoped block-reconciliation chunk was implemented in `ab7c63e`, with the
zero-grace and streamed-listing safety follow-up in `502ba35`; both were merged
with concurrent mainline changes and pushed at `69dd740`. Their local evidence
is recorded above; a fresh hosted workflow and provider/retention/security
review are required before the lifecycle production gates move beyond pending.

The bounded directory-materialization chunk was implemented in `fddf4e1`, merged
with concurrent mainline changes, and pushed at `9de159a`. Its local evidence
is recorded above. The provider-bounded key-enumeration follow-up was committed
as `ab551be`, merged with concurrent mainline changes, and published at
`c719306`. Its local evidence is recorded above; the current CI run
`35608567514` has Ozone and Ozone-TiDB jobs in progress with Ozone compositions
and FoundationDB queued, so none of those current-revision states is promoted
to a pass. Providers without a backend-enforced listing limit remain
fail-closed, and `MOUNTX_SOURCE` parity remains an explicit environment gate.

The bounded unstorage parity fixtures were committed as `8057fd1`, merged with
concurrent mainline changes, and published at `d619f93`; the published revision
was verified to match `origin/main`. The capable-provider overflow/success and
legacy-provider `ENOTSUP` paths pass locally. Its current CI run `35609794020`
is pending and Fault injection run `35609793694` is in progress, so neither is
evidence yet; `MOUNTX_SOURCE` remains unset and its oracle parity is still open.

The Ozone provider-backed bounded-listing qualification was committed as
`e2ea888`, merged with concurrent mainline changes, and published at `ec58ba5`;
the published revision was verified to match `origin/main`. The SQLite/R2 and
PGlite/R2 ignored composition tests now assert bounded success and
`EOVERFLOW`. Current CI run `35610948086` and Fault injection run `35610947492`
are queued, while Live Cloudflare R2 run `35610947637` is in progress; none is
promoted to W26 evidence yet.

The exact bounded-provider limit correction was committed as `73cdc93` and
verified on `origin/main`. The KV adapter now passes the caller's exact limit
to `get_keys_bounded`, and the 13-test suite records the expected provider
limits. Concurrent W08/W25 and SDK/FoundationDB changes were then
fast-forwarded through `3fca802`; the current CI attempt has CI run
`35612976567` pending, Fault injection `35612976712` queued, Live Cloudflare
R2 `35612976618` queued and Live AWS S3 `35612976513` queued. None is promoted
to W26 evidence.

The durable-provider IOPS qualification chunk was committed as `de9d267` and
published after reconciling concurrent `origin/main` changes at `414a469`.
`scripts/test-tidb.sh` now runs the public Node split-TiDB/R2 benchmark with
the hard `--min-iops 1000` target and retained JSON output. The FoundationDB
harness runs the same benchmark inside the feature-enabled client container,
uses its owned writable `/fdb` volume for the result, copies the JSON to the
workflow artifact path and emits `FOUNDATIONDB_OZONE_IOPS_PASS` only after the
provider run and artifact complete. Both dedicated CI jobs retain their JSON
artifacts even on failure. Positive-integer validation, shell syntax, runner
syntax/unit tests, YAML parsing and diff checks passed locally. Current tip
run `35619958098` was canceled before jobs started and Live Cloudflare R2 run
`35619958070` failed; neither is evidence.

Focused security-diff scan `5fc5a943-07bb-4979-9f37-efd87a7f505e` covered the
W26 IOPS shell/workflow surfaces and found zero reportable findings. Its
coverage is intentionally partial: hosted credential/isolation, provider
TLS/IAM/rotation, retained artifact access and live TiDB/FoundationDB/Ozone
execution remain deferred production gates.

The current-revision standard security scan
`70af8d4e-9fb3-4d2c-b688-54a9b6535739` targeted `3fca802` and completed with
zero reportable local findings across 16 surfaces. Its coverage is explicitly
partial: customer Ozone TLS/IAM/certificate rotation, provider-native bounded
allocation, dependency/platform provenance, 99.99%/recovery drills and
production operations remain deferred. This closes the local scan action, not
the production security gate.

The durable-provider bounded-listing extension was committed as `d1c9e44` and
published with concurrent mainline changes at `44b01a7`; the published revision
was verified to match `origin/main`. TiDB's locked chunked test target and
strict Clippy passed; FoundationDB `cargo check --tests` passed, while local
test-binary linking is blocked by missing `libfdb_c`. The new live TiDB,
FoundationDB and Ozone markers remain hosted gates, so no provider pass is
promoted from the local compile evidence.

The current published-tip standard security scan
`5ad61e60-20e3-4223-885a-d4b516d49bb1` targeted `44b01a7` and completed with
zero reportable findings across 16 W26-relevant surfaces. Semantic coverage is
explicitly partial: the parent fallback was used after independently launched
workers did not return within bounded waits. Customer Ozone TLS/IAM/rotation,
provider-native allocation, dependency/native provenance, 99.99%/recovery and
operations remain open. The `44b01a7` hosted workflows (CI, fault injection,
W08 policy and Live Cloudflare R2) are canceled or failed and are not evidence.

The FoundationDB Ozone Node/N-API bounded-listing extension was committed as
`b80c19c`, merged with concurrent mainline changes, and published at `0842474`;
the published revision was verified to match `origin/main`. The dedicated
Ozone FoundationDB job now builds the feature-enabled addon in the pinned
client image, runs the Node seed/reopen bounded success/overflow checks, and
keeps its R2 prefix under the owned Ozone cleanup scope. Local syntax checks
passed and the non-feature invocation skipped safely; hosted markers remain
pending and are not promoted to acceptance.

The TiDB Ozone Node/N-API bounded-listing extension was committed as
`ef6a876`, merged with concurrent mainline changes, and published at `43f8df2`;
the published revision was verified to match `origin/main`. The Ozone TiDB job
now installs and builds the public addon, runs the provider-backed Node
seed/reopen bounded success/overflow test, and scopes its R2 prefix under the
owned Ozone run. Shell, JavaScript, YAML and diff checks passed locally; the
hosted markers remain pending and are not promoted to acceptance.

The credential-free Ozone production-config policy chunk was committed as
`b8d8fb1`, merged with concurrent mainline changes, and published at `4d8b5fd`;
the expanded negative-path chunk `7018b59` was subsequently merged with
concurrent mainline changes and published at `6a1b94b`. The published revision
was verified to match `origin/main`. The policy script and fixtures cover
SQLite, PGlite, TiDB and FoundationDB metadata, durable settings, HTTPS R2
blocks, scoped prefixes, exact external secret references and TiDB TLS options
without contacting a provider. The expansion independently rejects inline
credentials, unsafe FoundationDB lease authority and TiDB TLS verification
downgrades. Local four-positive/four-negative executions, Node syntax, YAML
parsing and diff checks passed. Focused scans `d74e3e86-2e0a-45cf-9819-
e31f428eb5d4` and `7addeeb5-4601-4951-aca9-becffb9bd4b9` completed with zero
reportable findings; hosted/customer controls remain partial/deferred. Latest
tip CI `35623755790` is queued, W08 release targets `35623755638` are pending,
W08 release policy `35623755613` is in progress and Live Cloudflare R2
`35623755595` failed, so no current-tip result is promoted.

The strict provider-configuration IOPS chunk was committed as `d46e091`,
merged with concurrent mainline changes, and published at `74fe4c7`; the
published revision was verified to match `origin/main`. The storage benchmark
now fails in qualification mode when any requested provider is skipped, and
the generic Ozone composition lane explicitly qualifies only SQLite/R2 and
PGlite/R2 while dedicated TiDB/FoundationDB lanes remain strict. Local unit,
missing-provider, syntax, YAML and diff checks passed. Focused scan
`60269206-bb22-4b78-aaf7-f05d16ffcca0` completed with zero reportable findings
across five source surfaces; hosted/provider coverage remains partial. Latest
tip CI `35628078285` is pending, Fault injection `35628078322` is queued, W08
release targets `35628078265` and policy `35628078261` are pending, W04
production policy `35628078220` is queued and Live Cloudflare R2 `35628078323`
is queued; none is W26 acceptance evidence.

The IOPS artifact/profile integrity chunk was committed as `66f3670`, merged
with concurrent mainline changes, and published at `00d2b80`. The generic,
TiDB and FoundationDB wrappers now reject a target below 1,000 or a weakened
4 KiB/400-iteration/concurrency-64 profile, then validate the exact requested
provider set, no skipped/configuration-failed rows, successful cleanup and
per-size lifecycle success in the retained JSON before emitting a pass marker.
Local benchmark unit tests, verifier negative cases, Node/shell syntax and
diff checks passed. Focused scan `1d97028f-e153-4b49-9fac-c3a8c1fc1117`
completed with zero reportable findings across five changed source surfaces;
hosted/provider/customer controls remain partial or deferred.

The fail-closed IOPS artifact-retention chunk was committed as `c0f8370`,
merged with concurrent mainline changes, and published at `12ba117`. The
generic, TiDB and FoundationDB JSON uploads now use
`if-no-files-found: error`, so a missing expected artifact fails the CI
evidence job. CI YAML parsing, shell syntax, benchmark unit tests and diff
checks passed. Focused scan `18010cad-ed69-4da3-b0a9-57163091e878` completed
with zero reportable findings; hosted artifact authorization/retention and
provider/customer controls remain deferred. Latest tip CI `35628709359` is
pending, Fault injection `35628709207` is queued, W08 release targets
`35628709298` and policy `35628709428` are pending, W04 production policy
`35628709324` succeeded but is unrelated, and no Live Cloudflare R2 result was
recorded; none is W26 acceptance evidence.

The documentation chunk `3741b29` was merged with concurrent mainline changes
and published at `fd670b1`; it was superseded by the expanded security chunk
published at `6a1b94b`, then reconciled with concurrent mainline changes and
published at `9b3b90b`, followed by the concurrent FUSE/mainline update
published at `3681f27`, the strict qualification publication at `74fe4c7`,
the concurrent reconciliation at `80344aa`, the later reconciliations at
`3552344` and `75b900f`, the IOPS-verifier publication at `00d2b80`, and the
latest fail-closed artifact-retention publication at `12ba117`. Its CI run
`35628709359`, W08 release targets `35628709298`, W08 release policy
`35628709428` and Fault injection `35628709207` are pending or queued; W04
production policy `35628709324` succeeded but is unrelated, and these states
are not W26 acceptance evidence.

The one-revision W26 Ozone evidence-packet chunk was committed as `08f4530`
and published after reconciling concurrent `origin/main` changes at `097ed00`.
It retains the policy/base/provider logs and generic, TiDB and FoundationDB
IOPS JSON artifacts, adds explicit negative-policy pass markers, and makes the
`w26-ozone-evidence` aggregate job fail closed when any artifact, source
revision, provider marker, integration/recovery marker or cleanup marker is
missing. Local benchmark/evidence unit tests, Node/shell syntax, YAML parsing
and diff checks passed. Security scan
`c67ae8e0-99af-4ade-8284-a612d599b5e4` completed with zero reportable findings;
hosted/provider/customer coverage remains explicitly deferred. The subsequent
concurrent mainline merge published `f3aae7d`; its CI run `35630613146` was
canceled before jobs started, Fault injection `35630613141` is pending, W04
production policy `35630613331`, W08 release targets `35630613180` and W08
release policy `35630613275` are queued or pending, and no current-tip W26
result is promoted.

The W26 IOPS metric/payload integrity chunk was committed as `e875ba6` and
published after concurrent mainline reconciliation at `1775895`. The artifact
verifier now requires the fixed payload-size mapping, complete lifecycle and
statistic sample counts, 100% success, zero timeout/cleanup failures and finite
operation statistics before accepting a provider result. Local benchmark and
synthetic evidence tests, Node/shell syntax, YAML parsing and diff checks passed.
Security scan `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564` completed with zero
reportable findings; hosted provider performance and customer capacity remain
deferred, and no matching current-tip hosted W26 result was visible at the
publication snapshot.

The W26.13 rollout-contract implementation was committed as `8d2cfbb` and
published after concurrent mainline reconciliation at `0398d94`. It adds the
credential-free customer contract validator, positive/negative fixtures, CI
policy markers and one-revision packet requirements for the Tier-1 envelope,
secure Ozone topology, tenant scope, all four metadata providers, operational
controls and customer-owned recovery boundary. The focused security diff scan
`33c09a35-77f5-417c-862c-e5848185f50e` found zero reportable findings, with
customer and hosted/provider controls explicitly deferred. The companion
[`docs/w26-production-rollout.md`](w26-production-rollout.md) is the customer
and cross-stream handoff contract; its validator PASS is declaration-only.
The production decision remains **NO-GO** until terminal one-revision hosted
evidence and external customer/Ozone gates are complete.

## Current session time log continuation — 2026-09-22

These rows extend the chronological log above. Engineering time is provisional;
hosted queue, provider startup and artifact-service time are not counted as
implementation effort.

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — SQLite publication CAS chunk | Implemented and reviewed the SQLite conditional fenced metadata update, retained zero-row classification and transaction boundaries, ran focused/full gates, and published `ba4e89d0` through the shared mainline reconciliation. | ~1.5–2.5 h | ~0.5–1 h fetch/merge/push; live hosted latency external | Local tests, strict Clippy, formatting/diff checks and security scan `ea882470-0ef4-4f7a-8d91-03c8d85d7fb7` passed; exact hosted requalification remains open. |
| 2026-09-22 — TiDB session-isolation chunk | Moved `REPEATABLE-READ` setup/verification to private-pool connection creation, removed per-transaction negotiation, retained rollback guards and fail-closed validation, and updated the provider contract README. | ~1.5–2.5 h | ~0.75–1.25 h shared-target build wait and remote reconciliation | Focused TiDB 7/7, full locked workspace tests, strict Clippy, formatting/diff checks and security scan `5975446d-8bb7-457e-92d3-74c5c6ccf671` passed; implementation `0f95cb7d` is included in `origin/main` `0ca59c85`. |
| 2026-09-22 — Hosted run `35686340751` review | Refreshed the completed producer/aggregate job state, downloaded retained artifacts, parsed SQLite/PGlite/TiDB metrics and recorded the FoundationDB `--locked` preflight blocker. | ~0.75–1.25 h | ~0.5–1 h hosted runner/provider startup and artifact retrieval | Base Ozone passed; SQLite `997.06`, PGlite `592.43`, TiDB `277.59` IOPS missed the hard threshold; FoundationDB produced no benchmark; aggregate `106616062969` failed closed. Packet is diagnostic only. |
| 2026-09-22 — Current ledger reconciliation | Added the exact shared SHA, current work-item statuses, completion percentages, evidence, remaining actions, provisional estimates, external blockers and the latest hosted packet to this ledger; reconciled tracker status for other worktrees. | ~0.75–1.25 h | ~0.25–0.5 h remote status verification | Current authority is `0ca59c85`; production remains **NO-GO**. Next action is a fresh non-canceling W26 matrix on that exact SHA. |

## Current publication record — superseding older snapshots

The current shared build-on boundary is `origin/main` at
`0ca59c85a36227a07730f0282194ef7af8650fbf`, and this checkout matches it.
The W26 implementation commit `0f95cb7d` is the TiDB session-isolation
optimization; `ba4e89d0` is the SQLite metadata-publication CAS optimization;
`8428a5ef` is the FoundationDB test-lockfile correction. The last hosted run
`35686340751` selected earlier SHA `1e64bc25` and is diagnostic only. A fresh
matrix must be dispatched against the current exact SHA; no failed, queued,
partial or earlier-SHA result is promoted to acceptance.

## Current hosted dispatch override — run `35688061634`

After the ledger publication, concurrent mainline work advanced the shared tip
to `06fc70612b9387a281ab050f711fc878713177ea`. A fresh manual
`gh workflow run ci.yml --ref main` was dispatched and GitHub selected that
exact SHA. The run is currently **queued/in progress**, not acceptance evidence.

| Producer | Job ID | Current state | Acceptance rule |
| --- | ---: | --- | --- |
| Ozone base | `106619031921` | queued | Must pass gateway, policy, recovery and cleanup markers |
| Ozone compositions | `106619031684` | queued | Must pass SQLite/R2 and PGlite/R2 at >=1,000 IOPS plus all composition/end-to-end markers |
| Ozone TiDB | `106619031746` | queued | Must pass TiDB durable/restart/bounded-listing markers and >=1,000 IOPS |
| Ozone FoundationDB | `106619031804` | queued | Must pass lockfile/preflight, durable restart, bounded-listing, cleanup and >=1,000 IOPS |
| W26 aggregate | not created at capture | pending | Must verify all exact-SHA artifacts and emit the complete aggregate pass marker |

The required current code includes SQLite CAS `ba4e89d0`, TiDB isolation
`0f95cb7d` and FoundationDB lockfile `8428a5ef`. I will not promote any
producer result until its job and the aggregate are terminal, and the
production decision remains **NO-GO** while this run is queued or any hard gate
fails.

## Latest terminal qualification correction — run `35688061634`

The dispatch above is now terminal and supersedes its queued/in-progress state.
GitHub selected SHA `06fc70612b9387a281ab050f711fc878713177ea`, before the
published SQLite autocommit chunk `1e7e7571`, so this run remains diagnostic and
does not qualify the current shared tip.

| Producer | Job | Terminal result | Evidence |
| --- | ---: | --- | --- |
| Ozone base | `106619031921` | **PASS** | Gateway policy, block contract, failure window, restart/reopen and cleanup markers passed |
| Ozone compositions | `106619031684` | **FAIL** | SQLite/R2 `940.817629` IOPS; PGlite/R2 `1,004.326920`; both 1,200/1,200, timeout 0, cleanup failures 0; SQLite emitted `IOPS_TARGET_NOT_MET` |
| Ozone TiDB | `106619031746` | **FAIL** | TiDB/R2 `332.247378` IOPS, 1,200/1,200, timeout 0, cleanup failures 0; emitted `IOPS_TARGET_NOT_MET` |
| Ozone FoundationDB | `106619031804` | **FAIL** | FoundationDB/R2 `399.594055` IOPS, 1,200/1,200, timeout 0, cleanup failures 0; emitted `IOPS_TARGET_NOT_MET` |
| W26 aggregate | `106621589545` | **FAIL / fail closed** | `W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS`; no provider failure was promoted to pass |

The full local gates for the published `1e7e7571` chunk passed after the
hosted dispatch. The next action is a fresh exact-SHA W26 matrix; production is
still **NO-GO** until all configured providers meet 1,000 lifecycle IOPS and
the complete one-revision end-to-end aggregate passes.

## Latest session time log continuation — 2026-09-22

All estimates remain provisional. Hosted runner, provider startup, CI queue and
artifact retrieval time are external gate time, not implementation effort.

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — SQLite autocommit publication chunk | Changed the successful SQLite metadata publication path to a single autocommit fenced CAS UPDATE while retaining locked zero-row classification and fail-closed lease/revision semantics. | ~1–2 h | ~0.25–0.5 h shared-target lock wait | Focused SQLite tests 11/11, full locked workspace tests, strict workspace Clippy, formatting and diff checks passed. |
| 2026-09-22 — SQLite security review | Completed diff scan `1b6c1c72-1e6c-4d85-a008-5a8fced9e7c6` over the changed provider file and supporting publication path. | ~0.5–0.75 h | 0 h hosted | Complete coverage, three reviewed surfaces, zero reportable findings; sealed report retained in the security scan directory. |
| 2026-09-22 — Code publication/reconciliation | Committed, fetched concurrent mainline work, replayed the code commit after two non-fast-forward races and pushed the tested chunk to `origin/main`. | ~0.5–0.75 h | ~0.5–1 h remote reconciliation | `1e7e75716349f1eff09fd9b77bdeb652c8d0c1b2` verified on `origin/main`; no force push used. |
| 2026-09-22 — Hosted packet review | Downloaded and parsed composition, TiDB and FoundationDB artifacts; inspected the aggregate fail-closed log. | ~0.75–1.25 h evidence review | ~0.5–1 h hosted provider startup/artifact service | Terminal diagnostic packet recorded above; base passed, three provider rows missed the target, PGlite alone exceeded it. |
| 2026-09-22 — Ledger/tracker refresh | Added the current published tip, every W26 item’s status/evidence/remaining action/estimate/external gate, current production decision and the terminal packet to this ledger and `WORK_TRACKER.md`. | ~0.5–0.75 h | ~0.25–0.5 h current-tip verification | Documentation is the next push chunk; the next hosted matrix must use the exact current published SHA. |

## Current hosted dispatch — run `35689474986`

The fresh manual `ci.yml` dispatch selected exact SHA
`1891c36375296bc3695a9d71233c624cad46445c`, which contains the published
SQLite autocommit chunk and the current ledger/tracker. At capture, no producer
or aggregate result was acceptance evidence.

| Producer | Job ID | Capture state | Acceptance rule |
| --- | ---: | --- | --- |
| Ozone base | `106623193674` | queued | Must pass Ozone gateway policy, block contract, failure/restart/reopen and cleanup markers |
| Ozone compositions | `106623193668` | queued | SQLite/R2 and PGlite/R2 must each complete the fixed lifecycle and meet >=1,000 IOPS with all composition markers |
| Ozone TiDB | `106623193474` | in progress | Must pass durable TiDB/Ozone markers, bounded listing/reopen/fencing and >=1,000 IOPS |
| Ozone FoundationDB | `106623193564` | in progress | Must pass strict lockfile/preflight, durable restart/reopen/bounded listing/cleanup and >=1,000 IOPS |
| W26 aggregate | not yet created | pending | Must verify exact-SHA artifacts and emit the complete all-provider/end-to-end aggregate pass marker |

This packet remains **NO-GO** until every producer and the aggregate are
terminally successful on the same revision. Any provider failure, skip,
configuration failure, missing artifact or missing marker must remain a
fail-closed diagnostic result.

## Current exact-SHA provider-result correction — run `35689474986`

The four provider jobs have since become terminal on the selected SHA
`1891c36375296bc3695a9d71233c624cad46445c`. The aggregate job
`106625464560` is still queued, so this is a partial packet update and not an
acceptance result.

| Provider | Terminal result | Lifecycle evidence | Remaining action |
| --- | --- | --- | --- |
| SQLite/R2 | **PASS row**: `1,325.636778` IOPS, elapsed `905.225338 ms`; p95 write/read/delete `152.742148/16.861092/15.101845 ms` | 400 writes, 400 verified reads and 400 deletes; 1,200/1,200; timeout 0; cleanup failures 0 | Preserve this result and recheck it on the final aggregate packet; this is the first current-tip SQLite pass after `1e7e7571` |
| PGlite/R2 | **FAIL**: `766.631418` IOPS, elapsed `1,565.289358 ms`; p95 write/read/delete `258.600725/25.202828/15.859995 ms` | 1,200/1,200; timeout 0; cleanup failures 0; `IOPS_TARGET_NOT_MET` | Continue PGlite-specific publication/remote-latency investigation; do not average with SQLite |
| TiDB/R2 | **FAIL**: `292.606794` IOPS, elapsed `4,101.066772 ms`; p95 write/read/delete `778.754505/201.422311/65.283270 ms` | 1,200/1,200; timeout 0; cleanup failures 0; `IOPS_TARGET_NOT_MET` | Continue TiDB-specific performance work after the aggregate result; preserve durable/restart/fencing evidence |
| FoundationDB/R2 | **FAIL**: `410.703639` IOPS, elapsed `2,921.814870 ms`; p95 write/read/delete `457.390808/22.661206/41.907334 ms` | 1,200/1,200; timeout 0; cleanup failures 0; `IOPS_TARGET_NOT_MET`; durable/preflight/restart/bounded-listing markers passed | Continue FoundationDB-specific performance work; retain strict lockfile and durability markers |

The provider rows remain diagnostic until the aggregate verifies the complete
artifact set and marker contract. The current production decision is **NO-GO**:
one provider passes the hard row, three do not, and the aggregate is not
terminal.
