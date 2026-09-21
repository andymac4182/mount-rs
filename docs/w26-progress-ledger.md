# W26 progress ledger — Apache Ozone S3 backend

This ledger is the working record for the W26 Apache Ozone S3 backend
workstream. It distinguishes repository implementation, local evidence, and
hosted/native/provider acceptance. Estimates are provisional and are intended
for engineering planning, not a commitment.

## Snapshot

| Field | Current value |
| --- | --- |
| Workstream | W26 — Apache Ozone S3 backend |
| Ledger snapshot | 2026-09-22, Australia/Brisbane |
| Repository | `mount-rs` |
| Snapshot base | `6429c7ba` (published lazy-atime/EOF read-path code reconciled with concurrent `origin/main`; the exact remote tip is recorded in the publication/session rows below) |
| Checklist completion | 11 of 12 W26 tracker rows checked: shipped implementation rows are complete, while W26.15 (the terminal 1,000-IOPS remediation/qualification gate) remains open; hosted/provider production gates remain open |
| Provisional execution completion | W26 implementation scope: 100% for the 11 shipped tracker rows; current terminal qualification packet: **FAILED / NO-GO** because W26.15's hard performance gate is open; production-rollout readiness: 50% (scope, benchmark matrix, lifecycle safety, bounded remote enumeration, provider-bounded KV/N-API contract, durable-provider bounded-listing tests, strict provider-specific hard-threshold IOPS wiring, fail-closed artifact/profile/metric verification and retention, all-provider credential-free production-config policy, one-revision CI evidence-packet aggregation, complete end-to-end surface marker enforcement and the credential-free customer rollout contract are implemented and locally tested; no terminal production gates yet). Customer deployment, native, provider-durability, capacity/SLO, backup/DR and release-stream gates remain separately bounded |
| Current acceptance state | The retained W26 packet on `9c098e5` remains the last accepted hosted result within its documented provider/platform boundaries. The W26-owned optimistic read/write paths and atomic whole-file `FsDriver::write_file` contract remain published; the atomic implementation is in `96a25f17`, and the lazy-atime/EOF follow-up is in `d1bc8fb9`, reconciled and pushed in `6429c7ba`. The latest path coalesces read atime in coordinator state, avoids metadata publication for ordinary reads and EOF probes, and flushes pending atime through the existing fenced boundary on mutation, sync/fsync or graceful shutdown; its process-crash atime-loss boundary is explicit and does not weaken data durability. The full locked workspace test and strict Clippy commands both exited 0, including 16 ChunkedFs unit tests; environment-gated native/provider rows remain skips, not passes. Current run `35649202405` is terminal for every W26 producer and aggregate: base Ozone passed, all four configured provider rows completed 1,200/1,200 lifecycle operations with zero timeouts/cleanup failures but missed the hard 1,000-IOPS target, and the aggregate failed closed without a pass marker. Focused security diff scans `11fa7e54-bb66-488d-a369-c1a7ac46da81` and `5c0c59fb-a2c6-4e03-be6c-c334ca7cf0e7` completed with zero reportable findings within their stated scopes; the latest scan explicitly defers hosted/provider/customer controls and the lazy-atime crash boundary. No Live Cloudflare R2 or Live AWS S3 result is recorded. |
| Latest hosted workflow | Run `35649202405` on exact tested revision `e1765b7b996bae74b3720517d7cdbc72a7fa5e35`: `ozone` job `106497104104` passed; `ozone-compositions` `106497104006`, `ozone-tidb` `106497104443`, `ozone-foundationdb` `106497103955` and aggregate `w26-ozone-evidence` `106501216372` failed. The aggregate reason was `ozone-compositions-log-missing-marker=OZONE_IOPS_PASS` for SQLite/PGlite at target 1,000. |
| Current-head CI attempt | The atomic whole-file path improved the current hosted lifecycle measurement to SQLite/R2 91.94 IOPS and PGlite/R2 95.82 IOPS, while TiDB/R2 measured 16.10 and FoundationDB/R2 34.97; all remain far below 1,000. Each row recorded 400 writes, 400 reads, 400 verified reads and 400 deletes; 1,200/1,200 operations succeeded with zero timeouts and cleanup failures. The failure reason in every artifact was only `IOPS_TARGET_NOT_MET`; the current run is diagnostic, not acceptance. W26.15 remains open and the threshold is unchanged. |
| Next hosted qualification | Dispatch after this ledger publication against published tip `6429c7ba` | The next retained CI packet must rerun `ozone`, `ozone-compositions`, `ozone-tidb`, `ozone-foundationdb` and `w26-ozone-evidence` on one revision, retain all four provider JSON/log artifacts, and report whether the strict 1,000-IOPS gate closes after lazy atime/EOF publication reduction. Until then, current run `35649202405` is terminal diagnostic evidence only. | GitHub-hosted runners, Ozone/provider startup, provider latency and artifact retention are external gates; queued, canceled, failed or skipped jobs are not evidence |
| Local Docker boundary | Docker Desktop capacity was about 5 CPUs and 8.2 GiB; this is sufficient for the durable FoundationDB proof but below the TiDB harness's 10 GiB durable-topology minimum |
| Production rollout track | Open, currently **NO-GO**; 0 of 15 production gates are terminally accepted. The 50% figure reflects scope decisions, local hardening, provider-boundary implementation, strict provider-configuration and artifact-integrity/metric/retention qualification, expanded credential-free security policy, one-revision evidence-packet aggregation, complete end-to-end packet surface enforcement and the credential-free customer rollout contract, not deployable readiness |
| W26 production target | Customer-deployed Ozone integration; W26 owns provider/client correctness and CI qualification, not customer deployment, backup/DR or release promotion |
| Required service envelope | Target 1,000 IOPS per drive; Tier 1 99.99% reliability; 5-minute RPO and 5-minute RTO. RPO/RTO and availability remain dependent on the customer's Ozone topology and operations |
| Available qualification environment | CI only; no staging environment is available. Production-like evidence must therefore be achieved through controlled hosted CI/provider fixtures and clearly labeled customer-owned prerequisites |
| Release/acceptance decision | W26 implementation and local qualification controls remain accepted within their documented scope; production rollout remains **NO-GO** because current run `35649202405` terminally failed every hard 1,000-IOPS provider row and aggregate job `106501216372` failed closed. The lazy-atime/EOF implementation is locally tested, published at `6429c7ba` and security-scanned with zero reportable findings, but it has no hosted result yet. Earlier standard, policy, negative-path, strict-IOPS, evidence-packet and rollout-contract scans remain recorded above; no failed run is promoted, and no broader native, customer secure-runtime, Ozone backup/DR or release claim is made |

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
| W26.3a — SQLite and disk-backed PGlite composition over real Ozone | Functional composition complete; atomic whole-file write measured; hard IOPS qualification failed again | 100% functional; 88% production qualification | Current-tip `ozone-compositions` job `106497104006` on `e1765b7b` completed all SQLite/R2 and PGlite/R2 lifecycle operations with zero timeout/cleanup failures but measured 91.94 and 95.82 IOPS against 1,000; artifact `10661329293` contains `IOPS_TARGET_NOT_MET` and no `OZONE_IOPS_PASS`. Atomic whole-file commit `96a25f17` improved both rows over the prior 85.70/94.04 result. | Continue W26-owned metadata publication/concurrency work; retain the hard target and rerun the complete packet only after a substantive implementation change. | ~1–4 d follow-up performance/design; 0.5–1.5 d hosted review | Current Ozone fixture capacity/topology and customer target capacity remain external, but the remaining volume-wide publication cost is W26-owned and remains insufficient. |
| W26.3b — independent single-node TiDB composition over Ozone | Local smoke and contract complete; not replicated acceptance | 100% of this sub-item | Single-node TiDB/Ozone run passed TiDB identity/provider/fencing, ChunkedFs partial/truncate/CAS/stale-fencing/reopen, ambiguous commit, and cleanup. Evidence was explicitly labeled `single-node-smoke-not-replicated-acceptance`. | Preserve this as a lower-level contract signal only; it does not replace the durable hosted topology gate. | 0 h for current scope | Single-node TiDB is intentionally not a durability or failover claim. Local machine capacity is below the durable TiDB harness minimum. |
| W26.3c — independent durable three-node FoundationDB composition over Ozone | Durable provider lifecycle and restart markers passed; atomic write measured; hard IOPS gate remains open | 90% base; 84% extension | The arm64 durable FoundationDB proof and provider-backed bounded-listing contract remain green. Current hosted job `106497103955` completed all 1,200 lifecycle operations and measured 34.97 IOPS, with zero timeout/cleanup failures but `IOPS_TARGET_NOT_MET`; artifact `10661084241` is diagnostic only. The atomic whole-file path and lifecycle shutdown barrier are covered by local concurrency/full-workspace gates. Local FoundationDB binary linking remains blocked by missing `libfdb_c`. | Continue performance remediation while preserving durable restart, fencing, bounded-listing and cleanup assertions; rerun the provider packet only after a substantive change. | ~1–4 d shared concurrency follow-up; 0.75–1.5 h hosted review | FoundationDB client image/runtime, Ozone capacity/topology and customer secure durability remain external; local provider-native linking remains blocked. |
| W26.3d — durable multi-node TiDB composition over Ozone | Durable TiDB lifecycle and N-API bounded markers passed; atomic write measured; hard IOPS gate remains open | 100% base; 84% extension | Current hosted job `106497104443` completed all 1,200 lifecycle operations and measured 16.10 IOPS, with zero timeout/cleanup failures but `IOPS_TARGET_NOT_MET`; artifact `10661094464` is diagnostic only. The atomic path retains fencing/CAS and serialized conflict fallback, with local full-workspace tests and Clippy green. | Continue performance remediation while preserving restart/ambiguous-commit/fencing/bounded-listing/cleanup assertions; rerun the provider packet only after a substantive change. | ~1–4 d shared concurrency follow-up; 0.75–1.5 h hosted review | GitHub-hosted TiDB/PD/TiKV, N-API runtime, Ozone capacity/topology and customer secure durability remain external. |
| W26.4a — Node provider matrix and Rust/Node CLI coverage | Implementation and local/hosted composition gate complete | 100% | The live arm64 composition run passed Node SDK coverage including PGlite-to-S3 partial/truncate/reopen (`pass=7 skip=1 fail=0`), Node CLI coverage, and the matching ignored Rust CLI live Ozone split-provider/reopen test. Final hosted `ozone-compositions` job `106286459622` passed on `35585066458` with `SUMMARY node-sdk pass=7 skip=1 fail=0`. | Keep the Node matrix's one intentional skip and native/provider boundaries visible. | 0 h acceptance; 0–1 h review | Hosted NAPI/PGlite build and Linux Node runtime are provider/native gates. Local arm64 evidence is not Linux-amd64 evidence. |
| W26.4b — CI wiring, pinned images, cleanup, and documentation | Implementation complete; current-tip one-revision packet terminally failed the hard provider gate | 100% implementation; 63% hosted packet qualification | CI installs NAPI/PGlite with locked Rust dependencies, runs the Ozone composition matrix, Node/CLI coverage, the shipped HTTP server/client reopen path, and durable Ozone/TiDB/FoundationDB jobs. Run `35649202405` on exact revision `e1765b7b` retained all four producer artifacts (`10661562646`, `10661329293`, `10661094464`, `10661084241`); base Ozone passed, each provider missed 1,000 IOPS and aggregate job `106501216372` failed closed on the missing `OZONE_IOPS_PASS` marker. | Require a new substantive W26.15 implementation result, then rerun all producer jobs and `w26-ozone-evidence` on one revision; retain exact artifacts, policy markers, end-to-end markers, performance markers and cleanup before promotion. | ~1–4 d performance follow-up; 0.75–1.5 d hosted review per retained run | Workflow concurrency, hosted provider startup, Ozone capacity/topology, artifact service and customer TLS/IAM remain external; no failed/queued/skipped artifact is acceptance evidence. |
| W26.14 — complete end-to-end Ozone evidence-surface gate | Implementation complete; current-tip packet failed performance; customer admission open | 100% implementation; 44% production qualification | Expanded `scripts/verify-w26-ozone-evidence-packet.mjs` to require gateway health/ready/restart, SQLite/PGlite composition and bounded listing, Rust CLI, Node provider matrix/CLI, remote HTTP CLI, TiDB Rust/N-API seed/reopen and FoundationDB Rust/N-API seed/reopen markers. Local benchmark/evidence tests, Node/shell/YAML checks, diff checks, full locked workspace tests and strict Clippy pass; current run `35649202405` on `e1765b7b` retained all producer artifacts, but all four hard IOPS rows failed and aggregate `106501216372` reported the missing SQLite/PGlite pass marker. No packet result is promoted. | Close the packet only after a post-remediation run has terminal pass evidence from every producer and aggregate; retain native/mount and customer secure-runtime evidence separately. | ~1–4 d W26 performance follow-up; ~0.75–1.5 d hosted review; ~0.75–1.25 h security review | Ozone fixture capacity/topology, hosted provider/N-API execution, artifact authorization, native mount runners, customer TLS/IAM/rotation and measured SLO/RPO/RTO remain external. |
| W26.15 — per-drive 1,000-IOPS qualification and safe performance remediation | Lazy-atime/EOF publication reduction implemented and published; hard target still open; **NO-GO** | 68% W26-owned implementation / 31% hosted qualification | Implementation commits `96a25f17` and `d1bc8fb9` are published in reconciled remote tip `6429c7ba`. The latest `ChunkedFs::read_at` path skips EOF metadata work and coalesces read atime until mutation, sync/fsync or graceful shutdown, preserving the fenced namespace/revision and immutable-block contracts. The regression `reads_coalesce_atime_and_eof_does_not_publish` passes; full locked workspace tests and strict workspace Clippy exit 0; focused security scan `5c0c59fb-a2c6-4e03-be6c-c334ca7cf0e7` found zero reportable findings with hosted/customer boundaries partial. Hosted run `35649202405` predates this chunk and remains diagnostic at 91.94/95.82/16.10/34.97 IOPS; every row completed 1,200/1,200 operations with zero timeout/cleanup failures but failed `IOPS_TARGET_NOT_MET`, and aggregate `106501216372` failed closed. | Dispatch and inspect a complete one-revision Ozone packet on `6429c7ba`; retain exact provider metrics, latency, resource envelope, artifacts, cleanup and complete-surface markers. If the hard target remains open, continue correctness-preserving provider/publication work or obtain an Ozone/customer capacity qualification; do not lower the target or convert failed rows to skips. | ~1.5–4 d implementation/design; ~0.75–1.5 d hosted review per rerun; external queue time excluded | GitHub runner/provider startup, Ozone fixture capacity/topology, provider-native performance, artifact retention, customer secure topology and capacity are external gates; no staging environment exists. |
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
| W26 integration readiness | **NO-GO** | The retained green packet is still `35585066458`, but current-tip run `35649202405` on `e1765b7b` also failed closed: all four configured provider rows completed 1,200/1,200 lifecycle operations with zero timeouts/cleanup failures yet measured only 91.94/95.82/16.10/34.97 IOPS against 1,000, and aggregate job `106501216372` rejected the missing SQLite/PGlite pass marker. W26-owned packet, policy, metric, retention, end-to-end verifier and lazy-atime/EOF controls are implemented and locally tested at published tip `6429c7ba`; W26.15 is open for hosted qualification and any further correctness-preserving performance work | A current-tip retained revision must close W26.15 and produce terminal pass markers for every configured provider plus the full end-to-end packet; customer Ozone deployment, DR and release remain explicit external dependencies |
| Customer production target | Customer-deployed Ozone; topology not supplied | Product direction fixes the service envelope at 1,000 IOPS per drive, 99.99% reliability and five-minute RPO/RTO, but W26 does not operate the customer topology | W26 documents the Ozone/provider/client contract; customers and deployment streams provide secure topology, backup/DR, monitoring and measured availability/recovery evidence |
| Qualification baseline | Historical green packet only | Revision `9c098e5` and hosted run `35585066458` are the last accepted packet within their documented boundaries. Current run `35649202405` on exact revision `e1765b7b` is diagnostic and failed the hard provider-performance gate; published tip `6429c7ba` contains the lazy-atime/EOF reduction but has not yet been hosted-qualified | Any performance/concurrency change gets a fresh full packet on the tested revision; no failed, queued, canceled or skipped result is promoted |

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
| P2 — production metadata-provider support matrix | Hosted/provider CI | Provider matrix exercised; current terminal performance gate failed | 72% | The benchmark has explicit SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 rows. Strict configuration prevented skips, and current run `35649202405` exercised all four rows with complete lifecycle counts; SQLite/R2 91.94, PGlite/R2 95.82, TiDB/R2 16.10 and FoundationDB/R2 34.97 IOPS all missed 1,000. Provider-bounded listing and durable restart markers passed where reached; producer artifacts are diagnostic only. | Resolve or qualify the shared performance path, then retain terminal artifacts/markers for all four providers on one revision; record provider versions/HA/failure semantics and unsupported combinations, add capable KV/provider rows, and do not promote an explicitly skipped or failed provider | 2–5 d shared performance/design; 1–2 d hosted review | `ChunkedFs` metadata publication/provider latency is W26-owned; CI capacity, provider images/versions, Ozone capacity/topology and customer secure durability remain external |
| P3 — authentication, TLS, secret lifecycle and redaction | Implementation / hosted/provider CI | Local transport and expanded credential-free production-config policy hardened; secure integration gate open | 60% | R2/Ozone config parsing and runtime validation reject non-HTTP(S), embedded credentials, query/fragment, missing-authority and remote plaintext-HTTP endpoints before client construction; HTTP config and runtime now reject non-loopback binds, require loopback behind a TLS reverse proxy, keep credentials as environment references and redact diagnostics. The `b8d8fb1` policy gate validates SQLite/PGlite/TiDB/FoundationDB metadata shapes, HTTPS R2 blocks, exact external secret references and TiDB TLS options without provider connections; `7018b59` adds independent inline-secret, shared-provider-authority and TiDB hostname-verification negative cases. Local positive/negative policy checks passed | Add authenticated HTTPS endpoint CI where available, certificate identity/rotation checks, secret injection/rotation references, least privilege and clean-client negative tests; retain no secret values; review customer runtime configuration against the policy | 3–7 d | Secure Ozone CI endpoint or customer-supplied fixture, certificates/identity, secret manager integration, customer IAM and security review |
| P4 — replicated block durability and storage failure protection | Provider/customer deployment dependency plus CI contract | Lifecycle protection implemented; durability qualification open | 40% | Restart/reopen and durable provider checks pass in bounded CI topologies. The explicit reconciliation path rejects zero grace, protects committed and open-unlinked roots, retains a configurable grace window, streams the configured R2/Ozone prefix and scopes deletion to validated block IDs; no customer storage, power-loss or Ozone replication guarantee exists | Test client behavior for object loss, unavailable gateway, retries, integrity mismatch and recovery in CI; document Ozone replication/fsync/storage requirements, retention ownership, deletion authorization and space-pressure alerts that customers must satisfy | 1–3.5 d W26 CI work; deployment work external | Ozone storage and replication semantics, failure controls, retention policy and customer topology; CI restart is not power-loss evidence |
| P5 — fencing, ambiguous commit and stale-writer recovery under failover | Implementation / hosted/provider CI | Lease-protected reconciliation implemented; failover matrix open | 35% | W26 exercises CAS, stale fencing, ambiguous commit and durable restart in bounded provider compositions; hosted TiDB marker reports `ambiguous_commit=pass`; reconciliation renews the writer lease before deriving roots and never runs implicitly on shutdown | Extend all feasible Ozone/provider CI lanes with concurrent clients, retry, gateway/provider loss, delayed responses and post-ambiguity reconciliation; prove no stale publication, duplicate block or lost acknowledged commit | 3–7 d | Distributed CI fault controls, provider failover behavior and multiple-client scheduling |
| P6 — backup, restore, disaster recovery and retention | External dependency — Ozone/customer owned | Not a W26 implementation task | 0% W26 DR evidence | Product direction assigns backup and DR to Ozone/customer deployment; W26 has no competing backup system | Document the Ozone/customer requirements needed to meet 5-minute RPO/RTO and test W26 reopen/error behavior around supplied recovery scenarios when CI fixtures expose them | 0.5–1.5 d W26 contract documentation | Ozone backup/replication/restore design, failure domains, KMS and customer operations |
| P7 — observability, alerts, dashboards and runbooks | Implementation / CI contract / cross-workstream | Local HTTP/OTLP and provider-boundary evidence passed; deployment integration open | 45% | `mount-rs-http` passed 8 unit and 12 integration tests; the OTLP-enabled HTTP suite passed 11 integration tests including bounded error telemetry; full-feature observability passed 5 unit tests plus local collector and exporter-failure tests; CLI observability passed 44 unit, 9 CLI, 2 HTTP subprocess and 1 native-artifact test. `reconcileBlocks` returns scanned/protected/recent/deleted counts and fails closed when unsupported, while `readdir_bounded` is forwarded through observability/CLI wrappers. The N-API unstorage bridge also preserves the provider-boundary `EOVERFLOW` contract. These are local collector/fixture results, not deployed alerting. | Add/retain machine-readable Ozone/provider error categories, reconciliation and bounded-listing metrics, and health evidence in the hosted packet; coordinate dashboards, alerts and runbooks with W30/customer operations | 2–5 d W26 contract/tests | Collector reachability, alerting and paging are external; W30 and customer operations own deployed dashboards/paging |
| P8 — load, capacity, soak and cost envelope | Hosted/provider CI plus W26.15 remediation | Hard-threshold and one-revision packet gates implemented; current packet failed performance; lazy-atime/EOF reduction is locally verified but unqualified hosted | 52% | The benchmark records successful write+read+delete lifecycle IOPS, supports a hard `--min-iops` threshold, strict `--require-configured` provider qualification and redacted JSON. The fixed 4 KiB/400-iteration/concurrency-64 profile is enforced. Current run `35649202405` completed all 1,200 lifecycle operations per row with zero timeouts/cleanup failures but measured only 91.94/95.82/16.10/34.97 IOPS; aggregate job `106501216372` failed closed. Published `96a25f17` optimized whole-file publication, and `d1bc8fb9` now avoids per-read/EOF metadata publication while preserving explicit sync/shutdown durability. No hosted result exists yet for `6429c7ba`. | Run the current-tip all-provider packet, then retain p95/p99 latency, errors, CPU/memory and topology; add soak/capacity variants only after every hard provider row closes | 1.5–4 d W26 performance follow-up; 1–2 d hosted review; soak effort provisional | Ozone fixture capacity/topology, stable hosted runners, artifact service, provider quotas and customer 99.99% capacity remain external; do not lower the target |
| P9 — upgrade, rollback and compatibility | External release/deployment dependency | Not a W26 release task | 0% W26 migration evidence | W26 pins Ozone 2.2.1 and provider fixture versions for qualification only; release execution belongs to another stream | Supply compatibility notes, config/schema/object invariants and requalification commands for the release stream; do not own promotion or rollback automation here | 1–3 d W26 compatibility notes | Release stream, maintained provider versions, change window and customer deployment approval |
| P10 — security, privacy, tenancy and audit review | Published-tip local security review complete; hosted/customer security remains open | 82% | All prior scans remain zero-finding evidence within their scopes. The latest lazy-atime diff scan `5c0c59fb-a2c6-4e03-be6c-c334ca7cf0e7` reviewed `integrations/mount-rs-chunked/src/lib.rs`, completed with zero reportable findings, and explicitly modeled pending-atime state, stat visibility, fenced namespace publication, EOF behavior, sync/shutdown durability, lease/revision/orphan handling and the deliberate atime crash boundary. Local workspace tests and strict Clippy pass. Hosted/provider TLS/IAM, customer Ozone security, dependency/native provenance, 99.99%/recovery drills and production operations remain deferred. | Retain the scan with the published revision, exercise provider-native security and artifact authorization in hosted CI, and obtain customer Ozone endpoint/IAM/certificate/rotation evidence; preserve the fail-closed contracts and no-secret policy | 2.5–7 d | Customer identity/tenancy model, secure Ozone endpoint/certificates, provider-native allocation, compliance requirements, hosted CI and scanning infrastructure |
| P11 — end-to-end client, mount and platform qualification | Native/provider CI / cross-workstream | HTTP path and all implemented bounded-listing contracts added to Ozone CI; full matrix required | 46% | W26 covers Rust/Node/CLI and the shipped HTTP server/client path through the Ozone composition gate with scoped cleanup; built-in Rust providers enforce the directory bound before response materialization; the unstorage/N-API path has a provider callback, public `readdirBounded` API, Node error-shape test and TypeScript declaration; parity fixtures cover capable overflow/success and legacy fail-closed behavior; SQLite/PGlite plus the TiDB/RustFS and FoundationDB/RustFS composition tests assert provider-backed bounded success and `EOVERFLOW`; the feature-built FoundationDB and TiDB Node/N-API Ozone lanes now assert the same contract with scoped cleanup prefixes. TiDB compile/Clippy and FoundationDB check evidence pass locally, but live provider/Ozone execution is hosted-only. Native mount, providers without the callback, `MOUNTX_SOURCE` parity and every platform are not yet accepted | Review terminal Ozone evidence for HTTP and every bounded-listing marker and cleanup, add/retain bounded provider rows for key-value/JavaScript-owned metadata, run mountx parity and then exercise every advertised native/mount surface; retain platform/provider matrices, restart/recovery and negative capability evidence | 5–15 d depending on advertised platforms | macOS/Linux/Windows runners, privileged mount facilities, native workstreams, provider callback contracts, mountx source, signing and provider connectivity |
| P12 — release packaging, CI promotion, canary and rollback automation | External release stream | Not a W26 release task | 0% W26 release evidence | Product direction assigns releases to another stream; W26 hosted jobs provide qualification inputs only | Publish reproducible CI commands, version/image pins, evidence markers and compatibility notes for the release stream; no W26 canary claim | 0.5–2 d W26 handoff | CI/CD, artifact registry, signing keys, deployment platform and release owner |
| P13 — incident, failover and recovery rehearsal | External customer/Ozone operations plus CI fault contract | Not a W26 operator task | 0% W26 rehearsal evidence | W26 restart and cleanup tests are bounded qualification checks, not customer incident exercises | Add CI fault/recovery cases where controllable and document the operator scenarios customers must rehearse to meet 99.99% and 5-minute RTO | 1–3 d W26 fault contract | Customer on-call, Ozone operations, paging/incident tooling and maintenance windows |
| P14 — final W26 integration-readiness review | W26 implementation / hosted CI / handoff | NO-GO review documented; cannot close while W26.15 is open | 31% | Current packet `35649202405` is audited: base Ozone passed, all four provider lifecycles completed but missed the hard IOPS target, aggregate `106501216372` failed closed. Lazy-atime/EOF commit `d1bc8fb9` is published at `6429c7ba`, locally tested, strict-Clippy clean and security-scanned with zero reportable findings; no hosted result exists for the new tip. | Inspect the next one-revision packet, close W26.15 only with terminal pass markers for every provider and the full end-to-end surface, attach provider/platform/security/performance evidence, list customer/Ozone dependencies and hand off an explicit integration-ready or NO-GO decision to release/deployment streams | 1–2 d review after performance gate; 1.5–4 d W26.15 follow-up | All W26-owned CI gates plus external customer Ozone secure topology, capacity/SLO, DR and release-stream confirmations |

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
  correctly failed closed on replacement run `35641941218` (exact code head
  `88b707ba`) after measuring SQLite/R2 85.70, PGlite/R2 94.04, TiDB/R2
  14.50 and FoundationDB/R2 30.46 IOPS. All rows completed 1,200/1,200
  lifecycle operations with zero timeout/cleanup failures. The next published
  code head includes atomic whole-file write commit `96a25f17`; dispatch a
  fresh one-revision packet against it, then resolve or qualify any remaining
  shared concurrency/capacity blocker. Do not lower the threshold or convert
  failed rows to skips.
- [ ] P9/P12/P13: hand compatibility, CI evidence, customer incident scenarios
  and release inputs to the owning streams.
- [ ] P11: run every advertised Rust/Node/CLI/HTTP/native surface end to end
  through Ozone, retaining cross-workstream native blockers.
- [ ] P14: audit one retained CI revision and record W26 integration-ready or
  NO-GO before another stream promotes a release.

## Provisional remaining effort and blockers

| Category | Estimate | Notes |
| --- | --- | --- |
| Hosted result inspection | 0.5–1.5 d engineering plus external queue time | Historical green packet `35585066458` remains the last accepted hosted result. Current run `35649202405` on `e1765b7b` is terminal for all W26 jobs but diagnostic: base Ozone passed, all four provider rows completed 1,200/1,200 operations with zero timeouts/cleanup failures but missed 1,000 IOPS, and aggregate job `106501216372` failed closed. Atomic whole-file commit `96a25f17` is measured but insufficient; another current-tip retained packet is required after the next substantive change. |
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
| Performance and end-to-end matrix (P8, P11, P14, W26.15) | 12–32 d engineering | The workload and packet are implemented, but current terminal diagnostic run `35649202405` failed the hard provider target. Atomic whole-file write commit `96a25f17` improved SQLite/PGlite and durable-provider measurements to 91.94/95.82/16.10/34.97 IOPS, still far below target. The next design must reduce or safely defer more metadata publication while preserving POSIX/fencing/recovery semantics, then requires another current-tip packet, possible provider-latency work or production-like Ozone capacity qualification, stable hosted CI runners, native runners and a one-revision audit. |
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

## Publication record

This document is intentionally updated alongside `WORK_TRACKER.md`. The
ledger's percentages and estimates are snapshots; the tracker checkbox and
terminal hosted evidence remain authoritative for whether W26 is actually
complete.

Current-status override: historical session rows below preserve what was known
at the time they were written. The authoritative current state is the snapshot
and terminal diagnoses above: runs `35635486040`, `35641941218` and current-tip
`35649202405` are failed diagnostic packets, W26.15 is open, and no queued,
in-progress, canceled or failed job is promoted to acceptance. Current-tip
aggregate `106501216372` failed closed on the missing SQLite/PGlite pass marker;
that failure does not become acceptance evidence merely because unrelated
workflow jobs were still running at an earlier poll. Published tip `6429c7ba`
contains the lazy-atime/EOF remediation and has local test, Clippy and focused
security evidence, but no hosted qualification result yet; the next packet
must bind its artifacts to that revision (or a later explicitly recorded
reconciliation) before W26.15 can move.

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
