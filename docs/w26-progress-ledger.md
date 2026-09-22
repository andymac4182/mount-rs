# W26 progress ledger — Apache Ozone S3 backend

This ledger is the working record for the W26 Apache Ozone S3 backend
workstream. It distinguishes repository implementation, local evidence, and
hosted/native/provider acceptance. Estimates are provisional and are intended
for engineering planning, not a commitment.

## Current authority override — 2026-09-22, optimistic preparation snapshot chunk

This is the newest W26 implementation boundary. Source commit
[`c1d3037ad3e179a2df01005f1b72295f08f0887b`](https://github.com/andymac4182/mount-rs/commit/c1d3037ad3e179a2df01005f1b72295f08f0887b)
(`perf(w26): overlap whole-file preparation snapshots`) is published at
`origin/main`. The 13-line runtime change adds a dedicated lease-renewal
gate, serializes lease renew/validation provider mutations, and removes only
the read-only initial `write_file_atomic` snapshot from the global mutation
gate. The state mutex still makes the snapshot coherent; the existing
mutation-batch revision/CAS, create rebase, conflict fallback, fenced
publication, lifecycle lock, shutdown ordering and fail-closed paths remain
the commit boundary. This allows concurrent whole-file operations to overlap
preparation without allowing concurrent lease mutations.

| Gate / item | Current result | Evidence | Remaining action / ownership |
| --- | --- | --- | --- |
| Source implementation | **PUBLISHED / 100% for this chunk** | `c1d3037a` is at both the detached checkout and `origin/main`; `cargo fmt --all` and `git diff --check` pass. | Requalify the real Ozone/provider path on this exact source. |
| Chunked focused test gate | **PASS** | `./scripts/cargo-shared test -p mount-rs-chunked --lib --locked`: 22 passed, 0 failed, 0 ignored, including lease-loss, shutdown, revision conflict, batch publication and preparation-window tests. | Hosted/provider execution remains separate. |
| Full workspace locked test gate | **PASS** | `./scripts/cargo-shared test --workspace --all-targets --locked` completed with executed suites passing; explicitly opt-in native/live provider tests remain ignored where external services or host privileges are not configured. | Do not promote ignored native/provider cases as hosted acceptance. |
| Focused/full strict Clippy | **PASS** | Focused chunked Clippy and `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings` both exited successfully. | No local action; retain hosted evidence boundary. |
| Security diff scan | **PASS — 0 findings / complete coverage** | Scan `86a4e46e-caef-4d9b-8ab7-aeba21571d80` covered the one changed source file with complete coverage and zero findings; snapshot `codex-security-snapshot/v1:sha256:aaa80da6e5eca5d80170d0a6ba50f6c3b967d02d821fbb0ceda46eb7d0821d2e`; report `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-7kSFBv/mount-rs/186b0d081086abbf91bafd80017a15bc2ad71288_20260922T120042Z_ughpz8yq/report.md`. | Customer certificate/IAM, secret rotation, tenant isolation and provider-native controls remain separate production gates. |
| Exact-head manual W26 qualification | **PENDING — run `35724922283`** | Manual run [`35724922283`](https://github.com/andymac4182/mount-rs/actions/runs/35724922283) has exact workflow head `c1d3037ad3e179a2df01005f1b72295f08f0887b`. At 22:03 AEST, `ozone-foundationdb` (`106736188870`), `ozone-tidb` (`106736188994`), `tidb-rustfs` (`106736189003`), `ozone` (`106736189040`), `ozone-compositions` (`106736189052`), `foundationdb-rustfs` (`106736189166`) and `tidb` (`106736189217`) were queued. | Wait for terminal all-provider/base/aggregate artifacts from this exact run; queued is not acceptance. |
| Production readiness | **NO-GO** | Local correctness/security gates are positive, but the exact source has no terminal Ozone/provider packet. The hard 1,000 IOPS/drive target, aggregate/e2e packet, Tier-1 99.99% reliability, five-minute RPO/RTO and customer-deployed Ozone security evidence remain open; Ozone/customer backup/DR and the separate release stream remain external. | Continue W26 compatibility and qualification only; do not claim customer deployment readiness. |

### Work-item delta and provisional estimates

The complete W26.1–W26.15/P14 table below remains authoritative for every
work item. This chunk improves the shared optimistic preparation path but does
not promote a hosted performance result or change provider acceptance counts.

| Work item | Status / completion after this chunk | Evidence and remaining action | Provisional engineering time / external blocker |
| --- | --- | --- | --- |
| W26.1/W26.2 — base Ozone and immutable-block contract | **OPEN / 100% implementation; exact-head hosted evidence pending** | Lease mutations are serialized independently while read-only preparation snapshots overlap; run `35724922283` is queued on `c1d3037a`. | 0.25–0.75 d per hosted qualification/remediation cycle; CI/Ozone topology are external. |
| W26.3a–d — SQLite, PGlite, TiDB and FoundationDB compositions | **OPEN / 100% implementation; hard-target acceptance unchanged** | No provider metric is available for `c1d3037a`; require terminal finite rows for all four feasible providers at or above 1,000 IOPS/drive. | 0.5–1.5 d per cycle; provider startup, Ozone/R2 latency and hosted runners are external. |
| W26.4/W26.8 — end-to-end and requested-provider/no-skip qualification | **OPEN / 100% verifier; exact packet pending** | Local tests pass, but only the exact manual run can provide hosted functional/restart/cleanup/authority/lease markers and no-skip provider rows. | 0.25–0.75 d review; CI scheduling and provider services are external. |
| W26.11/W26.14 — aggregate and end-to-end packet | **OPEN / 100% fail-closed verifier; pending** | Run `35724922283` has no terminal aggregate result; require all retained artifacts on one accepted workflow revision. | 0.5–1.5 d review; GitHub artifact service, native runners and Ozone topology are external. |
| W26.15 — per-drive hard IOPS gate | **OPEN / 100% verifier; current acceptance remains 1/4 from prior terminal evidence** | This change is not a performance pass. Require four individual finite rows at or above 1,000 IOPS/drive; do not average, lower or skip. | 1.5–4 d per cycle plus queue; provider/Ozone performance and artifact retention are external. |
| W26.7/W26.10/W26.13 — security, retention and customer rollout contract | **OPEN for production / source-diff gate positive** | Complete local diff scan is clean; customer TLS/IAM/rotation/tenant isolation, artifact retention and customer-run deployment evidence remain open. | 0.5–2 d review; customer/Ozone controls and deployment environment are external. |
| P14 — final integration-readiness review | **NO-GO / 50% provisional** | The source/runtime chunk is published and locally validated, but hosted provider/performance/aggregate and customer production gates are not closed. | 1–2 d after W26.15; customer SLO/RPO/RTO, security, backup/DR and release-stream gates are external. |

### Session time log — optimistic preparation snapshot chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — diagnosis/design | Reviewed the global gate, lease renewal cache, mutation runner and optimistic conflict paths; identified the initial whole-file snapshot as the remaining serialized preparation section. | ~0.5–0.75 h | 0 h | Change scope limited to read-only preparation plus a dedicated lease-mutation gate. |
| 2026-09-22 — implementation | Added `lease_gate`, wrapped renew/validate operations, and moved the initial `write_file_atomic` snapshot outside the global mutation gate with an explicit optimistic-CAS comment. | ~0.25–0.5 h | 0 h | Provider lease mutations remain serialized; publication semantics unchanged. |
| 2026-09-22 — local verification | Ran formatting/diff checks, focused 22-test chunked suite, focused Clippy, full locked workspace tests and full workspace strict Clippy. | ~0.75–1 h | ~0.25–0.5 h shared Cargo target wait | Local Rust gates passed; opt-in provider/native tests remain external. |
| 2026-09-22 — security review | Completed preflight, threat model, one-file review and scan `86a4e46e-caef-4d9b-8ab7-aeba21571d80` with complete coverage and zero findings. | ~0.25–0.5 h | ~0.1–0.25 h security workbench finalization | No security candidate survived; hosted/customer controls remain separate. |
| 2026-09-22 — source publication | Committed the runtime chunk, rebased over concurrent mainline updates through `672564b0`, pushed `c1d3037a`, and verified local `HEAD == origin/main` clean. | ~0.25–0.5 h | ~0.1–0.25 h mainline reconciliation | Other threads can build from the exact runtime tip. |
| 2026-09-22 — hosted dispatch (22:03 AEST) | Dispatched manual run `35724922283` from `main`, verified exact head `c1d3037a` and all seven W26 jobs queued. | ~0.1 h | CI/provider capacity pending | The run is the authoritative hosted boundary for this source chunk. |
| 2026-09-22 — next gate | Poll `35724922283` with bounded waits; retrieve exact provider artifacts after terminal completion, classify all four rows against 1,000 IOPS/drive, then update W26.15/W26.14/P14. | ~0.5–1.5 d provisional | ~0.5–2 h provisional hosted wait | Keep production **NO-GO** until the complete packet and customer gates close. |

## Current authority override — 2026-09-22, hosted queue recheck after bounded watch

At 21:52 AEST, stable manual run
[`35723306179`](https://github.com/andymac4182/mount-rs/actions/runs/35723306179)
still reports exact head `73534bce21368b976e0bcfb06a61fd62da853128`, overall
status `queued`, and no conclusion. A bounded five-minute live watch observed
no job start; a direct status query then confirmed all seven W26 jobs remain
queued: `foundationdb-rustfs` `106730959371`, `ozone-tidb` `106730959495`,
`ozone-foundationdb` `106730959622`, `ozone-compositions` `106730959643`,
`ozone` `106730959647`, `tidb` `106730959786` and `tidb-rustfs`
`106730959954`. The local watch was stopped only; the hosted run was not
canceled. This is unchanged external runner/provider capacity, not a provider
failure or a qualification result.

| Gate / item | Current result | Evidence | Remaining action / ownership |
| --- | --- | --- | --- |
| Stable hosted qualification | **PENDING / unchanged** | Run `35723306179` is still queued after the bounded watch; exact head remains the current published shared tip. | Wait for terminal W26 provider/base/aggregate jobs and retain exact artifacts. |
| Work-item completion | **UNCHANGED** | W26.1–W26.15/P14 percentages and local PASS evidence are unchanged; no hosted provider metric exists to advance W26.15. | Do not change completion percentages or promote queued state. |
| Production readiness | **NO-GO / unchanged** | No terminal all-provider packet; 1,000 IOPS/drive, aggregate/e2e, Tier-1 99.99%, five-minute RPO/RTO and customer/Ozone security gates remain open. | Keep the goal active and continue polling/implementation when hosted capacity or new evidence changes. |

### Session time log — hosted queue recheck after bounded watch

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — bounded hosted watch (21:47–21:52 AEST) | Watched manual run `35723306179` at 30-second intervals for approximately five minutes, then confirmed its seven W26 jobs are still queued. | ~0.1 h | ~0.1 h hosted capacity wait | No hosted state change; the run remains the authoritative pending boundary. |
| 2026-09-22 — next gate | Leave the hosted run intact, poll with bounded waits, and retrieve exact provider artifacts only after terminal completion. | ~0.1–0.25 h per recheck | External runner/provider capacity | Keep production **NO-GO** until terminal evidence closes the packet. |

## Current authority override — 2026-09-22, stable manual hosted qualification dispatch

At the 21:46 AEST recheck, `origin/main` and the detached checkout were both
at [`73534bce21368b976e0bcfb06a61fd62da853128`](https://github.com/andymac4182/mount-rs/commit/73534bce21368b976e0bcfb06a61fd62da853128)
(`docs(w26): record preparation-window regression coverage`). This tip contains
the published active-preparation regression commit `bd28cf46` and the full
W26 ledger/tracker update. A new workflow-dispatch run
[`35723306179`](https://github.com/andymac4182/mount-rs/actions/runs/35723306179)
was dispatched from `main` and reports exact workflow head
`73534bce21368b976e0bcfb06a61fd62da853128`. It is now the stable hosted
qualification boundary for this W26 state; unlike push-triggered descendants,
it is not being treated as disposable concurrency noise.

| Gate / item | Current result | Evidence | Remaining action / ownership |
| --- | --- | --- | --- |
| Published source and ledger ancestry | **PASS** | `HEAD == origin/main == 73534bce`; the runtime source and regression test are present, and the ledger/tracker are published for other threads. | Continue from this shared tip; no source change is implied by the dispatch. |
| Stable manual W26 hosted run | **PENDING — run `35723306179`** | Exact-head workflow-dispatch run `35723306179` is queued. W26 jobs `ozone-tidb` (`106730959495`), `ozone-foundationdb` (`106730959622`), `ozone-compositions` (`106730959643`), `ozone` (`106730959647`), `tidb` (`106730959786`), `tidb-rustfs` (`106730959954`) and `foundationdb-rustfs` (`106730959371`) were all queued at capture. | Wait for every provider/base/aggregate job to reach terminal state; accept only complete artifacts from this exact run. |
| Hosted runner/provider capacity | **BLOCKED — external scheduling gate** | The stable run has no provider, performance or aggregate result yet. The older exact-head run `35720016370` also remains queued; that run is bound to the preceding runtime head and is not the acceptance boundary for this ledger state. | Allow hosted capacity and provider services to execute; do not cancel or substitute unrelated runs. |
| W26.15 hard IOPS / aggregate / end-to-end | **OPEN — no percentage change** | No metric or aggregate artifact exists for `35723306179` yet. Require four finite provider rows at or above 1,000 IOPS/drive plus all functional, restart, cleanup, authority and lease markers. | Retrieve and classify each artifact; no averaging, skipping or queued-result promotion. |
| Production readiness | **NO-GO** | Local gates are green, but hosted Ozone/provider evidence is non-terminal. Tier-1 99.99% reliability, five-minute RPO/RTO, customer-deployed Ozone security, customer/Ozone backup/DR and separate release-stream evidence remain open. | Continue W26 compatibility/qualification only; customers deploy Ozone and Ozone/customer owns backup/DR. |

### Session time log — stable manual hosted qualification dispatch

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — hosted status recheck (21:42–21:46 AEST) | Inspected run `35720016370`; its exact-head provider/base/aggregate jobs remain queued. | ~0.1 h | Hosted capacity pending | No older queued result was promoted. |
| 2026-09-22 — stable dispatch (21:46 AEST) | Dispatched manual run `35723306179` from current `origin/main` `73534bce`; verified exact head and all seven W26 provider/base/aggregate jobs queued. | ~0.1 h | CI/provider queue pending | The new manual run is the authoritative hosted boundary for the current ledger state. |
| 2026-09-22 — next gate | Poll `35723306179` with bounded waits; download exact artifacts only after terminal completion, classify each provider against 1,000 IOPS/drive, then update W26.15/W26.14/P14. | ~0.5–1.5 d provisional | ~0.5–2 h provisional hosted wait | Keep production **NO-GO** until the complete packet and customer gates close. |

## Current authority override — 2026-09-22, active-preparation regression-coverage chunk

This is the newest published W26 boundary. Source commit
[`bd28cf46569ec94a481f97eab1dbf8e2b27d0515`](https://github.com/andymac4182/mount-rs/commit/bd28cf46569ec94a481f97eab1dbf8e2b27d0515)
(`test(w26): cover active mutation preparation window`) is at both the detached
checkout and `origin/main`. It adds a focused regression test proving that the
mutation runner remains pending after the initial adaptive idle window while a
peer whole-file operation is still preparing immutable blocks, then publishes
the queued mutation once that preparation is released. This is test coverage
only; the preceding bounded-preparation implementation and its fenced
metadata-publication, lease, revision/CAS and fail-closed boundaries are
unchanged. The commit was rebased over the concurrent shared-mainline tip
`1f8dcd4197a5f6cd36a519f19026d4d57d34a556` before the push.

| Gate / item | Current result | Evidence | Remaining action / ownership |
| --- | --- | --- | --- |
| Source/test implementation | **PUBLISHED / 100% for this chunk** | `bd28cf46` is verified at `origin/main`; the change is one 40-line regression test in `integrations/mount-rs-chunked/src/lib.rs`. `cargo fmt --all` and `git diff --check` pass. | Keep the runtime implementation boundary unchanged; hosted provider qualification remains required. |
| Chunked Rust focused test gate | **PASS** | `./scripts/cargo-shared test -p mount-rs-chunked --lib --locked`: 22 passed, 0 failed, 0 ignored; this includes `mutation_runner_waits_for_active_preparation_past_initial_idle_window`. | Retain the focused regression in the hosted CI path; do not promote local tests to Ozone/provider acceptance. |
| Chunked strict Clippy gate | **PASS** | `./scripts/cargo-shared clippy -p mount-rs-chunked --lib --locked -- -D warnings` completed successfully. | No local action; hosted qualification remains open. |
| Full locked Rust workspace test gate | **PASS** | `./scripts/cargo-shared test --workspace --all-targets --locked` exited 0; executed workspace suites passed. Explicitly opt-in native/live provider tests remained ignored because their external services or host privileges were not configured. | Hosted Ozone/provider execution remains required; ignored native/provider cases are not promoted as passes. |
| Full workspace strict Clippy gate | **PASS** | `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings` exited 0. | No local action; retain the hosted/provider evidence boundary. |
| Security diff scan | **PASS — 0 findings / complete coverage** | Scan `d72a974f-0b62-4b30-a6d2-dbd848a37b38` covered the one changed source file with complete coverage and zero findings; snapshot digest `codex-security-snapshot/v1:sha256:4f16f3f630af339c7d6131db71d24c5755ac8d4efeb4884c5cf0002e245631b8`. The sealed report is retained under the security-scan temporary report directory. | Customer certificate/IAM, secret rotation, tenant isolation and provider-native controls remain separate production gates. |
| Prior exact-head Ozone qualification | **PENDING — run `35720016370`** | Manual CI run [`35720016370`](https://github.com/andymac4182/mount-rs/actions/runs/35720016370) is bound to exact workflow head `9f6041db2f8aba9301507bad24664015890295f9`, the preceding runtime source commit. Its SQLite/base, PGlite, TiDB, FoundationDB and aggregate-related jobs remain queued at the latest recheck; it does not contain this test-only descendant commit. | Wait for terminal all-provider/base/aggregate artifacts; accept no queued result and do not mislabel this source-test commit as having hosted execution until an exact descendant run is terminal. |
| New automatic descendant CI | **PENDING — run `35722832900`** | Push-triggered run [`35722832900`](https://github.com/andymac4182/mount-rs/actions/runs/35722832900) has exact workflow head `bd28cf46569ec94a481f97eab1dbf8e2b27d0515` and was pending at 21:41 AEST. It is a queue observation only; later documentation pushes may supersede/cancel push-triggered runs through repository concurrency. | After the ledger publication, use a manually dispatched descendant as the stable hosted evidence boundary if the queued W26 provider jobs have not reached terminal state. |
| Production readiness | **NO-GO** | Local implementation, full workspace tests/Clippy and the complete source-diff security scan are positive, but no terminal exact-descendant Ozone/provider packet exists. The 1,000 IOPS/drive gate, aggregate/e2e packet, Tier-1 99.99% reliability, five-minute RPO/RTO and customer-deployed Ozone security evidence remain open; backup/DR and release execution remain external. | Continue W26-owned compatibility and qualification until every feasible provider and the end-to-end packet pass. Customers deploy Ozone; Ozone/customer owns backup/DR; another stream owns releases. |

### Work-item delta and provisional estimates

The complete W26.1–W26.15/P14 itemized table below remains the ledger of
every work item. This test-only chunk advances regression evidence without
changing hosted acceptance percentages or production-gate ownership.

| Work item | Status / completion after this chunk | Evidence and remaining action | Provisional engineering time / external blocker |
| --- | --- | --- | --- |
| W26.1/W26.2 — base Ozone and immutable-block contract | **OPEN / 100% implementation and local regression coverage; hosted acceptance pending** | The new test exercises the active-preparation scheduling boundary while preserving the existing fenced publication contract. Run `35720016370` is still queued on the preceding runtime source; run `35722832900` is pending on `bd28cf46`. | 0.25–0.75 d per hosted qualification/remediation cycle; Ozone topology and CI capacity are external. |
| W26.3a–d — SQLite, PGlite, TiDB and FoundationDB compositions | **OPEN / 100% implementation; hosted acceptance unchanged and pending** | The test prevents a queue-idle race in the shared chunked layer, but it supplies no provider performance result. Require terminal finite rows for all four providers at or above 1,000 IOPS/drive. | 0.5–1.5 d per qualification/remediation cycle; provider startup, Ozone/R2 latency and hosted runners are external. |
| W26.4/W26.8 — end-to-end and requested-provider/no-skip qualification | **OPEN / 100% verifier; exact descendant packet pending** | Local focused and workspace tests pass; queued/pending CI is not acceptance. Preserve the no-skip rule and require functional, restart, cleanup, authority, lease and provider markers on one accepted workflow revision. | 0.25–0.75 d review; CI scheduling and provider startup are external. |
| W26.11/W26.14 — aggregate and end-to-end packet | **OPEN / 100% fail-closed verifier; aggregate pending** | No aggregate result is promoted from run `35720016370` or `35722832900` while jobs are non-terminal. | 0.5–1.5 d review; GitHub artifacts, native runners and Ozone topology are external. |
| W26.15 — per-drive hard IOPS gate | **OPEN / 100% verifier; current acceptance remains 1/4 from the last terminal packet** | The regression does not alter performance claims. The next accepted packet must show four finite provider rows at or above 1,000 IOPS/drive; no averaging, lowering or skipping is permitted. | 1.5–4 d per cycle plus queue; provider/Ozone performance and artifact retention are external. |
| W26.7/W26.10/W26.13 — security, retention and customer rollout contract | **OPEN for production / local source gate positive** | The one-file scan is clean, but customer TLS/IAM/rotation/tenant controls, artifact retention, customer-run deployment evidence and operational ownership remain open. | 0.5–2 d review; customer/Ozone controls and deployment environment are external. |
| P14 — final integration-readiness review | **NO-GO / 50% provisional** | This chunk strengthens regression coverage only. Production remains blocked by hosted provider/performance/aggregate evidence and customer production gates. | 1–2 d after W26.15; customer SLO/RPO/RTO, security, backup/DR and release-stream gates are external. |

### Session time log — active-preparation regression-coverage chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — regression implementation | Added a deterministic no-waker regression that holds an active whole-file preparation across the adaptive idle window, proves the queued unlink is still pending, then releases the preparation and verifies one publication. | ~0.25–0.5 h | 0 h | The race boundary is now directly covered without changing production behavior. |
| 2026-09-22 — local validation | Ran formatting/diff checks, the focused 22-test library suite, focused strict Clippy, the full locked workspace all-target test matrix and full workspace strict Clippy. | ~0.5–0.75 h | ~0.1–0.25 h shared Cargo target wait | All local Rust gates exited successfully; opt-in native/live provider tests remain separate. |
| 2026-09-22 — source security review | Ran the configured security diff workflow for the one changed file; scan `d72a974f-0b62-4b30-a6d2-dbd848a37b38` sealed with complete coverage and zero findings. | ~0.25–0.5 h | ~0.1–0.25 h security workbench finalization | No security candidate survived; customer/provider controls remain separately tracked. |
| 2026-09-22 — source publication (21:35–21:40 AEST) | Committed `3754eac9`, fetched the concurrent `origin/main` tip `1f8dcd41`, rebased, pushed published `bd28cf46`, and verified local `HEAD == origin/main` with a clean worktree. | ~0.25–0.5 h | ~0.1–0.25 h mainline reconciliation | Other threads can build from the published test chunk. |
| 2026-09-22 — hosted recheck (21:41 AEST) | Queried the CI queue after publication: automatic descendant run `35722832900` has exact head `bd28cf46` and is pending; the prior manually dispatched provider run remains queued on `9f6041db`. | ~0.1 h | Hosted runner/provider capacity pending | No provider, performance or aggregate result is promoted. |
| 2026-09-22 — ledger publication | Update this ledger and `WORK_TRACKER.md`, commit and push them as a separate docs chunk; then dispatch/recheck a stable manual descendant if the hosted queue remains non-terminal. | ~0.25–0.5 h provisional | ~0.5–2 h provisional CI/provider wait | Keep production **NO-GO** until the complete packet and customer gates close. |

## Current authority override — 2026-09-22, bounded remote preparation-wave scheduling chunk

This is the newest implementation and qualification boundary. Source commit
[9f6041db2f8aba9301507bad24664015890295f9](https://github.com/andymac4182/mount-rs/commit/9f6041db2f8aba9301507bad24664015890295f9)
(perf(w26): coalesce remote mutation preparation waves) is verified at both
the detached checkout and the current origin/main history. Current
At the 21:28 AEST recheck, origin/main was 2bb846247aca02e9ce47a80a808314ca4beeed29,
which contains this source commit plus the ledger correction and unrelated
earlier work; the authoritative qualification run below is bound to the exact
9f6041db source head. It adds a bounded in-flight
preparation counter for whole-file writes so the mutation runner does not
declare the queue idle while peer operations are still preparing immutable
remote blocks. The guard is released before the prepared request waits for
the shared response; lease renewal, revision checks, fenced metadata
publication, block flush ordering, conflict fallback and fail-closed handling
are unchanged.

| Gate / item | Current result | Evidence | Remaining action / ownership |
| --- | --- | --- | --- |
| Source implementation | **PUBLISHED / 100% for this chunk** | 9f6041db is included in current origin/main history; git diff --check and cargo fmt --all -- --check pass. The exact qualification run is bound to 9f6041db. | Requalify the real Ozone/provider path on this exact source. |
| Chunked Rust unit gate | **PASS** | ./scripts/cargo-shared test -p mount-rs-chunked --lib --locked: 21 passed, 0 failed. | Workspace/hosted gates remain separate; retain the exact source boundary. |
| Chunked strict Clippy gate | **PASS** | ./scripts/cargo-shared clippy -p mount-rs-chunked --lib --locked -- -D warnings completed successfully. | No local action; hosted qualification remains open. |
| Full locked Rust workspace test gate | **PASS** | ./scripts/cargo-shared test --workspace --all-targets --locked exited 0; executed workspace suites passed, while explicitly opt-in native/live provider tests remained ignored because their external services or host privileges were not configured. | Hosted Ozone/provider execution remains required; ignored native/provider cases are not promoted as passes. |
| Full workspace strict Clippy gate | **PASS** | ./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings exited 0. | No local action; retain hosted/provider evidence boundary. |
| Security diff scan | **PASS — 0 findings / complete coverage** | Scan 8f4fb7da-a251-4b3b-8843-1ee87d25724c sealed with one changed-file surface, snapshot codex-security-snapshot/v1:sha256:afa23aa9a18a85096107c642e970cfbea760d566154e45dc716fa883393c05b4, findings SHA 6b7427d0359e50f49f96f11a25b45b262e51e34c9fb37801638876a702788ad0, coverage SHA 43f6e2da64b46f64f96afc696c293b521020b521ac51f56ac4fd8b4ac874df6d, manifest SHA 5b974adbc84f87b2b0c271ea60256e209dd1998c8008e54cb79b4109ab723473; report is under /private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/aa1c12a8f7d3a7c9a2399403ff3e40ff22edec21_20260922T110256Z_is0wbsnb/report.md. | Customer certificate/IAM, secret rotation, tenant isolation and provider-native controls remain separate production gates. |
| Fresh exact-head Ozone qualification | **PENDING — run 35720016370** | Manual CI run [35720016370](https://github.com/andymac4182/mount-rs/actions/runs/35720016370) has exact workflow head 9f6041db2f8aba9301507bad24664015890295f9. Provider jobs are ozone-tidb 106720478264, ozone-foundationdb 106720478642, ozone 106720478677, and ozone-compositions 106720478705; all were queued at capture. | Wait for terminal SQLite, PGlite, TiDB, FoundationDB, base and aggregate artifacts; accept no queued result. |
| Hosted runner capacity | **BLOCKED — external scheduling gate** | The exact-head run is workflow-dispatch triggered and currently queued across its jobs; this is runner/provider startup state, not a test failure or provider result. | Allow hosted capacity to execute; do not cancel or substitute unrelated runs. |
| Production readiness | **NO-GO** | The new source has no terminal hosted evidence yet. Prior terminal provider rows remain below the 1,000 IOPS/drive target for three of four providers; Tier-1 99.99% reliability, five-minute RPO/RTO, customer-deployed Ozone security and Ozone/customer-owned backup/DR remain open. | Continue W26-owned implementation/qualification until all feasible providers and the end-to-end packet pass; deployment, backup/DR and releases remain external. |

### Work-item delta and provisional estimates

The complete W26.1–W26.15/P14 itemized table below remains the ledger of
every work item. This chunk directly advances the implementation evidence for
the provider-composition, no-skip qualification, aggregate/end-to-end,
per-drive IOPS and final integration-readiness rows; it does not promote
their hosted acceptance percentages.

| Work item | Status / completion after this chunk | Evidence and remaining action | Provisional engineering time / external blocker |
| --- | --- | --- | --- |
| W26.3a–d — SQLite, PGlite, TiDB and FoundationDB compositions | **OPEN / 100% implementation for this scheduling chunk; hosted acceptance pending** | The preparation counter preserves the existing fenced publication boundary and is published at 9f6041db; run 35720016370 is the exact-head provider gate but has no terminal artifacts. | 0.5–1.5 d per qualification/remediation cycle; Ozone/R2 latency, provider startup and hosted runners are external. |
| W26.7 — security/configuration policy | **OPEN for production / 100% source-diff security gate for this chunk** | Scan 8f4fb7da... is complete with zero findings and full changed-file coverage; customer/provider security evidence remains open. | 0.5–1.5 d review; customer/Ozone TLS, IAM, rotation and tenant controls are external. |
| W26.8 — requested-provider/no-skip qualification | **OPEN / 100% verifier; exact-head packet pending** | The manual run includes all four provider jobs plus base and aggregate jobs; queue state is not acceptance. | 0.25–0.75 d review; CI scheduling and provider startup are external. |
| W26.11/W26.14 — aggregate and end-to-end packet | **OPEN / 100% fail-closed verifier; exact-head aggregate pending** | Require terminal functional/restart/cleanup/authority/lease markers and all provider artifacts on run 35720016370; no partial packet is promoted. | 0.5–1.5 d review; GitHub artifacts, native runners and Ozone topology are external. |
| W26.15 — per-drive hard IOPS gate | **OPEN / 100% verifier; current acceptance unchanged at 1/4 from the last terminal packet** | Retain the hard 1,000 IOPS/drive threshold. The new run must produce four terminal finite rows at or above target; do not average, lower or skip a provider. | 1.5–4 d per cycle plus queue; provider/Ozone performance and artifact retention are external. |
| P14 — final integration-readiness review | **NO-GO / 50% provisional** | Positive source/local/security evidence is insufficient while exact-head hosted performance, aggregate, customer SLO/RPO/RTO and production security gates remain open. | 1–2 d after W26.15; customer SLO/RPO/RTO, security, backup/DR and release-stream gates are external. |

### Session time log — bounded preparation-wave scheduling chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — diagnosis/implementation | Traced the 64-way whole-file qualification wave and changed only the batcher's idle decision to observe active immutable-block preparation; the publication response is explicitly excluded from the counter to avoid self-waiting. | ~0.75–1.25 h | 0 h | The scheduling window remains bounded by the existing 64 rounds, 64-request target and 1024 pending-request cap. |
| 2026-09-22 — local verification | Ran cargo fmt --all -- --check, git diff --check, the chunked 21-test library suite and strict Clippy with -D warnings. | ~0.25–0.5 h | ~0.1–0.25 h shared Cargo target wait | All local source gates pass. |
| 2026-09-22 — workspace regression verification (21:20 AEST) | Re-ran the full locked workspace all-target test matrix and full workspace strict Clippy with -D warnings; both exited 0. Explicitly opt-in native/live provider cases remain separate external gates. | ~0.25–0.5 h | ~0.1 h shared Cargo target wait | Workspace regression evidence is green without converting ignored native/provider cases into acceptance. |
| 2026-09-22 — security review | Ran security preflight, one-file discovery, source-backed threat review and final scan 8f4fb7da...; complete coverage and zero findings were sealed before commit. | ~0.5–0.75 h | ~0.25 h workbench finalization | No security candidate survived; customer/provider controls remain separately tracked. |
| 2026-09-22 — hosted live recheck (21:20 AEST) | Re-polled exact-head run 35720016370: W26 provider/base/aggregate jobs remain queued; unrelated Node jobs are progressing and the workflow still has no terminal conclusion. | ~0.1 h | Hosted capacity pending | No provider, performance or aggregate result is promoted from unrelated job progress. |
| 2026-09-22 — mainline/CI recheck (21:25 AEST) | Fetched origin/main at a107b382, which contains the W26 source commit plus unrelated W04/W07 documentation; exact-head run 35720016370 remains queued, with all W26 provider/base/aggregate jobs still queued. | ~0.1 h | Hosted capacity pending | Corrected the source ancestry wording; no provider, performance or aggregate result is promoted. |
| 2026-09-22 — hosted recheck (21:28 AEST) | Fetched origin/main at 2bb846247aca02e9ce47a80a808314ca4beeed29 and re-polled exact-head run 35720016370. `ozone`, `ozone-compositions`, `ozone-tidb`, `ozone-foundationdb`, `tidb`, `tidb-rustfs` and `foundationdb-rustfs` remain queued; unrelated ARM Node and Windows/macOS jobs have started or completed. | ~0.1 h | Hosted capacity pending | Recorded runner progress only; no W26 provider, performance or aggregate result is promoted. |
| 2026-09-22 — publication | Committed, rebased over concurrent mainline updates, pushed 9f6041db to origin/main, fetched again, and verified local HEAD equals remote. | ~0.25–0.5 h | ~0.25–0.75 h mainline reconciliation | Other threads can build from the exact implementation tip. |
| 2026-09-22 — dispatch | Dispatched exact-head manual CI run 35720016370; its workflow head is exactly 9f6041db and all listed provider/base/aggregate jobs were queued at capture. | ~0.1–0.25 h | CI queue pending; provisional ~0.5–2 h | The run is the authoritative hosted gate; queued state is not acceptance. |
| 2026-09-22 — live CI recheck (21:16 AEST) | Re-polled run 35720016370: workflow status remains queued; all four W26 provider jobs, the base Ozone job and ozone-compositions remain queued, while one unrelated ubuntu-24.04-arm Node job has started. | ~0.1 h | Hosted capacity still pending for W26 jobs | This is progress in runner allocation only; no provider, performance or aggregate result is promoted. |
| 2026-09-22 — next gate | Retrieve every terminal provider and aggregate artifact from run 35720016370, compare each provider row to the hard target and classify any native/provider/customer gate separately. | ~0.5–1.5 d provisional | Hosted runner/provider wait is external | Keep production **NO-GO** until the complete end-to-end packet and all production gates close. |

## Current authority override — 2026-09-22, FoundationDB publication read-overlap chunk

This is the newest implementation boundary. Source commit
[`fba61979f1f6c9858026cd5ebc4c5d3d357f366b`](https://github.com/andymac4182/mount-rs/commit/fba61979f1f6c9858026cd5ebc4c5d3d357f366b)
(`perf(w26): overlap FoundationDB publication reads`) is on `origin/main`.
The FoundationDB metadata publication still uses one transaction, read
version, lease/fence predicate, revision CAS, shared authority time, retry
policy and fail-closed ambiguous-commit handling; only the three independent
read futures are polled together. The direct `futures-util` dependency and
root lockfile entry are part of the same chunk.

| Gate / item | Current result | Evidence | Remaining action / ownership |
| --- | --- | --- | --- |
| Source implementation | **PUBLISHED / 100% for this chunk** | `fba61979` is verified at the detached checkout and `origin/main`; `cargo fmt --all -- --check` and `git diff --check` pass. | Requalify the real Ozone/provider path on this exact source. |
| FoundationDB Rust compile/test targets | **PASS** | `./scripts/cargo-shared check -p mount-rs-foundationdb --features foundationdb --lib`; the same with `--tests --locked`; strict feature Clippy with `-D warnings`; and portable feature-off package tests pass. | Hosted/native runtime evidence remains required. |
| FoundationDB native unit execution | **BLOCKED — native gate** | Focused feature tests compile through the new futures dependency and reach only the linker failure `ld: library 'fdb_c' not found`; no Rust diagnostic or test assertion failure was produced. | Supply/install the FoundationDB client library on the native runner or rely on the hosted FoundationDB job; do not convert this to a provider pass. |
| Security diff scan | **PASS — 0 findings** | Scan `26c525f1-03bc-4a3a-9c91-b7577eab8316` has complete changed-file coverage for `Cargo.toml` and `src/lib.rs`, snapshot `codex-security-snapshot/v1:sha256:cc4d0d02c1e8b528faa28ec9a7b44d7b5265e1e1e6980aab03c4bdd4f4d34e90`, findings SHA `0f77c368b78a180616894b3742f133a9ece9fff3b2215c6a736579185e6b636b`, coverage SHA `16970263a466a591c8b21afb371132472800dcbda67499c8b90d6951561f4bb1`. | Customer certificate/IAM, secret rotation, tenant isolation and provider-native controls remain separate production gates. |
| Fresh exact-head Ozone qualification | **PENDING — run `35715790619`** | Manual CI run [`35715790619`](https://github.com/andymac4182/mount-rs/actions/runs/35715790619) has workflow head `af7e73dfb7f54a31c1a91be829238571950a48e1`; `fba61979f1f6c9858026cd5ebc4c5d3d357f366b` is an ancestor, so the published optimization is included. All four provider jobs are queued and no result is promoted. | Wait for terminal SQLite, PGlite, TiDB, FoundationDB and aggregate jobs; retain exact artifacts and classify provider/native/customer gates separately. |
| Later manual descendant qualification | **PENDING — run `35716722852`** | Manual run [`35716722852`](https://github.com/andymac4182/mount-rs/actions/runs/35716722852) has head `32b85965deef60c84d90f5fb353f45fb6001a303`; ancestry is verified locally to include `fba61979`, but all four provider jobs are also queued. It is a usable fallback evidence boundary, not a pass. | Accept either manual run only after terminal all-provider and aggregate artifacts are checked against the exact workflow head. |
| Canceled automatic descendant | **REJECTED — run `35717708439`** | The push-triggered run head `05e320ade08cecc4c6ca85edf0e38d2d02b01233` includes `fba61979`, but its four provider jobs were canceled by push-run concurrency; the aggregate remained queued. | Do not promote canceled, partial or aggregate-less evidence; retain manual runs as the acceptance boundary. |
| Prior hosted producer checkpoint | **PARTIAL / not this source** | Run `35712159705` workflow head `844f4032c857bc5033ab04fd0f853b3f66a25ab2` contains the prior `ff0ccfdd` source, not `fba61979`; its retained FoundationDB/TiDB rows are recorded in the next authority section below and remain useful diagnosis only. | Keep prior metrics as diagnosis only; accept no performance result for this chunk until run `35715790619` is terminal. |
| Production readiness | **NO-GO** | The previous provider rows missed `1000` IOPS/drive, and this new source has no hosted evidence yet. Tier-1 `99.99%` reliability, five-minute RPO/RTO, customer-deployed Ozone security and customer/Ozone-owned backup/DR remain open. | Continue W26-owned implementation/qualification until all feasible providers and the end-to-end packet pass; releases remain with the separate stream. |

### Work-item delta and provisional estimates

| Work item | Status / completion after this chunk | Evidence and remaining action | Provisional engineering time / external blocker |
| --- | --- | --- | --- |
| W26.3c — FoundationDB/R2 composition | **OPEN / 100% implementation; hosted requalification in flight** | The publication read window is now overlapped without weakening the metadata transaction boundary. Run `35715790619` includes `fba61979` as an ancestor and must show functional markers and `>=1000` IOPS. | 0.5–1.5 d per qualification/remediation cycle; hosted FoundationDB image/client, Ozone/R2 latency and runner capacity are external. |
| W26.7 — security/configuration policy | **OPEN for production / 100% source-diff security gate for this chunk** | Scan `26c525f1-03bc-4a3a-9c91-b7577eab8316` is complete and zero-finding across both changed files; customer/provider security evidence remains open. | 0.5–1.5 d review; customer/Ozone security controls are external. |
| W26.8 — requested-provider/no-skip qualification | **OPEN / 100% verifier; run `35715790619` in flight** | `fba61979` is published and included in workflow head `af7e73df`; all four provider jobs were queued at capture, so no provider or aggregate result is accepted yet. | 0.25–0.75 d review; CI scheduling/provider startup are external. |
| W26.11/W26.14 — aggregate and end-to-end packet | **OPEN / 100% fail-closed verifier; new packet pending** | The previous aggregate boundary remains fail-closed; only a terminal all-surface, all-provider packet on `fba61979` can advance these rows. | 0.5–1.5 d review; GitHub artifacts, native runners and Ozone topology are external. |
| W26.15 — per-drive hard IOPS gate | **OPEN / 100% verifier; prior terminal acceptance 25% (1/4)** | Do not lower, average or skip `1000`; the new source must produce four terminal finite rows at or above target. | 1.5–4 d per cycle plus queue; provider/Ozone performance and CI artifact retention are external. |
| P14 — final integration-readiness review | **NO-GO / 50% provisional** | Source and diff-security evidence are positive, but exact-head hosted performance, aggregate, customer SLO/RPO/RTO and production security gates remain open. | 1–2 d after W26.15; customer SLO/RPO/RTO, security, backup/DR and release-stream gates remain external. |

### Session time log — FoundationDB publication read-overlap chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — diagnosis/implementation | Traced the hosted FoundationDB row's publication latency and changed only the independent lease, manifest and authority reads to overlap inside the existing transaction. | ~0.5–1 h | ~0.25 h hosted artifact inspection | Transactional validation and fail-closed outcome semantics are unchanged. |
| 2026-09-22 — local verification | Ran formatting, diff checks, feature lib/test-target checks, strict Clippy, portable tests and the focused native test; the latter remains blocked only by missing `fdb_c`. | ~0.5–0.75 h | ~0.25 h shared Cargo target wait | Compile/test-target gates pass; native runtime is explicitly not promoted. |
| 2026-09-22 — security/publication | Completed scan `26c525f1-03bc-4a3a-9c91-b7577eab8316` with two changed surfaces and zero findings, rebased the source commit onto concurrent mainline, and pushed `fba61979`. | ~0.5–0.75 h | ~0.25–0.75 h GitHub/mainline wait | Other threads can build from `origin/main`; production remains NO-GO pending exact-head hosted evidence. |
| 2026-09-22 — dispatch | Dispatched fresh exact-head CI run `35715790619`; workflow head `af7e73df` contains `fba61979` as an ancestor. | ~0.25 h | CI queue/execution pending | The run is bound to the published source; queued jobs are not acceptance. |
| 2026-09-22 — CI queue checkpoint (20:41 AEST) | Re-polled run `35715790619`; all four provider jobs remain queued and the workflow has not entered execution. Recent automatic pushes are separate runs; this manual group remains the authoritative exact-head gate. | ~0.1 h | ~0.25 h and growing CI-capacity wait | External scheduling is the current blocker; no provider, aggregate or performance result is promoted from queue state. |
| 2026-09-22 — CI queue inventory (20:47 AEST) | `gh run list --workflow ci.yml --status queued` shows nine whole workflow runs queued since 10:01 UTC, including the authoritative manual run; `35715790619` still has all four W26 provider jobs queued. | ~0.1 h | ~0.4 h hosted-runner capacity wait | This is a repository-wide scheduling condition; retain the manual run and do not substitute newer revisions or infer provider results. |
| 2026-09-22 — manual fallback/cancellation review (20:51 AEST) | Verified `fba61979` is an ancestor of manual descendant `35716722852`; watched it for a bounded interval with every listed job still queued. Classified automatic descendant `35717708439` as non-acceptance because its provider jobs were canceled. | ~0.15 h | ~0.5 h hosted-runner capacity wait | The next action is still terminal artifact retrieval from either manual run; no source or performance conclusion changes. |
| 2026-09-22 — local control recheck (20:54 AEST) | `node benchmarks/storage/test.mjs`; positive `verify-w26-ozone-production-config.mjs` fixtures for SQLite, PGlite, TiDB (with the required out-of-band `MOUNT_RS_TIDB_TLS_URL` reference) and FoundationDB; `verify-w26-ozone-rollout-contract.mjs`; and evidence-packet `--help` validation all passed. No provider connection was opened. | ~0.1 h | 0 h hosted | Local policy/contract controls remain positive, but they do not replace terminal Ozone/provider artifacts or the hard IOPS gate. |
| 2026-09-22 — next gate | Retrieve all provider and aggregate artifacts from run `35715790619`, classify performance versus functional/provider gates, and select the next safe chunk if any row remains below target. | ~0.5–1.5 d provisional | ~0.5–2 h provisional CI/provider wait | Never close W26.15/P14 on queued, skipped, partial, canceled or ancestor-only evidence. |

## Current authority override — 2026-09-22, FoundationDB chunk-clear hosted checkpoint

This is the newest implementation checkpoint and hosted-qualification
boundary. Source commit [`ff0ccfddcad786fdce142adebc16fa50347b9b13`](https://github.com/andymac4182/mount-rs/commit/ff0ccfddcad786fdce142adebc16fa50347b9b13)
(`perf(w26): avoid redundant FoundationDB chunk clears`) is verified at both
the detached checkout and `origin/main`. It keeps the existing FoundationDB
lease, revision, manifest and transaction controls, but avoids clearing the
entire metadata chunk range when the current and next manifests have the same
chunk count; a missing manifest still clears the full prefix and a shrinking
manifest clears only its stale trailing chunks. The new helper test covers all
three branches and the deterministic big-endian key. Hosted run `35712159705`
actually tested workflow head `844f4032c857bc5033ab04fd0f853b3f66a25ab2`,
which contains the source commit as an ancestor; the run is still waiting on
unrelated CI jobs, so its producer artifacts are evidence but not an
aggregate acceptance packet.

| Gate / item | Current result | Evidence | Remaining action / ownership |
| --- | --- | --- | --- |
| Source implementation | **PUBLISHED / 100% for this chunk** | `ff0ccfddcad786fdce142adebc16fa50347b9b13` on `origin/main`; `git diff --check` and `cargo fmt --all -- --check` pass. | Requalify the real Ozone/provider path; hosted performance remains open. |
| FoundationDB Rust compile | **PASS** | `./scripts/cargo-shared check -p mount-rs-foundationdb --features foundationdb --lib --locked` and the same command with `--tests --locked` both pass; strict FoundationDB Clippy with `-D warnings` passes. | Native runtime/link evidence remains required. |
| FoundationDB native unit execution | **BLOCKED — native gate** | Focused feature test reached the linker and failed only with `ld: library 'fdb_c' not found`; no Rust diagnostic or test assertion failure was produced. | Supply/install the FoundationDB client library on the native runner or rely on the hosted FoundationDB job; do not convert this to a provider pass. |
| Security diff scan | **PASS — 0 findings** | Scan `fa9447cc-4cff-4680-baef-d7f7c260d8c5` sealed with complete changed-file coverage, no candidates/findings, snapshot `codex-security-snapshot/v1:sha256:fd4bff61d580f1f6e414c872936baee053bcc6639b24ab02904bfb4920c75d09`; report `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/0a816d34a63a9a6fee506414c6f629fa73e2964f_20260922T093930Z_dueoa8nf/report.md`. | Keep customer certificate/IAM, secret rotation, tenant isolation and provider-native controls as separate production gates. |
| Hosted Ozone qualification | **PARTIAL PRODUCER EVIDENCE — aggregate pending** | Run [`35712159705`](https://github.com/andymac4182/mount-rs/actions/runs/35712159705), workflow head `844f4032c857bc5033ab04fd0f853b3f66a25ab2`, published FoundationDB artifact `10688630886` and TiDB artifact `10688255611`. FoundationDB/R2 completed `400/400`, `1200/1200`, zero timeout/cleanup failures and all functional/restart/authority markers, but measured `458.266944` IOPS over `2618.561118 ms`; TiDB/R2 completed `400/400`, `1200/1200`, zero timeout/cleanup failures and functional/restart/fencing/ambiguity markers, but measured `421.067715` IOPS over `2849.897909 ms`. Artifact JSON/log SHA-256 pairs are FoundationDB `251b2cb2f325e60c36b852bc3cc4ae06b74122f1db7dff0a6be3c12e66d5f7d6` / `93505b98ee964c97d14d7bfe0c649666af7ceb8615385fe76c39d25a6b9fa4e6` and TiDB `ad5d1223d39c73f709915675434995ffec1d8def44e62579dc48991db9b56c74` / `c794ee124e0477b6029dc9593620b73f2801fa883cdb7685f5f7d0dc9cdd5ad6`. | Wait for SQLite/PGlite, base and aggregate jobs to finish; classify all four rows from retained artifacts and do not promote this partial packet. |
| Production readiness | **NO-GO** | The new FoundationDB and TiDB rows are functionally clean but miss the hard `1000` IOPS/drive target; the last terminal packet remains `1/4` over target. Customer-deployed Ozone security, Tier-1 `99.99%` reliability, five-minute RPO/RTO and customer/Ozone-owned backup/DR remain external gates. | Continue W26-owned implementation/qualification until every feasible provider and end-to-end packet passes; releases remain with the separate stream. |

### Work-item delta and provisional estimates

The complete W26.1–W26.15/P14 table immediately below remains the detailed
ledger for every item. This chunk changes the current delta as follows:

| Work item | Status / completion after this chunk | Evidence and remaining action | Provisional engineering time / external blocker |
| --- | --- | --- | --- |
| W26.3c — FoundationDB/R2 composition | **OPEN / 100% implementation; 0% current hard-target acceptance** | Run `35712159705` artifact `10688630886` proves `400/400` successful lifecycles, all functional/restart/authority markers and zero timeout/cleanup failures at `458.266944` IOPS, below target. The chunk-clear optimization improved the prior `306.794117` result but did not close the gate. | 0.5–1.5 d per qualification/remediation cycle; hosted FoundationDB image/client, Ozone/R2 latency and runner capacity are external. |
| W26.3d — TiDB/R2 composition | **OPEN / 100% implementation; 0% current hard-target acceptance** | Run `35712159705` artifact `10688255611` proves `400/400` successful lifecycles, functional/restart/fencing/ambiguity markers and zero timeout/cleanup failures at `421.067715` IOPS, below target; the log records a transient metadata write-conflict retry. | 1–3 d per qualification/remediation cycle; hosted TiDB topology, Ozone/R2 latency and runner capacity are external. |
| W26.7 — security/configuration policy | **OPEN for production / 100% source-diff security gate for this chunk** | Current scan is zero-finding and complete for the changed file; customer TLS/IAM, rotation, tenant isolation and provider-native security evidence remain open. | 0.5–1.5 d review; customer/Ozone security controls are external. |
| W26.8 — requested-provider/no-skip qualification | **OPEN / 100% verifier; producer packet partial** | Source is on `origin/main`; the workflow head contains it and the TiDB/FoundationDB producer artifacts are retained, while SQLite/PGlite and the aggregate remain pending. Require terminal rows for all four providers on one workflow revision. | 0.25–0.75 d review; CI scheduling/provider startup are external. |
| W26.11/W26.14 — aggregate and end-to-end packet | **OPEN / 100% fail-closed verifier; new aggregate pending** | The previous aggregate correctly failed closed; only a terminal all-surface, all-provider packet can advance these rows. | 0.5–1.5 d review; GitHub artifact service, native runners and Ozone topology are external. |
| W26.15 — per-drive hard IOPS gate | **OPEN / 100% verifier; current partial evidence 0/2 newly observed rows** | Do not lower, average or skip the `1000` target: FoundationDB `458.266944` and TiDB `421.067715` both fail their individual rows despite complete lifecycle metrics. The last terminal accepted count remains `1/4`; current run is not terminal. | 1.5–4 d per cycle plus queue; provider/Ozone performance and CI artifact retention are external. |
| P14 — final integration-readiness review | **NO-GO / 50% provisional** | New source/security evidence is positive, but hosted provider results and customer production gates are not terminal/closed. | 1–2 d after W26.15; customer SLO/RPO/RTO, security, backup/DR and release-stream gates remain external. |

### Session time log — FoundationDB optimization chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — implementation | Traced FoundationDB metadata publication and load key/range behavior, then added deterministic key construction and selective clear-range logic with a focused unit test. | ~0.75–1.25 h | ~0.25 h shared-target contention | Same-size publications avoid the redundant clear; missing and shrinking manifests retain cleanup semantics. |
| 2026-09-22 — local verification | Ran formatting, diff checks, FoundationDB feature lib check, test-target check, and strict Clippy. The linked native test remains blocked only by missing `fdb_c`. | ~0.5–0.75 h | ~0.25–0.75 h shared Cargo cache wait | Compile-only gates pass; native execution is explicitly not promoted. |
| 2026-09-22 — security review | Ran the configured security diff preflight and sealed scan `fa9447cc-4cff-4680-baef-d7f7c260d8c5` with complete one-file coverage and zero findings. | ~0.5–0.75 h | ~0.25 h workbench finalization | No security candidate survived; customer/provider controls remain separately tracked. |
| 2026-09-22 — publication | Rebased commit `9d02861a` onto concurrent mainline, pushed final source SHA `ff0ccfddcad786fdce142adebc16fa50347b9b13`, and dispatched run `35712159705`. | ~0.25–0.5 h | pending CI/provider queue and execution | Other threads can build from `origin/main`; production remains NO-GO pending terminal hosted evidence. |
| 2026-09-22 — hosted producer evidence | Downloaded retained FoundationDB artifact `10688630886` and TiDB artifact `10688255611`; validated exact workflow head ancestry, functional markers, `400/400` lifecycle counts, finite metrics, zero timeout/cleanup failures and the two hard IOPS misses. | ~0.5–0.75 h | ~0.5–1 h CI artifact availability | Provider behavior is now classified; aggregate and remaining provider rows are still external/hosted gates. |
| 2026-09-22 — next gate | Retrieve SQLite/PGlite, base and aggregate jobs from run `35712159705`, compare exact SHA, record metrics/artifact digests, then choose the next correctness-preserving provider optimization from the complete packet. | ~0.5–1.5 d provisional | ~0.5–2 h provisional CI/provider wait | Never close W26.15/P14 on queued, skipped, partial, canceled or single-provider evidence. |

## Current authority override — 2026-09-22, exact-head replacement provider packet

This is the newest hosted qualification boundary. The standalone Ozone
lockfile correction is now in the source under test, so the earlier
`--locked` pre-test failure is not promoted as provider behavior. Replacement
run [`35707725455`](https://github.com/andymac4182/mount-rs/actions/runs/35707725455)
targeted exact source/lockfile SHA `1ceaa96486a96ed4288c079dcde2b3b2d18900bb`
(`fix(w26): sync Ozone test lockfile`). The ledger-only publication that
currently advances `origin/main` to `58fc18c0a3d62dc92108a2be4efff81eb023950a`
does not change the tested source or lockfile. The producer jobs and the
single-revision aggregate job are now terminal; the aggregate failed closed,
so this packet is not an acceptance result.

| Provider / gate | Hosted result | Exact evidence | Remaining boundary |
| --- | --- | --- | --- |
| Base Ozone harness | **PASS** — job `106680595638` | Gateway contract, restart/reopen, stopped-gateway failure and cleanup job completed successfully; base artifact `10685202339`. | Retrieve/review the base artifact with the aggregate; customer TLS/IAM, tenant isolation and Ozone topology remain external. |
| SQLite/R2 composition | **FAIL — hard IOPS only** — job `106680595812` | `400/400` iterations and `1200/1200` operations; zero timeouts and cleanup failures; `781.576517` IOPS, elapsed `1535.358309 ms`; write/read/delete p50/p95/p99 `185.359683/248.161647/249.131837`, `58.390019/145.392184/167.923716`, `7.014842/35.126116/51.413983 ms`. Ozone contract, gateway failure, restart/reopen, bounded listing, Node CLI/HTTP and cleanup markers passed. Artifact `10685187964`; JSON SHA-256 `b0f880653f8035a360de3093ddd245db101155b8a12780cd962203746011e54e`; log SHA-256 `408d9e591f2a57ea0c4183307f779d86dd989a0c4b186fa137fc206862031d44`. | Preserve the functional result, improve or diagnose the provider/Ozone path, and obtain a terminal row at or above `1000` IOPS. |
| PGlite/R2 composition | **PASS provider row** — job `106680595812` | `400/400` iterations and `1200/1200` operations; zero timeouts and cleanup failures; `1340.137102` IOPS, elapsed `895.430772 ms`; write/read/delete p50/p95/p99 `80.621764/97.293680/98.013326`, `57.816639/75.039101/75.302673`, `8.367168/40.395027/42.116686 ms`. PGlite composition, restart/reopen, Node SDK/CLI/HTTP, bounded listing and cleanup markers passed. It is the only current provider row over the hard target. | Keep the fast path and collision/failure semantics unchanged; the sibling SQLite miss still fails the composition job and the aggregate is queued. |
| TiDB/R2 composition | **FAIL — hard IOPS only** — job `106680595912` | `400/400` iterations and `1200/1200` operations; `verifiedReads=400`; zero timeouts and cleanup failures; `492.510872` IOPS, elapsed `2436.494436 ms`; write/read/delete p50/p95/p99 `315.262023/357.507547/370.113025`, `77.6453845/226.76326/255.996367`, `12.86257/25.474518/46.023883 ms`. Ozone contract, durable lifecycle, restart/reopen, fencing/ambiguous-commit checks and cleanup markers passed. Artifact `10685592256`; JSON SHA-256 `a4ce539bbf6390f723b62b243c51ad97e0a7141fdf5ad7876b716c92d4823841`; log SHA-256 `293fe8620bbd1dd7dc575ff649a855605fa8981984a00350a46a675faa1d046d`. | Continue correctness-preserving TiDB/Ozone optimization and requalify; do not trade away duplicate, collision or ambiguity checks. |
| FoundationDB/R2 composition | **FAIL — hard IOPS only** — job `106680595831` | `400/400` iterations and `1200/1200` operations; `verifiedReads=400`; zero timeouts and cleanup failures; `306.794117` IOPS, elapsed `3911.417891 ms`; write/read/delete p50/p95/p99 `387.07509/499.478162/515.814127`, `146.906903/380.373178/380.609538`, `29.665532/124.670604/126.283307 ms`. Ozone contract, gateway failure, restart/reopen, transaction/image/authority readiness, bounded listing, lease policy, authority stats, N-API and cleanup markers passed. Artifact `10685113640`; JSON SHA-256 `c7cdf6ca890add30696690751c1c2acf45762449390348dcef7049fa09260e7d`; log SHA-256 `9c1afddc8f985e8a445acf6ef421b96eaebfdcfced612cbb46698f24ae21e06e`. | Continue correctness-preserving FoundationDB/Ozone optimization and requalify; retain transaction, lease/fence and authority checks. |
| One-revision aggregate | **FAIL / fail-closed — job `106685891954`** | Terminal log emitted `W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000`. No aggregate artifact was produced. The missing marker is consistent with the SQLite row failing the hard target; PGlite’s individual row passed but the composition packet cannot pass as a provider set. | Keep the verifier fail-closed, remediate the SQLite/Ozone performance miss, re-run all four providers on one exact SHA, and require a terminal aggregate pass. |

Security evidence remains explicit: the PGlite source diff scan
`2b2a12cd-0984-4dfb-9d89-2139055afb3d` and Ozone lockfile diff scan
`f2ba64e6-373d-4741-b27f-60bdcec6d7e6` both have complete changed-surface
coverage and zero reportable findings. Those scans cover the source/lockfile
changes under test; they do not replace customer certificate/IAM, secret
rotation, tenant-isolation or provider-native security evidence.

### Current W26 production-readiness status by work item

The historical itemized tables below retain prior boundaries. This table is
the authoritative status for the current exact-head packet and includes every
W26 work item plus P14. Percentages are provisional: implementation and
hosted/provider gates are deliberately separated.

| Work item | Status / completion | Current evidence | Remaining action | Provisional estimate | External blockers / ownership |
| --- | --- | --- | --- | ---: | --- |
| W26.1 — pinned Ozone gateway and health/readiness harness | **OPEN / 100% implementation; 100% current base job** | Base job `106680595638` passed the current harness. | Bind the base artifact and aggregate to the exact-head acceptance record. | 0–1 h review | Hosted runner, customer Ozone topology and TLS/IAM. |
| W26.2 — immutable block contract through Ozone S3 gateway | **OPEN / 100% implementation; current producer functional markers pass** | Composition, TiDB and FoundationDB logs cover create/read/restart/failure/cleanup paths; no provider row is accepted until the aggregate is terminal. | Review the aggregate’s complete marker set on one revision. | 0.5–1 d review | Ozone capacity, customer TLS/IAM and tenant isolation. |
| W26.3a — SQLite/R2 composition | **OPEN / 100% implementation; 0% current hard-target acceptance** | Functional lifecycle is complete with zero timeout/cleanup failures, but current IOPS is `781.576517` and the job fails closed. | Diagnose/remediate without weakening lifecycle checks; requalify at `>=1000`. | 0.5–1.5 d per cycle | Hosted runner variance, Ozone/R2 capacity and provider latency. |
| W26.3b — PGlite/R2 composition | **OPEN for aggregate / 100% implementation; 100% current provider-row target acceptance** | Current provider row passes at `1340.137102` IOPS with complete lifecycle and cleanup evidence. | Preserve the passing fast path; wait for aggregate and sibling-provider closure. | 0.25–0.75 d review | Aggregate scheduling and customer Ozone topology. |
| W26.3c — FoundationDB/R2 composition | **OPEN / 100% implementation; 0% current hard-target acceptance** | Functional, restart, authority, lease, N-API and cleanup markers pass; IOPS is `306.794117`, so the job fails closed. | Diagnose/remediate provider/Ozone latency and requalify at `>=1000`. | 0.5–1.5 d per cycle | Hosted FoundationDB image/client, native library and Ozone topology. |
| W26.3d — TiDB/R2 composition | **OPEN / 100% implementation; 0% current hard-target acceptance** | Functional, durable, restart, fencing/ambiguity and cleanup markers pass; IOPS is `492.510872`, so the job fails closed. | Diagnose/remediate provider/Ozone latency and requalify at `>=1000`. | 1–3 d per cycle | Hosted TiDB/PD/TiKV, Ozone topology and provider-native latency. |
| W26.4 — Rust/Node/CLI/HTTP/N-API end-to-end surfaces | **OPEN / 100% implementation; producer markers observed, aggregate failed closed** | Current composition/FDB/TiDB artifacts contain the advertised surface, reopen/restart and cleanup markers; the aggregate correctly rejects the missing all-provider IOPS marker. | Require one terminal aggregate containing every surface marker after all provider rows pass. | 0.5–1.5 d review | Native runners, provider fixtures and CI orchestration. |
| W26.5 — explicit immutable-block reconciliation | **IMPLEMENTED / 100% local contract; hosted operations evidence open** | Fail-closed reconciliation and cleanup markers remain present; no implicit shutdown cleanup is accepted. | Obtain provider retention, alerting and customer operational evidence. | 0.5–1.5 d review | Provider enumeration/retention and customer operations. |
| W26.6 — bounded remote directory enumeration | **IMPLEMENTED / 100% local contract; current producer markers observed** | Bounded listing/overflow evidence is present in current composition/FDB artifacts. | Bind all required provider surfaces to the terminal aggregate. | 0.5–1.5 d review | Hosted provider surfaces and customer scale/topology. |
| W26.7 — credential-free production security/configuration policy | **IMPLEMENTED / 100% local policy; production evidence open** | PGlite and lockfile scans are zero-finding; no credentials are persisted. | Verify customer certificates, IAM, rotation, tenant isolation and secure runtime readback. | 0.5–1.5 d review | Customer/Ozone security controls and provider-native identity. |
| W26.8 — strict requested-provider/no-skip qualification | **IMPLEMENTED / 100% verifier; 4/4 producer jobs executed** | Requested SQLite/PGlite/TiDB/FoundationDB jobs all ran on the exact source/lockfile SHA; aggregate remains queued. | Reject queued, skipped, partial or canceled aggregate results. | 0.25–0.75 d review | Hosted credentials, provider startup and CI scheduling. |
| W26.9 — fixed profile and hard 1,000-IOPS verifier | **IMPLEMENTED / 100% verifier; 1/4 current provider rows meet target** | Fixed `4 KiB`/`400`-iteration/concurrency-64 profile produced finite complete metrics; three misses failed closed. | Accept only four terminal finite rows at or above `1000` IOPS. | 0.5–1 d review | Hosted artifact service and provider performance. |
| W26.10 — fail-closed artifact retention | **IMPLEMENTED / 100% CI control; current producer artifacts retained** | Base, compositions, TiDB and FoundationDB artifacts are retained with recorded IDs and digests. | Verify aggregate artifact retention after it becomes terminal. | 0.25–0.5 d review | GitHub artifact service and retention policy. |
| W26.11 — one-revision evidence aggregation | **OPEN / 100% verifier; 0% current aggregate acceptance** | Aggregate job `106685891954` is terminal failure with the exact missing `OZONE_IOPS_PASS` reason; no conclusion or marker is promoted. | Re-run and validate a terminal aggregate pass on one exact SHA after provider remediation. | 0.5–1.5 d review | GitHub scheduling and artifact availability. |
| W26.12 — incomplete metric/lifecycle rejection | **IMPLEMENTED / 100% verifier; current producer checks complete** | All measured rows have complete `400/400`, `1200/1200`, finite metrics and zero timeout/cleanup failures; target misses correctly fail. | Preserve fail-closed behavior in the aggregate. | 0.25–0.75 d review | Hosted artifact correctness and provider behavior. |
| W26.13 — credential-free customer rollout contract | **OPEN / 100% declaration contract; customer evidence open** | Contract records Tier-1 `99.99%`, five-minute RPO/RTO, secure Ozone, tenant scope, feasible providers and customer-owned backup/DR. | Customer/Ozone must supply measured SLO, RPO/RTO, certificates/IAM/rotation and DR evidence. | 1–2 d review | Customer/Ozone ownership; backup/DR is outside W26. |
| W26.14 — complete end-to-end packet surface enforcement | **OPEN / 100% local control; aggregate failed closed** | Current producer artifacts contain the required gateway, provider, restart, listing, Node/native and cleanup markers; the aggregate withheld acceptance because the compositions set lacked `OZONE_IOPS_PASS`. | Produce and review one terminal exact-head all-surface packet after all provider target gates pass. | 0.75–1.5 d review | Native runners, provider fixtures and CI orchestration. |
| W26.15 — P8 per-drive 1,000-IOPS gate | **OPEN / 100% verifier; 25% current provider acceptance** | One of four current provider rows meets target: PGlite `1340.137102`; SQLite `781.576517`, TiDB `492.510872`, FoundationDB `306.794117` fail. | Close all four providers and aggregate on one exact SHA; never lower, average or skip the target. | 1.5–4 d per cycle plus queue | Hosted runners, Ozone/provider capacity, latency and artifact retention. |
| P14 — final integration-readiness review | **NO-GO / 50% provisional** | Current packet has strong functional producer evidence but only `1/4` provider rows over the hard target; aggregate job `106685891954` failed closed on the missing all-provider marker; customer security/SLO/RPO/RTO evidence is also open. | Re-audit terminal all-provider/end-to-end/security evidence and issue an explicit readiness decision; make no deployment or release claim. | 1–2 d after W26.15 | Customer secure Ozone topology, Tier-1 SLO, five-minute RPO/RTO, backup/DR and separate release stream. |

### Session time log — exact-head replacement packet

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — producer review | Polled run `35707725455`, confirmed exact SHA and terminal producer states, and separated functional markers from hard IOPS conclusions. | ~0.25–0.5 h | ~0.25–0.75 h CI queue | Base passed; PGlite passed target; SQLite, TiDB and FoundationDB failed only the hard target. |
| 2026-09-22 — artifact evidence review | Reviewed retained JSON/log evidence, lifecycle counts, latency percentiles, provider markers and artifact digests for the three failing producer jobs and the passing PGlite row. | ~0.75–1.25 h | ~0.25–0.75 h artifact retrieval/processing | Current packet is reproducible and fail-closed; no provider miss was converted to a skip. |
| 2026-09-22 — ledger/tracker publication | Added the exact-head authority override, every-work-item status, provisional estimates, external ownership boundaries and session log. | ~0.5–0.75 h | ~0.25–0.75 h rebase/push reconciliation | Documentation chunk is ready to publish; production remains NO-GO. |
| 2026-09-22 — aggregate review | Retrieved the terminal aggregate log and recorded its fail-closed `OZONE_IOPS_PASS` marker reason; no aggregate artifact was emitted. | ~0.25–0.5 h | ~0.25–0.75 h hosted log availability | The verifier correctly withheld acceptance because the SQLite/PGlite composition set did not pass as a whole. |
| 2026-09-22 — next gate | Select the next SQLite/TiDB/FoundationDB correctness-preserving optimization, re-run all providers on one exact SHA, and require a terminal aggregate pass. | ~0.25–0.75 h provisional | ~0.5–2 h provisional CI/provider queue | Do not mark W26.15 or P14 complete until four provider rows and the aggregate pass. |

## Current authority override — 2026-09-22, Ozone test lockfile synchronization

This is the newest W26 CI-reproducibility boundary. The PGlite dependency
addition exposed a separate standalone lockfile in `tests/ozone`; its
`mount-rs-pglite` package entry was missing the already-resolved `md-5
0.10.6` edge. Ozone TiDB and FoundationDB jobs therefore failed before their
provider tests under `--locked`. The one-line lockfile correction is now
published and a replacement exact-head packet is running. The earlier failure
is retained as evidence of the gate and is not promoted as provider behavior.

| Field | Current value |
| --- | --- |
| Shared build-on tip | `1ceaa96486a96ed4288c079dcde2b3b2d18900bb` (`fix(w26): sync Ozone test lockfile`) is verified on `origin/main`; it is rebased over concurrent mainline changes. |
| Implementation delta | `tests/ozone/Cargo.lock` now records `md-5 0.10.6` under `mount-rs-pglite`, matching the root lockfile and the published PGlite manifest. No source, credential, runtime-policy or provider behavior changed. |
| Local evidence | `git diff --check`, locked standalone Ozone metadata and `./scripts/cargo-shared check --manifest-path tests/ozone/Cargo.toml --all-targets --locked` passed. The latter completed the full Ozone test package compile in 10.34 seconds. |
| Security evidence | Lockfile diff scan `f2ba64e6-373d-4741-b27f-60bdcec6d7e6` is sealed with complete coverage of `tests/ozone/Cargo.lock`, 0 reportable findings, and snapshot digest `codex-security-snapshot/v1:sha256:221389df7c2e76fc8b4cc22076ff0ea7e97f1269b91adf9fbbfeffdc8901494a`. Report: `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/ef682793d64222ad299697f044a71d0b44c124c1_20260922T085554Z_x5xpbucd/report.md`. |
| Pre-fix hosted evidence | Run [`35706390612`](https://github.com/andymac4182/mount-rs/actions/runs/35706390612) targeted `c791ab31`. TiDB job `106676248986` and FoundationDB job `106676249267` both failed at `tests/ozone/Cargo.lock` under `--locked` before provider tests; their retained artifacts are `w26-ozone-tidb-evidence` (`10684169484`) and `w26-ozone-foundationdb-evidence` (`10684693731`). PGlite job `106676249562` also failed its runtime step, but its log is not promoted until the run is terminal. |
| Replacement hosted qualification | Run [`35707725455`](https://github.com/andymac4182/mount-rs/actions/runs/35707725455) targets exact SHA `1ceaa96486a96ed4288c079dcde2b3b2d18900bb`. At capture it was queued: base `106680595638`, compositions `106680595812`, FoundationDB `106680595831`, TiDB `106680595912`; queued/in-progress state is not acceptance. |
| Production decision | **NO-GO / last terminal packet passed 1 of 4 provider rows; replacement packet pending.** The 1,000 IOPS/drive, Tier-1 99.99% reliability, five-minute RPO/RTO, secure customer Ozone topology, complete end-to-end packet and exact-head security gates remain open. Deployment, backup/DR and release ownership remain external. |
| Next action | Let the replacement packet reach terminal state, retrieve every producer and aggregate artifact, and update each W26 row without treating the pre-fix lock failure as a provider result. Continue provider-specific remediation only from terminal runtime evidence. |

### Work-item impact of the lockfile correction

The complete W26.1–W26.15/P14 itemized table in the next authority section
remains current for implementation, estimates and external ownership. This
chunk changes only the hosted-gate state: W26.3a–d, W26.4, W26.8,
W26.10–W26.15 and P14 are awaiting the replacement exact-head packet; W26.1,
W26.2, W26.5–W26.7 and W26.9/W26.12/W26.13 remain implemented locally with
their hosted/customer evidence boundaries unchanged. No completion percentage
is promoted from a queued or pre-test-failure job.

### Session time log — Ozone lockfile correction

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — failure diagnosis | Retrieved retained TiDB/FoundationDB artifacts from run `35706390612` and identified the shared `tests/ozone/Cargo.lock` drift before provider tests. | ~0.25–0.5 h | ~0.25–0.5 h artifact/CI wait | Classified the failure as a W26 CI reproducibility defect, not provider performance evidence. |
| 2026-09-22 — lockfile repair and verification | Added the single required `md-5 0.10.6` edge, ran locked metadata and the standalone Ozone all-targets compile. | ~0.25–0.5 h | ~0.1–0.25 h shared-target build | Ozone test package compiles with `--locked`; no source behavior changed. |
| 2026-09-22 — security and publication | Sealed scan `f2ba64e6-373d-4741-b27f-60bdcec6d7e6`, rebased over concurrent mainline work, pushed `1ceaa964`, verified `origin/main`, and dispatched run `35707725455`. | ~0.5–0.75 h | ~0.5–1 h remote reconciliation and CI queue | Replacement packet is queued; production remains NO-GO. |

## Current authority override — 2026-09-22, PGlite local identity fast path

This is the newest W26 implementation boundary. The PGlite block store now
computes the exact identity produced by the existing PostgreSQL
`md5(encode($1, 'hex'))` expression locally. It removes one provider round trip
from every unique block put while retaining the conditional insert, duplicate
read-back, byte-for-byte collision check, volume scoping, and fail-closed
disappearance behavior. This is a W26-owned compatibility/performance change;
customers still deploy and operate Ozone.

| Field | Current value |
| --- | --- |
| Shared build-on tip | `c791ab318af4c5253dbe3e9e4d1d2f7a2026fa4c` (`perf(w26): compute PGlite block IDs locally`) is verified on `origin/main`. The source chunk was rebased onto concurrent mainline work before publication; this ledger update is a separate docs chunk. |
| Current implementation chunk | `c791ab31` adds the direct `md-5` dependency, hashes the lowercase ASCII hex representation byte-for-byte, and removes the PGlite `SELECT md5(encode(...))` query. Three regression vectors cover empty, ASCII and binary payloads; the existing insert/read-back collision path is unchanged. |
| Local implementation evidence | `cargo fmt --all -- --check`, `git diff --check` and `cargo metadata --locked --no-deps` passed. Focused `check --all-targets`, library tests and strict Clippy were attempted through `scripts/cargo-shared` and all stopped before project diagnostics at macOS linker exit 69 because the host has not accepted the Xcode license. This is a native-host gate, not a code pass or a suppressed test result; hosted Rust/Node/Ozone CI remains authoritative. |
| Security evidence | Diff scan `2b2a12cd-0984-4dfb-9d89-2139055afb3d` is sealed with complete changed-file coverage for `integrations/mount-rs-pglite/Cargo.toml` and `integrations/mount-rs-pglite/src/storage.rs`, 0 reportable findings, and snapshot digest `codex-security-snapshot/v1:sha256:d5536ab709e2b857bb5190686ba0056cd14ce798da886616241aa9a68a332833`. Report: `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/f5e040d6f3cc798fb323262685ccf42ef5fd31ab_20260922T083546Z_jz5axrld/report.md`. The captured working-tree snapshot contains the exact source change; a terminal exact-head packet is still required for production evidence. |
| Hosted qualification | Manual run [`35706390612`](https://github.com/andymac4182/mount-rs/actions/runs/35706390612) targets exact SHA `c791ab318af4c5253dbe3e9e4d1d2f7a2026fa4c`. At capture it was queued: base job `106676249167`, compositions `106676249209`, TiDB `106676248986`, FoundationDB `106676249267`; no result is promoted while queued. |
| Production decision | **NO-GO / last terminal packet passed 1 of 4 provider rows; current exact-head qualification pending.** The hard `>=1,000` IOPS per drive, Tier-1 99.99% reliability, five-minute RPO/RTO, secure customer Ozone topology, complete end-to-end surface packet and exact-head security evidence remain gates. Customer deployment, backup/DR and releases remain outside W26 ownership. |
| Next action | Retrieve the terminal exact-head producer artifacts and aggregate, compare all four providers without averaging or skipping misses, then update this ledger. If PGlite still misses, continue only with correctness-preserving provider work; keep all marker, lifecycle, collision and failure checks fail-closed. |

### W26 production-readiness ledger — every work item at this boundary

Percentages are provisional and separate implementation completeness from
hosted/customer gates. “External” means W26 cannot close the item from this
repository or CI alone.

| Work item | Status / completion | Evidence | Remaining action | Provisional engineering estimate | External blockers / ownership |
| --- | --- | --- | --- | --- | --- |
| W26.1 — pinned Ozone gateway and health/readiness harness | **IMPLEMENTED / 100% code; current packet pending** | Prior base packet passed gateway policy, readiness, restart/reopen and cleanup markers; exact-head base job is queued. | Reconfirm markers on one terminal exact SHA. | 0–1 h review | Hosted runner/image and customer Ozone topology. |
| W26.2 — immutable block contract through Ozone S3 gateway | **IMPLEMENTED / 100% code; hosted recheck pending** | Prior packets covered create/CAS, binary/range read, stopped-gateway failure, restart/reopen and owned cleanup. | Reconfirm the complete marker set on the current packet. | 0.5–1 d review | Ozone capacity, customer TLS/IAM and tenant isolation. |
| W26.3a — SQLite/R2 composition | **OPEN / 100% implementation; 25% last terminal provider qualification** | Last terminal row measured `1051.976655` IOPS with 400/400 lifecycles, zero timeouts and zero cleanup failures; current exact-head row is queued. | Require a current terminal row at or above 1,000 and aggregate pass. | 0.5–1.5 d per review/remediation cycle | Hosted runner variance, Ozone/R2 capacity and artifact retention. |
| W26.3b — PGlite/R2 composition | **OPEN / 100% implementation; 0% current exact-head requalification** | Last terminal row measured `837.779225` IOPS; the new local hash fast path is published and targets one less provider round trip per unique put. | Inspect exact-head IOPS and all PGlite/R2 functional markers; continue only if the hard target remains unmet. | 0.5–1 d per review/remediation cycle | Hosted PGlite runtime, Ozone latency and runner capacity. |
| W26.3c — FoundationDB/R2 composition | **OPEN / 100% implementation; 25% last terminal provider qualification** | Last terminal row measured `450.696578` IOPS; transaction-sharing code is published, but the current exact-head job is queued. | Requalify durable/restart/lockfile/bounded-listing and hard IOPS markers. | 0.5–1.5 d per review/remediation cycle | Hosted FoundationDB image/client, native library and Ozone topology. |
| W26.3d — TiDB/R2 composition | **OPEN / 100% implementation; 25% last terminal provider qualification** | Last terminal row measured `463.080116` IOPS; confirmed-insert acknowledgement fast path is published, and the current exact-head job is queued. | Requalify durable/restart/fencing/ambiguous-commit and hard IOPS markers. | 1–3 d per review/remediation cycle | Hosted TiDB/PD/TiKV, Ozone topology and provider-native latency. |
| W26.4 — Rust/Node/CLI/HTTP/N-API end-to-end surfaces | **IMPLEMENTED / 100% local; hosted packet pending** | Local advertised-surface and pinned-oracle suites have passed in prior chunks; current exact-head producer jobs are queued. | Reconfirm every advertised surface and reopen/restart path in one aggregate packet. | 0.5–1.5 d review | Hosted native/N-API prerequisites and customer Ozone runtime. |
| W26.5 — explicit immutable-block reconciliation | **IMPLEMENTED / 100% local contract; operations evidence open** | Opt-in, prefix-scoped, lease/fence-checked and fail-closed reconciliation is locally covered. | Obtain hosted/provider retention, alerting and customer operational evidence; no implicit shutdown cleanup. | 0.5–1.5 d review | Provider enumeration/retention and customer operations. |
| W26.6 — bounded remote directory enumeration | **IMPLEMENTED / 100% local contract; exact packet pending** | Rust/HTTP/CLI/N-API/KV bounds and overflow behavior pass locally; hosted marker existed in prior packets. | Reconfirm provider-backed success/overflow markers on the current exact revision. | 0.5–1.5 d review | Hosted provider surfaces and customer scale/topology. |
| W26.7 — credential-free production security/configuration policy | **IMPLEMENTED / 100% local policy; production evidence open** | Provider-positive/security-negative fixtures and the PGlite zero-finding diff scan passed/are sealed without credentials in the repository. | Verify customer certificates, IAM, secret rotation, tenant isolation and secure runtime readback. | 0.5–1.5 d review | Customer/Ozone security controls and provider-native identity. |
| W26.8 — strict requested-provider/no-skip qualification | **IMPLEMENTED / 100% local verifier; current packet pending** | Provider-specific selection and `--require-configured` fail closed on missing providers; all four exact-head jobs were created. | Preserve strict selection and reject queued, skipped, partial or canceled rows. | 0.25–0.75 d review | Hosted credentials, provider startup and CI scheduling. |
| W26.9 — fixed profile and hard 1,000-IOPS verifier | **IMPLEMENTED / 100% verifier; acceptance open** | Fixed 4 KiB/400-iteration/concurrency-64 profile and per-drive threshold are enforced. | Accept only terminal finite metrics with every provider pass marker. | 0.5–1 d review | Hosted artifact service and provider performance. |
| W26.10 — fail-closed artifact retention | **IMPLEMENTED / 100% CI control; current packet pending** | Expected JSON uploads use `if-no-files-found: error`; prior packets retained artifacts and digests. | Recheck retention and digest evidence on the exact-head packet. | 0.25–0.5 d review | GitHub artifact service and retention policy. |
| W26.11 — one-revision evidence aggregation | **IMPLEMENTED / 100% verifier; current aggregate pending** | Aggregate verification rejects missing producer markers, wrong revisions and hard-target misses; aggregate job is not yet created while producers are queued. | Require one terminal all-provider aggregate on SHA `c791ab31`. | 0.5–1.5 d review | Workflow scheduling and artifact availability. |
| W26.12 — incomplete metric/lifecycle rejection | **IMPLEMENTED / 100% verifier** | Finite metric, exact sample-map, complete lifecycle and zero timeout/cleanup rules remain fail closed. | Preserve rejection behavior in the terminal packet; never convert a miss to skip. | 0.25–0.75 d review | Hosted artifact correctness and provider behavior. |
| W26.13 — credential-free customer rollout contract | **IMPLEMENTED / 100% declaration contract; customer evidence open** | Contract records Tier-1 99.99%, five-minute RPO/RTO, secure Ozone, tenant scope, all feasible providers and customer-owned backup/restore. | Customer/Ozone must supply measured SLO, RPO/RTO, certificates/IAM/rotation and DR evidence. | 1–2 d review | Customer/Ozone ownership; backup and DR are explicitly outside W26. |
| W26.14 — complete end-to-end packet surface enforcement | **IMPLEMENTED / 100% local control; hosted acceptance open** | Aggregate requires gateway, provider compositions, Rust/Node/CLI/HTTP, bounded listing, TiDB/FDB restart/N-API and cleanup markers. | Produce and review one terminal exact-head all-surface packet. | 0.75–1.5 d review | Native runners, provider fixtures and CI orchestration. |
| W26.15 — P8 per-drive 1,000-IOPS gate | **OPEN / 100% verifier; 25% last terminal provider qualification** | Last terminal packet had SQLite/R2 pass and PGlite/TiDB/FoundationDB below target; new exact-head packet is queued with all four producers. | Close all four providers and aggregate on one exact SHA; never lower, average or skip the target. | 1.5–4 d per remediation cycle plus external queue | Hosted runners, Ozone/provider capacity, latency and artifact retention. |
| P14 — final integration-readiness review | **NO-GO / 50% provisional** | Implementation, local controls and security packets are strong, but the latest terminal provider packet was 1/4 and the new exact-head run is queued. | Re-audit the terminal all-provider/end-to-end/security packet and issue an explicit readiness decision; make no deployment or release claim. | 1–2 d after W26.15 | Customer secure Ozone topology, 99.99% SLO, five-minute RPO/RTO, backup/DR and separate release stream. |

### Session time log — PGlite identity fast-path chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — provider-path inspection | Compared PGlite identity generation with the existing PostgreSQL expression and verified the lowercase-hex contract and preserved collision path. | ~0.25–0.5 h | ~0.1 h inspection | Selected a bounded local-hash optimization; no change to namespace, collision or error semantics. |
| 2026-09-22 — implementation | Added the direct `md-5` dependency, exact local helper, and three regression vectors; updated the lockfile. | ~0.5–0.75 h | ~0.1–0.25 h dependency/cache work | Source chunk committed as `c791ab31` after rebase. |
| 2026-09-22 — local verification | Ran formatting, diff and locked metadata checks; attempted focused compile, library tests and strict Clippy. | ~0.25–0.5 h | ~0.25–0.5 h shared-target/native toolchain wait | Static gates passed; all Rust link-dependent gates are explicitly blocked by Xcode license exit 69. |
| 2026-09-22 — security and publication | Sealed scan `2b2a12cd-0984-4dfb-9d89-2139055afb3d`, rebased onto concurrent mainline work, pushed `c791ab31`, verified `origin/main`, and dispatched exact-head run `35706390612`. | ~0.5–0.75 h | ~0.5–1 h remote reconciliation and CI queue | Other threads can build from the published source; hosted qualification remains pending. |
| 2026-09-22 — next hosted gate | Retrieve producer artifacts, aggregate marker and exact-SHA digests; update both ledgers and decide the next provider-specific chunk. | ~0.5–1 h provisional | ~1–3 h provisional CI/provider queue | No production promotion until every provider and cross-workstream gate passes. |

## Current authority override — 2026-09-22, TiDB confirmed-insert acknowledgement fast path

This is the newest W26 implementation chunk. TiDB block creation now uses the
affected-row acknowledgement from the existing insert statement to avoid a
second read when the row was definitely inserted by this request. Duplicate,
provider-specific no-op and ambiguous acknowledgement cases still finish the
result and perform the existing byte-for-byte read-back collision check. The
change therefore reduces one round trip on the hot unique-block path without
changing immutable block semantics, fencing, collision handling or fail-closed
disappearance handling.

| Field | Current value |
| --- | --- |
| Shared build-on tip | `2ce6f753a588628593baf1000ae75330780dabd3` (`perf(w26): skip TiDB block readback on confirmed insert`) is verified on `origin/main`. The ledger update below is a separate docs publication and will advance the shared tip again. |
| Current implementation chunk | `2ce6f753` changes `integrations/mount-rs-tidb/src/storage.rs` from unconditional `INSERT` plus `SELECT` to `exec_iter`, `affected_rows() == 1`, explicit result draining, and read-back only for duplicate/ambiguous cases. |
| Local implementation evidence | `cargo fmt --all -- --check`, `git diff --check`, `./scripts/cargo-shared check -p mount-rs-tidb --all-targets --locked`, and strict TiDB Clippy with `-D warnings` passed. The focused locked TiDB library suite passed 8/8 before publication. A fresh all-targets test attempt could not link because this Mac has not accepted the Xcode license (`xcrun --sdk macosx --show-sdk-path`); this is recorded as a native-host gate, not a code pass or a suppressed failure. |
| Security evidence | Diff scan `b2761b0b-aa05-4f49-a52a-3c5b7f09cad9` reviewed the TiDB storage surface with complete coverage and 0 reportable findings. Report: `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/42e1025781d4bd620f687b0927dddfc2c9fc15c0_20260922T081459Z_f4z43kvw/report.md`; snapshot digest: `codex-security-snapshot/v1:sha256:543c7a6f7d6f3dd9135034f2e37940dd273523ac6a3b1b9f223f47019e63f6e9`. The result is pre-publication snapshot evidence; a matching terminal exact-head security packet remains required for production acceptance. |
| Hosted qualification | Existing run `35702188498 <https://github.com/andymac4182/mount-rs/actions/runs/35702188498>` targeted old head `245258d9`, not this chunk. At the latest live capture, FoundationDB job `106662583211` had failed, while Ozone base `106662583505`, compositions `106662583710` and TiDB `106662583344` were queued; the run and aggregate were nonterminal, so no result is promoted. A new exact-head packet is required after this ledger publication. |
| Production decision | **NO-GO / 1 of 4 provider rows passed the last terminal packet; current exact-head qualification pending.** The 1,000 IOPS/drive target, Tier-1 99.99% reliability, five-minute RPO/RTO, secure customer Ozone topology, and end-to-end provider markers remain acceptance gates. Customer backup/DR, deployment and the separate release stream remain external ownership boundaries. |
| Next action | Publish this ledger, dispatch a fresh exact-head W26 packet, and compare TiDB plus all other providers without lowering, averaging or skipping the per-drive target. Retrieve terminal artifacts, exact SHA evidence, security result and aggregate marker before any readiness change. |

### TiDB chunk production-readiness delta

| Work-item impact | Status / completion | Evidence and remaining action | Provisional estimate / external gate |
| --- | --- | --- | --- |
| W26.3c TiDB/R2 composition and W26.15 hard IOPS gate | **OPEN / 100% implementation chunk; hosted qualification pending** | The unique-insert path now avoids a confirmed redundant read; duplicate and ambiguous cases retain the collision check. The last terminal packet measured TiDB/R2 at `463.080116` IOPS with 400/400 lifecycles, zero timeouts and zero cleanup failures, below the hard target. Re-run the exact current head and keep any miss fail-closed. | ~0.5–1 d for the exact-head run and review; TiDB/Ozone fixtures, runner capacity and artifact retention are external. |
| W26.4 end-to-end Rust/Node/CLI/HTTP/N-API path | **PASS locally for this provider change / hosted packet pending** | Compile-only all-targets check, strict Clippy and 8/8 focused TiDB tests pass. The all-targets test linker is blocked by the unaccepted local Xcode license; hosted Rust/Node/Ozone markers remain authoritative for end-to-end acceptance. | ~0.25–0.75 d review; native macOS toolchain and hosted Ozone/provider fixtures are gates. |
| W26.7/W26.13 security and customer rollout contract | **OPEN for production / local TiDB scan zero findings** | Scan `b2761b0b-aa05-4f49-a52a-3c5b7f09cad9` is complete with zero reportable findings over the changed TiDB surface. Rebind security evidence to the final exact-head packet; customer IAM, certificates, secret rotation, tenant isolation and measured SLO/RPO/RTO evidence remain outside this local change. | ~0.5–1 d exact-head review; customer security and Ozone topology are external. |
| W26.9–W26.12 verifier/retention/aggregate | **IMPLEMENTED / current packet pending** | Existing verifier remains fail-closed on missing markers, incomplete metrics and target misses. The stale run has no terminal aggregate; a new exact-head run must emit one complete all-provider marker. | ~0.25–0.5 d review; GitHub scheduling/artifact services are external. |
| P14 final integration-readiness review | **NO-GO / 49% provisional** | This chunk improves one provider hot path and is published with local evidence, but it does not change the hosted 1/4 terminal acceptance result. | ~1–2 d after the four-provider packet; customer deployment, backup/DR and release ownership remain external. |

### Session time log — TiDB confirmed-insert chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — provider-path inspection | Compared TiDB, PGlite, FoundationDB and R2 block-write acknowledgement paths and identified the TiDB unique-insert readback as a correctness-preserving round-trip candidate. | ~0.5–0.75 h | ~0.25 h code inspection | Selected only the unambiguous affected-row fast path; retained duplicate collision checks. |
| 2026-09-22 — implementation and local verification | Implemented `exec_iter`/affected-row handling, drained the result, ran formatting/diff checks, 8/8 focused library tests, all-targets compile check and strict Clippy. | ~0.75–1.25 h | ~0.25–0.5 h shared-target build wait | Local code gates passed; all-targets test linking is blocked by the unaccepted Xcode license. |
| 2026-09-22 — security and publication | Sealed the complete zero-finding TiDB diff scan, rebased onto concurrent mainline work, committed and pushed `2ce6f753` to `origin/main`. | ~0.5–0.75 h | ~0.5–1 h publication race/reconciliation | Implementation is visible to other threads; hosted exact-head qualification is still pending. |
| 2026-09-22 — next hosted gate | Update both ledgers, dispatch the exact-head W26 packet and retrieve terminal provider/aggregate artifacts. | ~0.5–1 h provisional | ~1–3 h provisional CI/provider queue | No production promotion until every provider, marker, security and reliability gate passes. |

## Current authority override — 2026-09-22, adaptive concurrent mutation batching

This is the newest W26 implementation boundary. The shared chunked coordinator
now keeps the low-latency local-provider path, but continues collecting only
while the mutation queue is growing, targets the 64-worker Ozone qualification
wave, and caps the scheduler window. The change is correctness-preserving: the
existing fenced single-publication/CAS boundary, queue-capacity limit and
cancellation fail-closed path are unchanged. A fixed 64-round variant was
discarded after a local diagnostic latency regression; it is not part of the
published implementation.

| Field | Current value |
| --- | --- |
| Shared build-on tip | `origin/main` = `2dab2386ac86df1256299e0052f81319d1164130` at the current fast-forward before this ledger commit; implementation commit `fe4a6bbf0daf0b5f466536ab8caf864717c74e35` is in its history. Other workstreams added unrelated mainline changes after that implementation push; this ledger publication will advance the shared tip again. |
| Current implementation chunk | `fe4a6bbf` (`perf(w26): adapt mutation batching to concurrent waves`). The coordinator uses an eight-round initial window, extends only across observed queue growth, stops at two idle rounds or 64 queued requests, and caps at 64 rounds. The 64-way create and replace regressions both prove one fenced publication. |
| Local implementation evidence | `cargo fmt --all -- --check`, `git diff --check`, all 21 chunked tests, full locked workspace tests (exit 0; all runnable tests passed and service/native rows remained explicit skips), strict workspace Clippy with `-D warnings` (exit 0), shared-target N-API debug build, and the complete pinned-oracle N-API suite all passed. |
| Diagnostic performance evidence | Local split-SQLite direct-api run `/private/tmp/w26-local-split-sqlite-adaptive.json` (SHA-256 `23246f4a8a6d56ad393713e352967094557ca5fe192744aa6407ef485c632d2c`) completed 400/400 lifecycles with zero timeouts and cleanup failures but measured `631.141356` lifecycle IOPS (`1,901.317334 ms`; p95 write/read/delete `284.050833/96.620334/10.030708 ms`). This is macOS/local split-provider diagnostics, not customer Ozone/R2 acceptance. |
| Security evidence | Diff scan `b5429807-35af-4978-83d4-ed7bcde2d6f5` is sealed with complete coverage over `integrations/mount-rs-chunked/src/lib.rs` and `transports/mount-rs-fuse/src/mount.rs`, 0 reportable findings, and report `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/6c868cb487f0ab13359d3fd79fe26737d51db164_20260922T075316Z_bs0_9ffi/report.md`. It is evidence for captured snapshot digest `codex-security-snapshot/v1:sha256:b4dc9c71c4b0465ff0dc9c7e724b099d3a54ef486372fb7c0193379d5a4772dd`; the scanner warned that repository HEAD changed during the run, so the terminal exact-head packet must still carry a matching current-head security result before production acceptance. |
| Fresh hosted qualification | Manual run `35702188498 <https://github.com/andymac4182/mount-rs/actions/runs/35702188498>` selected shared head `245258d9`. W26 producers were queued at capture: base `ozone` job `106662583505`, compositions `106662583710`, TiDB `106662583344`, FoundationDB `106662583211`; aggregate had not yet produced a terminal packet. |
| Production decision | **NO-GO / 1 of 4 provider rows passed the last terminal packet; current requalification pending**. The prior exact-SHA packet remains the acceptance boundary: SQLite/R2 `1051.976655` passed, PGlite/R2 `837.779225`, TiDB/R2 `463.080116` and FoundationDB/R2 `450.696578` failed. Tier-1 99.99% reliability, five-minute RPO/RTO, secure customer Ozone topology, backup/DR and release ownership remain explicit external or cross-workstream gates. |
| Next action | Retrieve the terminal W26 artifacts for run `35702188498`, bind every provider row to its exact head, and decide whether adaptive batching changes PGlite/TiDB/FoundationDB throughput. If any row still misses, continue with provider-specific correctness-preserving optimization; do not lower, average or skip the per-drive 1,000-IOPS target. |

### Current implementation and production-readiness ledger delta

| Work-item impact | Status / completion | Evidence and remaining action | Provisional estimate / external gate |
| --- | --- | --- | --- |
| W26.3a–d provider compositions and W26.15 hard IOPS gate | **OPEN / 100% adaptive coordinator implementation; hosted requalification pending** | The published chunk changes only bounded mutation collection. The previous terminal packet remains `1/4` provider rows over target; the new run is queued. Local split-SQLite diagnostics are recorded for regression context only and do not replace Ozone/R2 evidence. | ~0.5–1.5 d per provider remediation/review cycle; hosted runners, Ozone/R2/provider capacity and artifact retention are external gates. |
| W26.4 end-to-end Rust/Node/CLI/HTTP/N-API path | **PASS locally / hosted current packet pending** | Full workspace and pinned-oracle suites pass, including 64-way protocol/provider concurrency checks; re-read the current W26 producer markers from one terminal run before promoting acceptance. | ~0.5–1 d review; hosted Ozone and provider fixtures remain external. |
| W26.9–W26.12 verifier, retention and one-revision aggregation | **PASS implementation / current aggregate pending** | Existing verifier remains fail-closed on missing markers, incomplete metrics and hard-target misses. Run `35702188498` must produce one complete exact-head aggregate before this row can close. | ~0.25–1 d review; GitHub scheduling/artifact services are external. |
| W26.7/W26.13 security and customer rollout contract | **OPEN for production / captured diff scan sealed** | Adaptive diff scan is sealed with 0 findings for its two reviewed surfaces, but it warns that repository HEAD changed while scanning; rebind the security result to the terminal exact head before closing the gate. Customer certificates, IAM, secret rotation, tenant isolation, measured 99.99% SLO, five-minute RPO/RTO and backup/DR evidence remain customer/Ozone-owned. | ~1–2 d review; current-head rescan, secure customer deployment and backup/DR are external or cross-workstream gates. |
| P14 final integration-readiness review | **NO-GO / 48% provisional** | The code chunk is published and locally tested, but no remote provider gate changed until the new exact-head packet is terminal. | ~1–2 d after W26.15; customer security/SLO/DR, native/provider qualification and release stream remain gates. |

### Session time log — adaptive batching chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — performance isolation | Compared the fixed 64-round window against the eight-round baseline; the fixed variant was discarded after a local split-SQLite diagnostic at `644.073320` IOPS, with all lifecycles successful but materially higher latency. Implemented the adaptive growth/idle/target/cap policy instead. | ~0.75–1.25 h | ~0.25 h local build/benchmark | Published `fe4a6bbf`; no threshold relaxation and no provider skip. |
| 2026-09-22 — local verification | Ran chunked unit tests, full locked workspace tests, strict Clippy, shared-target N-API build and the complete pinned MountX oracle suite. | ~0.75–1.25 h | ~0.5–1 h shared-target/build wait | All runnable local gates passed; explicit provider/native skips remain external. |
| 2026-09-22 — publication and hosted requalification | Fetched concurrent mainline work, fast-forwarded, committed/pushed `fe4a6bbf`, dispatched manual run `35702188498`, recorded all W26 job IDs and retained the local diagnostic digest. | ~0.25–0.5 h | pending; CI queue/provider startup and execution | Shared `origin/main` contains the chunk; production remains NO-GO pending terminal exact-head provider evidence. |
| 2026-09-22 — security closure and ledger refresh | Completed the compact diff review for the adaptive chunk plus the concurrent FUSE teardown diff, persisted the threat model, sealed scan `b5429807-35af-4978-83d4-ed7bcde2d6f5`, and recorded its complete two-surface/zero-finding result with the current-HEAD warning. | ~0.25–0.5 h | ~0.25 h security workbench finalization | Captured security evidence is sealed; a matching current-head result remains part of the exact-head production gate. |

## Current authority override — 2026-09-22, atomic write path through production wrappers

This is the newest W26 implementation and evidence boundary. The chunk fixes a
real production-path performance defect found while investigating the hosted
per-drive IOPS misses: the N-API `DriverSlot` (and the persistence and
observability wrappers) inherited the generic `FsDriver::write_file` fallback,
which could turn one atomic write into separate create and data publications.
The wrappers now forward the optimized atomic operation, preserving one
metadata publication after immutable block staging. The retained exact-SHA
hosted run is now terminal and remains a fail-closed diagnostic packet: no
provider or aggregate failure is promoted to production acceptance.

| Field | Current value |
| --- | --- |
| Shared build-on tip | `origin/main` = `8520e362710a4b3fe00fd567cf00fcc13e64c222` at the start of this ledger update; the terminal packet's implementation SHA `116e9ed405cdc1eb37634a2fa381ce387be036db` is in its history. The next docs publication will become the new shared tip. |
| Current implementation chunk | `116e9ed4` (`perf(w26): preserve atomic write path through wrappers`). `DriverSlot`, `MountDriver`, `InstrumentedDriver`, and `PersistedFs` now forward `FsDriver::write_file`; the N-API regression asserts a new atomic write advances the metadata revision exactly once. |
| Local implementation evidence | `cargo fmt --all`, `git diff --check`, focused N-API regression (1 passed), full locked workspace tests (all runnable tests passed; service/native rows explicitly ignored), strict workspace Clippy with `-D warnings`, shared-target debug N-API build, and the complete pinned-oracle N-API suite all passed. The suite recorded MountX oracle revision `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`; provider credential and native-mount opt-ins remained explicit skips. |
| Security evidence | Diff scan `2582d7c0-130a-454e-beb3-ffba77169e3e` completed with complete changed-file coverage across the three wrapper surfaces and zero reportable findings. Report: `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/eea79cbe13ae059a447925bf9ac37c4b41d08074_20260922T070839Z_t507xqc7/report.md`. |
| Fresh hosted qualification | Retained manual run `35698854392` selected exact implementation SHA `116e9ed405cdc1eb37634a2fa381ce387be036db`. Producers: base `106651810139` passed; compositions `106651810176`, TiDB `106651810223` and FoundationDB `106651810219` failed; aggregate `106656637297` failed closed. JSON/log artifacts were retained and digested below. |
| Production decision | **NO-GO / 1 of 4 provider rows passed the hard target**. SQLite/R2 reached `1051.976655` IOPS; PGlite/R2 `837.779225`, TiDB/R2 `463.080116` and FoundationDB/R2 `450.696578` failed the required `>=1,000` per-drive target. Tier-1 99.99% reliability, five-minute RPO/RTO, secure customer Ozone topology, backup/DR and release ownership remain explicit external or cross-workstream gates. |
| Next action | Diagnose and implement the next correctness-preserving metadata/provider throughput improvement for PGlite, TiDB and FoundationDB; rerun the exact four-provider packet. Preserve all lifecycle integrity, `RUSTFS_COMBO_FAIL`, artifact and marker failures; do not lower, average or skip the per-drive target. |

### Current implementation and production-readiness ledger delta

The full W26.1–W26.15/P14 ledger immediately below remains the itemized
authority for every work item. This delta records the effect of the current
chunk and separates code completion from hosted/provider acceptance.

| Work-item impact | Status / completion | Evidence and remaining action | Provisional estimate / external gate |
| --- | --- | --- | --- |
| W26.3a–d provider compositions and W26.15 hard IOPS gate | **OPEN / 100% wrapper implementation; 1/4 current provider rows passed** | Terminal run `35698854392`: SQLite/R2 `1051.976655` IOPS (elapsed `1,140.709724 ms`, p95 write/read/delete `179.865148/120.138688/32.928342 ms`); PGlite/R2 `837.779225` (`1,432.358268 ms`, `307.770188/127.094863/48.836423`); TiDB/R2 `463.080116` (`2,591.344259 ms`, `571.281588/251.164974/77.053363`); FoundationDB/R2 `450.696578` (`2,662.545177 ms`, `405.676561/210.387481/40.586330`). Each row had 400/400 successful iterations, 1,200 attempted operations, zero timeouts and zero cleanup failures; the three misses remain failures. | ~0.5–1.5 d per remediation/review cycle; hosted runners, Ozone/R2/provider capacity and artifact retention are external gates. |
| W26.4 end-to-end Rust/Node/CLI/HTTP/N-API path | **PASS locally and base/composition functional markers; hosted full packet FAIL** | Pinned-oracle N-API suite passed smoke, contract, provider/reopen/crash, protocol differential, distribution and aggregation checks. Current Ozone base and composition functional markers, bounded listing, Node/CLI/HTTP/reopen and cleanup passed; TiDB/FDB logs retained `RUSTFS_COMBO_FAIL`, and the aggregate failed. | ~0.5–1 d review; hosted Ozone and provider fixtures remain external. |
| W26.9–W26.12 verifier, retention and one-revision aggregation | **PASS implementation / 100%; terminal aggregate FAIL** | All four provider JSON/log artifacts were retained with digests below. Aggregate job `106656637297` failed after the producer packet; no all-provider acceptance marker was emitted. The verifier remains fail-closed on missing markers, incomplete metrics and hard-target misses. | ~0.25–1 d review; GitHub scheduling/artifact services are external. |
| W26.14 complete packet surface enforcement | **PASS implementation / 100%; hosted acceptance FAIL** | Base policy, gateway, block, failure/restart/reopen and cleanup markers passed. Composition functional markers passed; PGlite failed only its hard IOPS row, while TiDB/FDB also retained `RUSTFS_COMBO_FAIL`; no aggregate pass exists. | ~0.5–1 d review; hosted native/provider fixtures and customer Ozone topology are external. |
| W26.7/W26.13 security and customer rollout contract | **PASS local policy / production acceptance open** | Wrapper diff scan is zero-finding; local credential-free and fail-closed controls remain. Customer certificates, IAM, secret rotation, tenant isolation, measured 99.99% SLO, five-minute RPO/RTO and backup/DR evidence remain customer/Ozone-owned. | ~1–2 d review; secure customer deployment and backup/DR are external and outside W26 implementation. |
| P14 final integration-readiness review | **NO-GO / 48% provisional** | Current code is published with strong local evidence and one current provider row above target, but three providers and the aggregate fail. Releases are owned by another stream, and W26 does not claim deployment. | ~1–2 d after W26.15; customer security/SLO/DR, native/provider qualification and release stream remain gates. |

### Terminal hosted packet — run `35698854392`

This packet is exact-SHA evidence for the wrapper chunk, but it is not a
production acceptance packet. The FoundationDB artifact cannot self-verify the
Git revision (`revisionVerified=false` in its JSON); the workflow head SHA is
therefore the provenance boundary for that row. The generated Node addons made
the producer checkouts dirty, which is an expected artifact-build effect and
not a source-change promotion.

| Producer | Job / result | Artifact files and SHA-256 | Acceptance result |
| --- | --- | --- | --- |
| Ozone base | `106651810139` / success | `ozone-base.log` `7d7c9f97c6e56cee9290e46759dcf8b2cafbd5bf7e5ad63a870c51a42d28aef0`; `ozone-policy.log` `f58a6d905b0d0ca444be1fd422e65566bf339cd858071b0d62deda0b17aa74e2` | PASS: policy positive/negative cases, health/readiness, block contract, bounded gateway failure, restart/reopen, integration and cleanup. |
| SQLite/PGlite compositions | `106651810176` / failure | `ozone-compositions.log` `4abe9a3518cea66c7fde721d5ab4a2ee546df8d3478620b7c35d040fc02c9f2f`; `ozone-iops.json` `27c61d2e804c6cbff9b263433dadbba19dbf24db5d882d4553af087f7b737791` | SQLite PASS at `1051.976655`; PGlite FAIL at `837.779225`; functional/reopen/bounded/Node/CLI/HTTP/cleanup markers passed. |
| TiDB/Ozone | `106651810223` / failure | `ozone-tidb.log` `15b04849a3e2a95dfac897103dcf75157b15ba8de12c818a91a12a61955ca9af`; `ozone-tidb-iops.json` `5a0272d698b8deafdfb09d29d4d0f435843c25875f41e75507c874b165e533c7` | FAIL at `463.080116`; 400/400, zero timeout/cleanup; log retains `RUSTFS_COMBO_FAIL`. |
| FoundationDB/Ozone | `106651810219` / failure | `ozone-foundationdb.log` `347e65e7b691bd4fa083664663000096ed87a2cc5d8f7f650d977911c900e208`; `ozone-foundationdb-iops.json` `fe37f7a4e0edc47c7c645e991d364152234eef0cbeb9bfdd2f7d93c39fdbfa8` | FAIL at `450.696578`; 400/400, zero timeout/cleanup; log retains `RUSTFS_COMBO_FAIL`, and JSON lacks self-verified Git revision. |
| W26 aggregate | `106656637297` / failure | No aggregate artifact was published | FAIL closed: no complete all-provider/end-to-end pass marker. |

The local direct `mount-rs-sqlite` sanity benchmark on macOS is deliberately
not promoted to Ozone evidence: its combined local SQLite topology produced
`14.941059` lifecycle IOPS with 337/400 successful iterations and 63 timeout /
cleanup-deferred rows under the synthetic 64-way load. It is retained as a
diagnostic boundary, not as a replacement for the customer-facing SQLite/R2
composition result.

### Session time log — wrapper forwarding chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — defect isolation and implementation | Traced hosted IOPS misses through ChunkedFs, R2 caching and the N-API wrapper path; found the inherited generic `write_file` fallback and added forwarding through N-API, persistence and observability wrappers plus the one-publication regression. | ~1–2 h | ~0.25 h code/build inspection | Correctness-preserving implementation complete; no target relaxation. |
| 2026-09-22 — local verification | Ran formatting/diff checks, focused regression, full locked workspace tests, strict workspace Clippy, shared-target debug N-API build and complete pinned-oracle N-API suite. | ~0.75–1.25 h | ~0.5–1 h shared target/build wait | All runnable local gates passed; provider credentials/native mount opt-ins remained explicit skips. |
| 2026-09-22 — security review | Completed standard changed-file security diff scan `2582d7c0-130a-454e-beb3-ffba77169e3e`. | ~0.5–0.75 h | 0 h hosted | Complete coverage and zero reportable findings. |
| 2026-09-22 — publication | Committed, fetched concurrent mainline work, rebased, pushed and verified the implementation and ledger chunks; dispatched retained manual run `35698854392` and retrieved all terminal producer artifacts. | ~0.25–0.5 h | ~1.5–3 h CI queue/provider startup and provider execution | SQLite/R2 passed the hard target; PGlite/TiDB/FoundationDB and the aggregate failed closed. The next implementation chunk is required; production remains NO-GO. |

## Current authority override — 2026-09-22, FoundationDB transaction-sharing chunk

This is the newest implementation and production-readiness boundary. The
FoundationDB code is published on the shared mainline, but no hosted packet
has qualified that exact revision yet. The prior terminal packet and the
fully completed producer portion of targeted run `35693778762` remain
diagnostic because its aggregate is still queued; neither can be promoted to
production acceptance.

| Field | Current value |
| --- | --- |
| Shared build-on tip | Reconciled against `origin/main` = `07a6b501648120039b2f0ec8c83e549a64a84033` before this ledger chunk; the worktree is clean and this documentation publication is the next shared tip. The current W26 implementation code is the ancestor `5e3680117c26f5b7bb1b9280eec65a350b3dd2f7`; other threads should build on the full remote tip and verify it with `git ls-remote`. |
| Current implementation chunk | `5e368011` (`perf(w26): share fdb lease authority transaction`). `LeaseOracle` now has a source-compatible default `now_ms_in_transaction` hook. The production `FoundationDbSharedLeaseOracle` reads its protected authority key in the already-open metadata transaction, and acquire/renew/release/publish use that hook. Custom clocks and the legacy/persisted oracle retain the default asynchronous path; authority reads remain read-only and missing/malformed samples fail closed. |
| Local implementation evidence | `cargo fmt --all`, `git diff --check`, FoundationDB feature-enabled `cargo check -p mount-rs-foundationdb --features foundationdb --all-targets --locked`, feature-enabled strict Clippy, full `./scripts/cargo-shared test --workspace --all-targets --locked`, and strict workspace Clippy with `-D warnings` all passed. The feature-enabled FoundationDB test command reached linking but could not link locally because native `fdb_c` is unavailable; this is an external native gate, not a test pass. |
| Security evidence | Diff scan `2eb14a81-ce6f-4c30-bf98-a3e65479a8cd` completed and sealed with complete changed-file coverage, one reviewed surface and zero reportable findings. Usage: `1,200,080` total tokens, `1,195,542` input, `1,145,856` cached input. Report: `/private/var/folders/qx/1pyrtldd3nb1l0p44xbmd97h0000gn/T/codex-security-scans-MW6oGi/mount-rs/7d49e04b541978c278884fd92e918e689ed3892e_20260922T062232Z_8b0ncpw_/report.md`. |
| Latest hosted evidence | Manual retained run `35695427227` selected exact SHA `e7850fb41775351503e5aa685484906b3a3cbbe4`: base `106641134190`, compositions `106641134304`, TiDB `106641134221` and FoundationDB `106641134132` remain queued, with no aggregate at the latest capture. Targeted run `35693778762` has now completed all four producers but its aggregate `106641631860` remains queued; exact diagnostic metrics and artifacts are recorded below. Earlier push run `35694753908` selected `5e368011` but was canceled by ordinary-push concurrency. |
| Production decision | **NO-GO**. The latest complete aggregate packet had only PGlite above the 1,000-IOPS-per-drive threshold; the newer targeted producer packet missed the threshold for all four providers and has no terminal aggregate. Customer deployment, secure Ozone topology, 99.99% availability, five-minute RPO/RTO, backup/DR and release ownership remain explicit external boundaries. |
| Next action | Let manual run `35695427227` finish on exact `e7850fb4`, poll targeted aggregate `106641631860` once it becomes terminal, retrieve current-code W26 artifacts/logs/digests, and update acceptance only from one-revision all-provider/end-to-end results. Preserve the native `fdb_c` gate as external until hosted FoundationDB qualification is terminal. |

### Current W26 work-item ledger — exact status at this boundary

Percentages distinguish W26-owned implementation from hosted/native/provider
qualification. Estimates are provisional; hosted queue, provider startup and
customer/Ozone gates are not implementation time.

| Work item | Status / completion | Evidence | Remaining action | Provisional engineering-time estimate | External blocker / gate |
| --- | --- | --- | --- | ---: | --- |
| W26.1 — pinned Ozone gateway and health/readiness harness | **PASS / 100% implementation; current exact-SHA recheck pending** | Manual run `35695427227`, base job `106641134190` is queued on exact `e7850fb4`; prior terminal packet also passed gateway, readiness, restart/reopen and cleanup markers | Reconfirm markers on current `e7850fb4` packet | 0–1 h review | Hosted image/runner and customer Ozone topology |
| W26.2 — immutable block contract through Ozone S3 gateway | **IMPLEMENTED / 100% code; current exact-SHA acceptance pending** | Prior composition/base logs passed create/CAS, binary/range read, stopped-gateway fault, restart/reopen and owned-cleanup markers; targeted compositions also emitted the block/fault/gateway/restart/reopen and cleanup passes but failed its hard IOPS row | Reconfirm all markers on current exact revision; retain failures as non-acceptance evidence | 0.5–1 d review | Ozone gateway capacity, customer TLS/IAM and tenant isolation |
| W26.3a — SQLite/R2 composition | **FAIL / 100% implementation; hosted performance unresolved** | Targeted run `35693778762` measured `580.6091135` IOPS (elapsed `2,066.794978 ms`, p95 write/read/delete `575.803992/56.088376/30.611269 ms`); prior terminal row was `213.947686` and an earlier packet reached `1,325.636778`, proving high hosted variance. Both rows had 1,200/1,200, timeout `0`, cleanup `0` | Diagnose write variance without changing the hard target; requalify exact current SHA | 0.5–1.5 d per review/remediation cycle | Hosted runner variance, Ozone/provider capacity |
| W26.3b — PGlite/R2 composition | **OPEN / 100% implementation; hosted performance unresolved** | Targeted run measured `963.6041014` IOPS (elapsed `1,245.324712 ms`, p95 write/read/delete `340.233915/16.919748/6.393952 ms`), while the prior terminal row reached `2,065.669446`; both are 1,200/1,200 with timeout `0`, cleanup `0`. Node/Rust/CLI/HTTP/reopen markers passed in the retained packets | Retain the provider variance evidence, then close PGlite beside all other providers in one-revision aggregate | 0.5–1 d review | Hosted PGlite runtime, Ozone latency and runner capacity |
| W26.3c — FoundationDB/R2 composition | **IMPLEMENTED / 100% code; hosted performance unresolved** | `5e368011` shares the protected authority read with the metadata transaction. Targeted run measured `337.4773006` IOPS (elapsed `3,555.794709 ms`, p95 write/read/delete `555.893010/59.469158/28.423729 ms`); prior terminal pre-chunk row was `363.254472`; both had 1,200/1,200, timeout `0`, cleanup `0`. Manual current-code FDB job `106641134132` remains queued | Complete current exact-SHA FoundationDB qualification and compare transaction/read latency without weakening fencing | 0.5–1.5 d per review/remediation cycle | Native `fdb_c`, hosted FoundationDB image/client, Ozone topology and durability |
| W26.3d — TiDB/R2 composition | **IMPLEMENTED / 100% code; hosted performance unresolved** | `4098c7df` and the current mainline include the autocommit CAS chunk. Targeted run measured `385.2624172` IOPS (elapsed `3,114.760087 ms`, p95 write/read/delete `522.415305/117.581114/49.209468 ms`); prior terminal row was `333.356025`; both had 1,200/1,200, timeout `0`, cleanup `0`. Manual current-code TiDB job `106641134221` remains queued | Requalify current exact SHA and compare IOPS/p95; preserve fencing and ambiguous-commit markers | 1–3 d per review/remediation cycle | Hosted TiDB/PD/TiKV, Ozone topology and provider-native latency |
| W26.4 — Node factories, Rust/Node/CLI and HTTP surfaces | **IMPLEMENTED / 100% local; prior packet markers passed** | Latest composition logs: Node provider matrix `pass=7 skip=1 fail=0`, Rust/Node CLI, remote HTTP, bounded listing and reopen passed | Reconfirm every advertised surface in the next aggregate | 0.5–1.5 d review | Hosted native/N-API prerequisites and Ozone runtime |
| W26.5 — explicit immutable-block reconciliation | **IMPLEMENTED / 100% local contract** | Opt-in, prefix-scoped, lease/fence-checked and fail-closed reconciliation; local provider/ChunkedFs/SDK/observability gates pass | Obtain hosted retention, alerting and customer operational evidence; no implicit shutdown cleanup | 0.5–1.5 d review | Provider enumeration, retention policy and customer operations |
| W26.6 — bounded remote directory enumeration | **IMPLEMENTED / 100% local contract** | Bounded Rust/HTTP/CLI/N-API/KV paths and overflow behavior pass locally; Ozone marker passed previously | Reconfirm provider-backed success/overflow markers on the exact current revision | 0.5–1.5 d review | Hosted provider surfaces and customer scale/topology |
| W26.7 — credential-free production configuration policy | **IMPLEMENTED / 100% local policy** | Provider-positive/security-negative fixtures pass without opening providers or printing secrets; hosted policy markers passed | Validate customer certificates, IAM, rotation and runtime readback | 0.5–1.5 d review | Customer secure Ozone runtime and provider-native security |
| W26.8 — strict requested-provider/no-skip qualification | **IMPLEMENTED / 100% local verifier** | `--require-configured` and provider-specific jobs fail closed on missing providers; latest packet ran all four rows | Preserve strict selection in the fresh exact-SHA run | 0.25–0.75 d review | Hosted credentials, provider startup and CI scheduling |
| W26.9 — fixed profile and hard 1,000-IOPS artifact verifier | **IMPLEMENTED / 100% local verifier** | Fixed 4 KiB/400-iteration/concurrency-64 profile, finite metrics, lifecycle/cleanup/stat checks and pass-marker ordering remain enforced | Accept only terminal artifacts with every provider pass marker | 0.5–1 d review | Hosted artifact service and provider performance |
| W26.10 — fail-closed artifact retention | **IMPLEMENTED / 100% local CI control** | Expected JSON uploads use `if-no-files-found: error`; latest artifacts were retained and digest-recorded | Recheck retention/digest evidence on the next packet | 0.25–0.5 d review | GitHub artifact service and retention policy |
| W26.11 — one-revision evidence aggregation | **IMPLEMENTED / 100% local verifier; current packets pending** | Aggregate `106631474430` rejected missing `OZONE_IOPS_PASS` for SQLite/R2 and PGlite/R2; targeted aggregate `106641631860` was queued after all four producer failures; manual run `35695427227` still has no aggregate at latest capture | Wait for both exact-SHA aggregate state/results, then accept only a terminal all-provider marker set | 0.5–1.5 d review | Workflow scheduling and artifact availability |
| W26.12 — incomplete metric/lifecycle rejection | **IMPLEMENTED / 100% local verifier** | Latest rows had finite metrics, complete 1,200/1,200 lifecycle counts and zero timeout/cleanup failures, but hard misses stayed failures | Preserve integrity checks; do not convert target misses to skips | 0.25–0.75 d review | Hosted artifact correctness and provider behavior |
| W26.13 — credential-free customer rollout contract | **IMPLEMENTED / 100% declaration contract** | Contract encodes Tier-1 99.99%, five-minute RPO/RTO, secure Ozone, tenant scope, all four providers and customer-owned backup/restore | Customer must supply measured SLO/RPO/RTO, certificates/IAM/rotation and DR evidence | 1–2 d review | Customer/Ozone ownership; backup and DR are outside W26 |
| W26.14 — complete end-to-end packet surface enforcement | **IMPLEMENTED / 100% local control; hosted acceptance open** | Aggregate requires gateway, provider compositions, Rust/Node/CLI/HTTP, bounded listing, TiDB/FDB restart/N-API and cleanup markers; prior aggregate failed closed on performance marker absence, targeted producer logs included TiDB/FDB `RUSTFS_COMBO_FAIL`, and the manual packet has no aggregate at latest capture | Produce one terminal all-surface packet on exact `e7850fb4` and resolve every required marker | 0.75–1.5 d review | Native runners, provider fixtures and CI orchestration |
| W26.15 — P8 per-drive 1,000-IOPS gate | **OPEN / 100% verifier implementation; 0 of 4 targeted producer rows passed** | Targeted run rows: SQLite `580.6091135`, PGlite `963.6041014`, TiDB `385.2624172`, FoundationDB `337.4773006` IOPS; all 1,200/1,200 with zero timeout/cleanup failures, but all missed 1,000. The prior terminal aggregate still has PGlite as the only passing row; no targeted aggregate pass exists | Close all four providers and aggregate on one exact SHA; never lower, average or skip the hard target | 1.5–4 d per remediation cycle plus external queue | Hosted runner/provider capacity, Ozone topology and artifact retention |
| P14 — final integration-readiness review | **NO-GO / 45% provisional** | Local code, security and verifier gates are strong; current code has no exact-SHA hosted acceptance yet, and the latest terminal packet failed the hard target; customer security/SLO/DR/native/release evidence is not W26-owned | Re-audit a terminal all-provider/end-to-end pass and issue an explicit readiness decision; no deployment or release claim | 1–2 d after W26.15 | Customer Ozone security/SLO/DR and separate release stream |

### Current session time log continuation — TiDB chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — TiDB autocommit publication implementation | Added the successful-path autocommit conditional CAS, retained zero-row locked classification, preserved fence/revision/expiry predicates, and added statement-conflict versus ambiguous-acknowledgement tests. | ~1.5–2.5 h | ~0.25–0.5 h shared Cargo target/build wait | Focused TiDB tests, formatting and diff checks passed; implementation committed as `c9b47b5c` before mainline reconciliation. |
| 2026-09-22 — Workspace verification | Ran the complete locked workspace test suite and strict workspace Clippy with `-D warnings`. | ~0.75–1.25 h | ~0.5–1 h shared target/build wait | All runnable workspace tests passed; service-gated/native rows stayed explicitly ignored; Clippy passed. |
| 2026-09-22 — TiDB security review | Completed diff scan `c36f104e-965b-4e85-bfde-2d3d222f0de2` over the changed provider path and supporting storage contract. | ~0.5–0.75 h | 0 h hosted | Complete coverage, two reviewed surfaces, zero reportable findings; sealed report retained outside the repository. |
| 2026-09-22 — Code publication/reconciliation | Fetched concurrent mainline work, rebased the TiDB commit and pushed the code chunk without force-pushing. | ~0.25–0.5 h | ~0.5–1 h remote reconciliation | `4098c7df04a03f65269ef932cb921b96d6368297` verified on `origin/main`; worktree clean. |
| 2026-09-22 — Next hosted gate | Prepare this ledger/tracker publication, dispatch the exact resulting SHA, and retrieve terminal W26 artifacts before updating acceptance. | ~0.5–1 h documentation/evidence | ~1–3 h provisional CI queue/provider startup | No production promotion; the next packet must remain fail-closed until all providers and the aggregate pass. |

### Current session time log continuation — FoundationDB transaction-sharing chunk

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — FoundationDB implementation | Added the source-compatible transaction-aware oracle hook, shared-authority read helper, specialized same-transaction authority lookup, and four metadata mutation call sites. Preserved read-only authority access, conflict/read-version semantics, and fail-closed missing/malformed handling. | ~1–2 h | ~0.25–0.5 h code inspection | FoundationDB feature-enabled code compiles; the optimization is limited to the production shared authority path. |
| 2026-09-22 — FoundationDB local verification | Ran formatting/diff checks, feature-enabled package check, feature-enabled strict Clippy, full locked workspace tests and strict workspace Clippy. | ~0.5–1 h | ~0.5–1 h shared target/build wait | Workspace tests and Clippy passed. Feature-enabled FoundationDB tests reached link but remain blocked locally by missing native `fdb_c`. |
| 2026-09-22 — FoundationDB security review | Completed diff scan `2eb14a81-ce6f-4c30-bf98-a3e65479a8cd` with preflight, threat-model, changed-file discovery, validation and sealed report. | ~0.5–0.75 h | 0 h hosted | One surface, complete coverage, zero reportable findings; report retained outside the repository. |
| 2026-09-22 — FoundationDB code publication | Committed, fetched concurrent mainline work, rebased without force-pushing, and verified the exact commit on `origin/main`. | ~0.25–0.5 h | ~0.25–0.75 h remote reconciliation | `5e3680117c26f5b7bb1b9280eec65a350b3dd2f7` is the shared build-on tip; a push-triggered CI run is pending. |

## Current hosted dispatch override — manual run `35695427227`

This retained manual dispatch is the authoritative hosted qualification
boundary for the current mainline tip. It was started after ordinary push
concurrency canceled the earlier automatic run; its private workflow group
keeps provider and aggregate evidence from being canceled by unrelated
mainline pushes.

| Producer | Job ID | Capture state | Acceptance rule |
| --- | ---: | --- | --- |
| Ozone base | `106641134190` | queued | Must pass gateway policy, block contract, failure/restart/reopen and cleanup markers |
| Ozone compositions | `106641134304` | queued | SQLite/R2 and PGlite/R2 must each complete the fixed lifecycle and meet >=1,000 IOPS with all composition markers |
| Ozone TiDB | `106641134221` | queued | Must pass TiDB durable/restart/bounded-listing markers and >=1,000 IOPS |
| Ozone FoundationDB | `106641134132` | queued | Must pass strict lockfile/preflight, durable restart, bounded-listing, cleanup and >=1,000 IOPS |
| W26 aggregate | not created at capture | pending | Must verify exact-SHA artifacts and emit the complete all-provider/end-to-end aggregate pass marker |

| Field | Current value |
| --- | --- |
| Selected revision | `e7850fb41775351503e5aa685484906b3a3cbbe4` (`origin/main` at dispatch); it contains implementation chunk `5e368011` and the previously published W26 ledger. |
| Run | [GitHub Actions run `35695427227`](https://github.com/andymac4182/mount-rs/actions/runs/35695427227) — queued at the first capture; producer jobs are not acceptance evidence until terminal. |
| Acceptance state | **NO-GO / pending**. Any failed, skipped, canceled, incomplete, missing-artifact or missing-marker producer/aggregate result remains fail-closed. |
| Next action | Poll the four producer jobs and the aggregate, retain exact-SHA artifacts/logs/digests, then update this ledger only after the complete packet is terminal. |

### Session time log — retained manual hosted qualification

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — Manual exact-SHA dispatch | Dispatched `ci.yml` with `workflow_dispatch` so its private concurrency group preserves W26 evidence across unrelated pushes; recorded all four producer job IDs. | ~0.25–0.5 h evidence coordination | ~1–3 h provisional CI queue/provider startup | Run `35695427227` is the current hosted qualification boundary for `e7850fb4`; production remains **NO-GO** until the all-provider aggregate is terminal and passing. |
| 2026-09-22 — Targeted packet retrieval and review | Downloaded and parsed the four producer artifacts from run `35693778762`, verified the exact revision, lifecycle counts, latency metrics, artifact IDs/digests and marker boundaries. | ~0.75–1.25 h evidence review | ~1–2 h hosted provider startup/artifact service | All four producers are terminal failures on the 1,000-IOPS gate; aggregate `106641631860` remains queued, so this is diagnostic only. |

## Historical hosted dispatch override — run `35693778762`

The targeted W26 qualification was dispatched after the TiDB code and ledger
publication. It selected the exact ledger SHA below, so it is the preferred
diagnostic packet for `4098c7df`. All four W26 producer jobs are now terminal,
but the aggregate job is still queued; a run-level API status also remains
queued despite the terminal producer jobs. A concurrent mainline push later
advanced the shared branch and the retained manual run `35695427227` selected
an earlier exact SHA; that newer run is the current qualification boundary and
does not replace this exact-SHA diagnostic evidence.

| Producer | Job ID | Capture state | Acceptance rule |
| --- | ---: | --- | --- |
| Ozone base | `106636116163` | completed / success | Must pass gateway policy, block contract, failure/restart/reopen and cleanup markers |
| Ozone compositions | `106636116105` | completed / failure | SQLite/R2 and PGlite/R2 must each complete the fixed lifecycle and meet >=1,000 IOPS with all composition markers |
| Ozone TiDB | `106636116197` | completed / failure | Must pass TiDB durable/restart/bounded-listing markers and >=1,000 IOPS |
| Ozone FoundationDB | `106636116201` | completed / failure | Must pass strict lockfile/preflight, durable restart, bounded-listing, cleanup and >=1,000 IOPS |
| W26 aggregate | `106641631860` | queued at latest capture | Must verify exact-SHA artifacts and emit the complete all-provider/end-to-end aggregate pass marker |

| Field | Current value |
| --- | --- |
| Selected revision | `b3fb7988bce1edaafbd44f2adf22bb217ca99671` (`origin/main` when dispatched); it contains code `4098c7df` and the published W26 ledger/tracker. |
| Run | [GitHub Actions run `35693778762`](https://github.com/andymac4182/mount-rs/actions/runs/35693778762) — all four producers are terminal failures, while aggregate `106641631860` is queued; producer results are not acceptance evidence until the complete packet is terminal. |
| Concurrent shared tip | `origin/main` was reconciled to `07a6b501648120039b2f0ec8c83e549a64a84033` before this ledger update. Retained manual run `35695427227` selected exact SHA `e7850fb41775351503e5aa685484906b3a3cbbe4`; it is the current qualification boundary, while this older targeted run remains exact-SHA diagnostic evidence. |
| Acceptance state | **NO-GO / diagnostic non-acceptance**. SQLite/R2 `580.6091135`, PGlite/R2 `963.6041014`, TiDB/R2 `385.2624172` and FoundationDB/R2 `337.4773006` all missed the hard target; TiDB/FDB logs also emitted `RUSTFS_COMBO_FAIL`; the queued aggregate cannot close the gate. |
| Next action | Poll aggregate `106641631860` for its terminal fail-closed marker, then let `35695427227` run on exact `e7850fb4`; retrieve every current-code artifact/log/digest before changing acceptance. |

### Targeted producer metrics and artifact integrity — run `35693778762`

Each provider completed 400 writes, 400 full-byte reads/verifications and 400
deletes: 1,200/1,200 successful lifecycle operations, zero timeouts and zero
cleanup failures. Every row missed the hard 1,000-IOPS threshold, so no
`OZONE_IOPS_PASS` marker was emitted. These are diagnostic rows only because
aggregate `106641631860` was queued at the latest capture.

| Provider | IOPS / elapsed | p95 write / read / delete | Artifact ID / digest | Marker boundary |
| --- | ---: | ---: | --- | --- |
| SQLite/R2 | `580.6091135` / `2,066.794978 ms` | `575.803992 / 56.088376 / 30.611269 ms` | `10680155777` / `sha256:5af394db93c6c9b942a93da4db3fcef475bf00bd6a329e77aa5d8860875884dc` | Block/fault/gateway/restart/reopen, bounded-listing, Node/HTTP and cleanup markers passed; hard IOPS target failed |
| PGlite/R2 | `963.6041014` / `1,245.324712 ms` | `340.233915 / 16.919748 / 6.393952 ms` | same composition artifact | Bounded/reopen, Node/HTTP and cleanup markers passed; hard IOPS target failed |
| TiDB/R2 | `385.2624172` / `3,114.760087 ms` | `522.415305 / 117.581114 / 49.209468 ms` | `10680225852` / `sha256:d3ec5fb782a0b4e2ab20cc85ac9230e2c6de276af9ce9e948b23e18e7e50d9da` | Block/fault/gateway/restart/reopen, bounded-listing and cleanup markers passed; `RUSTFS_COMBO_FAIL` and hard IOPS target failed |
| FoundationDB/R2 | `337.4773006` / `3,555.794709 ms` | `555.893010 / 59.469158 / 28.423729 ms` | `10680357233` / `sha256:7d2714941a3e8e6e42b76de5a0468801e0d9728741e0fa0ecf1760accd67270c` | Block/fault/gateway/restart/reopen, bounded-listing and cleanup markers passed; `RUSTFS_COMBO_FAIL` and hard IOPS target failed |

The base Ozone artifact was `10678929400` with digest
`sha256:35bd3a0587fb494a7088dc9144afc8c2fab0dcb7bae23b54f1c1bdeb74bcc64b`.
No aggregate pass or acceptance marker exists for this packet. The `RUSTFS`
combo failures are retained as an additional end-to-end boundary for the next
current-code run; they are not silently reclassified as unrelated or skipped.

### Session time log — hosted dispatch

| Date / phase | Activity | Engineering time | External wait / gate time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-22 — Exact-SHA W26 dispatch | Dispatched `ci.yml` from the published ledger SHA, recorded the four W26 producer job IDs, and reconciled the subsequent current-code push run. | ~0.25–0.5 h evidence coordination | ~1–3 h provisional CI queue/provider startup | Run `35693778762` is a completed-producer diagnostic packet for the TiDB chunk; aggregate `106641631860` is queued. Manual run `35695427227` is the current qualification boundary for `e7850fb4`. Production remains **NO-GO** until terminal all-provider/end-to-end evidence passes. |

## Current authority override — 2026-09-22, terminal run `35691451007`

This is the newest hosted evidence boundary. The run exercised the published
PGlite implementation on one exact revision, but it is diagnostic rather than
acceptance evidence because three provider rows missed the hard threshold and
the aggregate correctly failed closed. Earlier pending-dispatch text remains
below for auditability.

| Field | Current value |
| --- | --- |
| Shared build-on tip | `origin/main` = `8ab5fc20c95d503cde895a430bc9ac2e2fa5cc04`; the hosted run selected its code-equivalent parent `dccd8351690ba21b4ea01ab8680369c76c442041` before the documentation-only dispatch record. |
| Terminal W26 jobs | Base Ozone `106629129201` **passed**; compositions `106629129102`, TiDB `106629129183`, FoundationDB `106629129105` and aggregate `106631474430` **failed**. All W26 jobs are terminal; unrelated workflow jobs may still be running. |
| Provider outcome | SQLite/R2 `213.947686` IOPS **FAIL**; PGlite/R2 `2,065.669446` **PASS**; TiDB/R2 `333.356025` **FAIL**; FoundationDB/R2 `363.254472` **FAIL**. Each row completed 1,200/1,200 lifecycle operations with zero timeouts and zero cleanup failures. |
| Retained artifacts | Base `10679061163`, digest `sha256:8a2633ea51b3a0d64bb73e57bdbe02ec1849ddfa0bdc1901336ac8567f1d9f53`; compositions `10679446128`, digest `sha256:eaa7a727830512afccdc3fc2733444c330ac60b4db11093e98105705eaba447f`; TiDB `10679510692`, digest `sha256:d00f662fd252651d96dbca472809b4fc525ac3e8af13b04df6958df702db0e2c`; FoundationDB `10679336769`, digest `sha256:c176c7ff5859363c0ce85e8d1937e040d7fade7e207bac4d137de1accb77c33d`. All were retained and non-expired. |
| Aggregate evidence | `W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000`. The fail-closed verifier rejected the packet because SQLite missed the threshold; this is the correct result. |
| Current production decision | **NO-GO**. PGlite now clears the hard target on this exact hosted packet, but SQLite is highly variable (`1,325.636778` on the prior diagnostic packet versus `213.947686` here), and TiDB/FoundationDB remain below target. Next work is correctness-preserving SQLite variance diagnosis plus a narrow TiDB publication-path optimization; no threshold reduction or provider skip is permitted. |

### Terminal provider metrics — run `35691451007`

| Provider | IOPS / elapsed | p95 write / read / delete | Lifecycle and markers | Status / next action |
| --- | ---: | ---: | --- | --- |
| SQLite/R2 | `213.947686` / `5,608.847758 ms` | `1,751.446370 / 105.174762 / 202.276166 ms` | 1,200/1,200; timeout `0`; cleanup `0`; Ozone gateway, restart/reopen, Node/Rust/CLI/HTTP and cleanup markers passed | **FAIL**; diagnose hosted write variance and requalify exact current SHA |
| PGlite/R2 | `2,065.669446` / `580.925473 ms` | `106.518128 / 7.567658 / 2.705368 ms` | 1,200/1,200; timeout `0`; cleanup `0`; composition functional/bounded/reopen markers passed | **PASS for this provider row**; retain as evidence but W26.15 remains open |
| TiDB/R2 | `333.356025` / `3,599.754942 ms` | `629.659611 / 36.006496 / 29.342427 ms` | 1,200/1,200; timeout `0`; cleanup `0`; TiDB identity, bounded listing, N-API seed/reopen, Ozone restart and cleanup markers passed | **FAIL**; evaluate autocommit CAS success path while preserving fail-closed ambiguity handling |
| FoundationDB/R2 | `363.254472` / `3,303.469310 ms` | `528.842275 / 97.605869 / 85.797321 ms` | 1,200/1,200; timeout `0`; cleanup `0`; durable readiness/image match, authority heartbeat, bounded listing, restart/reopen, N-API and cleanup markers passed | **FAIL**; retain durable markers and investigate transaction/runner/provider latency after TiDB chunk |

All provider-specific failures are performance-only in this packet: no
timeout, cleanup, lifecycle, marker, revision, restart or security-verifier
failure was promoted. The hosted JSON reported the source revision as verified
`dccd8351` and a generated native artifact made the checkout dirty; the
aggregate still bound all artifacts to the exact Git revision and rejected the
packet on the required hard marker, not on source mismatch.

## Current authority override — 2026-09-22, exact-SHA qualification dispatch

The PGlite implementation and its ledger are published, and the next hosted
qualification is now dispatched against the exact shared tip. This block is
newer than the terminal diagnostic packet below; it records pending state only
and does not promote queued or in-progress work to acceptance.

| Field | Current value |
| --- | --- |
| Shared implementation tip | `origin/main` = `dccd8351690ba21b4ea01ab8680369c76c442041`; code chunk `e0180c75` is present and the ledger/tracker publication is `dccd8351`. |
| Hosted qualification run | Run `35691451007` selected exact SHA `dccd8351690ba21b4ea01ab8680369c76c442041`; URL: `https://github.com/andymac4182/mount-rs/actions/runs/35691451007`. Parent status at dispatch capture: `queued`. |
| Initial W26 job state | `ozone-compositions` `106629129102` queued; `ozone-foundationdb` `106629129105` in progress; `ozone-tidb` `106629129183` in progress; base `ozone` `106629129201` in progress; aggregate job not yet present. |
| Acceptance state | **NO-GO / pending**. This run is the first packet eligible to test PGlite `e0180c75`, but it is not evidence until all W26 producers and the one-revision aggregate are terminal; any failed provider or missing pass marker remains fail-closed. |
| Next action | Poll the W26 jobs, download retained artifacts/logs after terminal completion, record exact provider metrics and update this ledger before deciding whether another correctness-preserving implementation chunk is justified. |

## Current authority override — 2026-09-22, PGlite publication chunk

This is the newest shared build-on boundary. Older current-status blocks remain
below as an audit trail, but this section is authoritative for the next W26
implementation and qualification work.

| Field | Current value |
| --- | --- |
| Shared implementation tip | `origin/main` = `e0180c75a190340f2c0a45265803de9df5605d59`; this published chunk adds the PGlite successful-publication autocommit fenced CAS path on top of the SQLite publication fast path and the earlier W26 safety/performance/evidence controls. |
| Current implementation chunk | `e0180c75` (`perf(w26): use pglite autocommit publication fast path`). PGlite now acknowledges one parameterized conditional UPDATE on the successful path; only a zero-row outcome opens the existing explicit locked classification transaction. Missing rows, stale leases, revision conflicts, unexplained zero-row outcomes, rollback handling and fail-closed errors remain intact. |
| Local verification | `cargo fmt --all -- --check`, `git diff --check`, the focused PGlite package test/compile gate, the full `./scripts/cargo-shared test --workspace --all-targets --locked` run and strict workspace Clippy with `-D warnings` passed. The focused PGlite package reported five server-dependent tests ignored because no isolated PGlite server was available locally; this is an external/provider gate, not a local failure. |
| Security evidence | Diff scan `6906585f-77c3-41fa-afe0-06ab4df9e2c6` completed and sealed with complete changed-file coverage, three reviewed surfaces and zero reportable findings. Measured usage: 700,532 total tokens, 698,889 input, 690,432 cached input. |
| Latest hosted packet | Run `35689474986` selected exact SHA `1891c36375296bc3695a9d71233c624cad46445c`, which predates `e0180c75`; W26 jobs are terminal: base `106623193674` passed, compositions `106623193668` failed, TiDB `106623193474` failed, FoundationDB `106623193564` failed and aggregate `106625464560` failed closed. The parent workflow remains `in_progress` only for unrelated jobs. |
| Current production decision | **NO-GO**. SQLite passed the per-drive threshold in the diagnostic packet, but PGlite, TiDB and FoundationDB did not; the packet cannot qualify the new PGlite chunk, and no all-provider/aggregate acceptance marker exists. A fresh exact-SHA packet on `e0180c75` is required. |

### Current work-item ledger — exact current status

The percentages below separate implementation completion from hosted/provider
qualification. A PASS in one provider row does not close W26.15: all four
configured providers, the end-to-end markers and the aggregate must pass on one
exact revision.

| Work item | Status / completion | Evidence | Remaining action | Provisional engineering-time estimate | External blocker / gate |
| --- | --- | --- | --- | ---: | --- |
| W26.1 — pinned Ozone gateway and health/readiness harness | **PASS / 100% implementation and hosted base gate** | Base job `106623193674` passed readiness, gateway policy, pinned-image, restart/reopen and cleanup markers; artifact `10678332701` (`sha256:b7cd42dcd121cb07a6c13f1d11d7209aa9a58466642e43340362141e9ac02f3f`) | Preserve the exact base artifact and markers in the next one-revision packet | 0–1 h review | Hosted runner/image and customer Ozone topology |
| W26.2 — immutable block contract through Ozone S3 gateway | **PASS / 100% current functional gate** | The same base/composition packet passed create/CAS, binary/range read, stopped-gateway fault, restart/reopen and owned-cleanup markers | Reconfirm on the exact post-`e0180c75` packet | 0.5–1 d review | Ozone gateway capacity, customer TLS/IAM and tenant isolation |
| W26.3a — SQLite/R2 composition | **PASS in diagnostic packet / 100% current IOPS row, not aggregate acceptance** | `1,325.636778` IOPS, elapsed `905.225338 ms`, p95 write/read/delete `152.742148/16.861092/15.101845 ms`; 1,200/1,200 operations, zero timeouts/cleanup failures; composition artifact `10678002197` (`sha256:227b752d82d54052ff3ad176618e89a49c17cc80c4fad9c9c2dd4bd165e638f2`) | Re-run on exact `e0180c75` and retain the pass marker beside the other providers | 0.5–1.5 d hosted review | Hosted runner variance and Ozone/provider capacity |
| W26.3b — PGlite/R2 composition | **FAIL / 100% lifecycle, 45% performance qualification** | `766.631418` IOPS, elapsed `1,565.289358 ms`, p95 write/read/delete `258.600725/25.202828/15.859995 ms`; 1,200/1,200 operations, zero timeouts/cleanup failures; composition artifact `10678002197` | Re-run after `e0180c75`; if still below 1,000, continue correctness-preserving provider/publication work without changing the target | 1.5–4 d per remediation/review cycle | Hosted PGlite runtime, Ozone latency and runner capacity |
| W26.3c — FoundationDB/R2 composition | **FAIL / 100% functional markers, 41% performance qualification** | `410.703639` IOPS, elapsed `2,921.814870 ms`, p95 write/read/delete `457.390808/22.661206/41.907334 ms`; 1,200/1,200 operations, zero timeouts/cleanup failures; durable/preflight/restart/bounded-listing markers passed; artifact `10677944021` (`sha256:4c1de1cb747a3c0b102a08b12abe38a6dad71a9f354c00c6c4218366db165856`) | Re-run on exact `e0180c75`, preserving durable restart, `--locked`, cleanup and provider markers | 0.5–1.5 d hosted review | Hosted FoundationDB image/client, Ozone topology and customer durability |
| W26.3d — TiDB/R2 composition | **FAIL / 100% functional markers, 29% performance qualification** | `292.606794` IOPS, elapsed `4,101.066772 ms`, p95 write/read/delete `778.754505/201.422311/65.283270 ms`; 1,200/1,200 operations, zero timeouts/cleanup failures; TiDB markers passed before the hard benchmark failure; artifact `10678367922` (`sha256:c579b64a63352d3361e3bfc39a5c0b85df9c8d9cdbebc4a8fb2e750a1cd39497f`) | Re-run on exact `e0180c75`; if still below target, isolate remaining transaction/provider latency while preserving fencing and ambiguous-commit semantics | 1–3 d hosted review plus provider work | Hosted TiDB/PD/TiKV, Ozone topology and provider-native latency |
| W26.4 — Node factories, Rust/Node/CLI and HTTP surfaces | **IMPLEMENTED / 100%; packet markers passed** | Composition logs passed Node provider matrix `pass=7 skip=1 fail=0`, Rust/Node CLI, remote HTTP, bounded listing and reopen markers | Reconfirm all advertised surfaces in the next exact-revision aggregate | 0.5–1.5 d review | Hosted native/N-API prerequisites and Ozone runtime |
| W26.5 — explicit immutable-block reconciliation | **IMPLEMENTED / 100% local contract** | Reconciliation is opt-in, prefix-scoped, lease/fence checked and fail-closed by default; local provider/ChunkedFs/SDK/observability gates pass | Obtain hosted/provider retention, alerting and customer operational evidence; no implicit shutdown cleanup | 0.5–1.5 d review | Provider enumeration, retention policy and customer operations |
| W26.6 — bounded remote directory enumeration | **IMPLEMENTED / 100% local contract** | Bounded Rust/HTTP/CLI/N-API/KV paths and overflow behavior are locally tested; Ozone composition emits bounded-listing markers | Reconfirm provider-backed success/overflow markers in the next one-revision packet | 0.5–1.5 d review | Hosted provider surfaces and customer scale/topology |
| W26.7 — credential-free production configuration policy | **IMPLEMENTED / 100% local policy** | Four provider-positive and four security-negative fixtures pass without opening providers or printing secrets; hosted packet contains policy markers | Validate customer certificates, IAM, rotation and runtime configuration readback | 0.5–1.5 d review | Customer secure Ozone runtime and provider-native security |
| W26.8 — strict requested-provider/no-skip qualification | **IMPLEMENTED / 100% local verifier** | `--require-configured` and provider-specific jobs fail closed on missing providers; all four requested rows ran in the diagnostic packet | Preserve strict selection in the fresh exact-SHA run | 0.25–0.75 d review | Hosted credentials, provider startup and CI scheduling |
| W26.9 — fixed profile and hard IOPS artifact verifier | **IMPLEMENTED / 100% local verifier** | Exact 4 KiB/400-iteration/concurrency-64 profile, target >=1,000, lifecycle/cleanup/stat checks and pass-marker ordering remain enforced | Accept only terminal artifacts with every provider pass marker | 0.5–1 d review | Hosted artifact service and provider performance |
| W26.10 — fail-closed artifact retention | **IMPLEMENTED / 100% local CI control** | Expected JSON uploads use `if-no-files-found: error`; retained current artifacts are non-expired and digest-recorded | Recheck retention/digest evidence on the fresh packet | 0.25–0.5 d review | GitHub artifact service and retention policy |
| W26.11 — one-revision evidence aggregation | **IMPLEMENTED / 100% local verifier; current packet failed closed** | Aggregate `106625464560` rejected the missing `OZONE_IOPS_PASS` marker; provider/base artifacts were retained with digests | Run the aggregate against exact `e0180c75` and require every producer marker | 0.5–1.5 d review | Workflow scheduling and artifact availability |
| W26.12 — incomplete metric/lifecycle rejection | **IMPLEMENTED / 100% local verifier** | Verifier requires finite metrics, exact sample maps, 100% lifecycle success and zero timeout/cleanup failures; all diagnostic rows met integrity but failed hard target where noted | Preserve the integrity checks; do not turn target misses into skips | 0.25–0.75 d review | Hosted artifact correctness and provider behavior |
| W26.13 — credential-free customer rollout contract | **IMPLEMENTED / 100% declaration contract** | Contract encodes Tier-1 99.99%, five-minute RPO/RTO, secure Ozone, tenant scope, all providers, durable topology and customer-owned backup/restore; local positive/negative fixtures pass | Customer must supply measured SLO/RPO/RTO, certificates/IAM/rotation and DR evidence | 1–2 d review | Customer/Ozone ownership; backup and DR are explicitly outside W26 |
| W26.14 — complete end-to-end packet surface enforcement | **IMPLEMENTED / 100% local control; current packet incomplete** | Aggregate requires gateway, provider compositions, Rust/Node/CLI/HTTP, bounded listing, TiDB/FDB restart/N-API and cleanup markers; current aggregate failed closed on performance marker absence | Produce one terminal all-surface packet on exact `e0180c75` | 0.75–1.5 d review | Native runners, provider fixtures and CI orchestration |
| W26.15 — P8 per-drive 1,000-IOPS gate | **OPEN / 95% implementation, 55% hosted qualification** | Latest diagnostic rows: SQLite `1,325.636778` PASS; PGlite `766.631418`, TiDB `292.606794`, FoundationDB `410.703639` FAIL; all lifecycle rows were 1,200/1,200 with zero timeout/cleanup failures | Close all four providers and aggregate on one exact SHA; never lower, average or skip the hard target | 1.5–4 d per remediation cycle plus external queue | Hosted runner/provider capacity, Ozone topology and artifact retention |
| P14 — final integration-readiness review | **NO-GO / 40%** | Current W26 producer jobs are terminal but the aggregate failed; secure customer runtime, 99.99% availability, five-minute RPO/RTO, native, DR and release boundaries remain open | Re-audit a terminal all-provider/end-to-end pass and issue an explicit readiness decision; no deployment or release claim | 1–2 d after W26.15 | Customer Ozone security/SLO/DR and separate release stream |

### Latest diagnostic packet — run `35689474986`

The W26 jobs in this run are terminal, but the parent workflow is still
`in_progress` for unrelated jobs. The run selected `1891c363`, so it does not
test the PGlite implementation at `e0180c75`; it is retained only as a
diagnostic packet. The composition artifact is `10678002197`, TiDB is
`10678367922`, FoundationDB is `10677944021`, and base Ozone is `10678332701`.
The exact aggregate failure was:

`W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000`

That fail-closed result is correct. The next hosted dispatch must select the
exact pushed revision `e0180c75a190340f2c0a45265803de9df5605d59`; queued,
in-progress, canceled, failed or partial jobs remain non-acceptance evidence.
CI is the only available qualification environment. Customers deploy and
operate Ozone, including its secure topology, capacity, availability,
backup/DR and RPO/RTO controls; W26 owns the client/provider correctness and
qualification packet, while another stream owns releases.

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
| 2026-09-22 — PGlite autocommit publication fast path | Moved successful PGlite fenced metadata publication from an explicit transaction to one parameterized PostgreSQL autocommit conditional UPDATE; retained the explicit locked classification transaction only for zero-row outcomes and kept fail-closed semantics. | ~1.5–2.5 h implementation/design; estimate remains provisional | Focused PGlite compile/tests, full locked workspace tests, strict workspace Clippy, formatting and diff checks all passed; five server-dependent focused tests remained ignored because no local isolated PGlite server was available. | Commit `e0180c75a190340f2c0a45265803de9df5605d59` is verified on `origin/main`; security scan `6906585f-77c3-41fa-afe0-06ab4df9e2c6` sealed with complete coverage and zero reportable findings. This is local implementation/security evidence only; the next hosted packet must select this exact revision. | Hosted PGlite/Ozone latency, provider startup and CI artifact retention remain external; no target reduction or semantic weakening is authorized. |
| 2026-09-22 — Terminal review of pre-PGlite hosted packet | Rechecked run `35689474986`, its exact tested SHA, terminal W26 producer jobs, retained artifact IDs/digests and the fail-closed aggregate log. | ~0.75–1.25 h hosted evidence review; estimate remains provisional | W26 jobs `106623193674` (base), `106623193668` (compositions), `106623193474` (TiDB), `106623193564` (FoundationDB) and aggregate `106625464560` are terminal; the parent workflow remains in progress only for unrelated jobs. | The packet selected `1891c363` before `e0180c75`: SQLite/R2 `1,325.636778` IOPS passed, PGlite/R2 `766.631418`, TiDB/R2 `292.606794` and FoundationDB/R2 `410.703639` failed; all four rows completed 1,200/1,200 operations with zero timeout/cleanup failures. Aggregate failed closed on missing `OZONE_IOPS_PASS`; no acceptance is promoted. | Fresh exact-SHA dispatch, hosted runner/provider startup and Ozone/customer capacity are external gates. |
| 2026-09-22 — W26 current ledger/tracker refresh after PGlite publication | Added the current authority override, every W26 work-item row, exact hosted metrics, artifact identities, production envelope, external ownership boundaries and this session log entry to `docs/w26-progress-ledger.md` and `WORK_TRACKER.md`. | ~0.75–1.25 h documentation; estimate remains provisional | `git diff --check` and the documentation review remain required before the separate tracker/ledger commit and push; the next work chunk is a fresh exact-SHA hosted qualification. | Documentation will be committed and pushed separately at the current clean implementation tip so other threads can build on the complete ledger. | No staging environment exists; customer deployment, secure Ozone runtime, 99.99%/RPO/RTO, backup/DR and release execution remain external/non-W26 gates. |
| 2026-09-22 — Fresh exact-SHA W26 qualification dispatch | Dispatched the non-canceling CI workflow after publishing the PGlite implementation and ledger; captured the selected SHA and initial producer state. | ~0.1–0.25 h dispatch/status capture; estimate remains provisional | Run `35691451007` targets `dccd8351690ba21b4ea01ab8680369c76c442041`; compositions `106629129102` was queued, FoundationDB `106629129105`, TiDB `106629129183` and base Ozone `106629129201` were in progress, and the aggregate was not yet created. | No hosted result is promoted while jobs are queued/in progress. Poll to terminal, retrieve artifacts/logs and update the ledger before the next implementation decision. | GitHub-hosted runner queue, Ozone/provider startup, artifact retention and customer-like capacity are external elapsed gates. |
| 2026-09-22 — Terminal exact-SHA W26 packet review | Retrieved the terminal composition, TiDB and FoundationDB artifacts, verified all four JSON digests, inspected provider logs/markers and retrieved the aggregate fail-closed log for run `35691451007`. | ~1–1.5 h hosted evidence review; estimate remains provisional | Base passed; SQLite/R2 `213.947686`, PGlite/R2 `2,065.669446`, TiDB/R2 `333.356025`, FoundationDB/R2 `363.254472` IOPS; every row completed 1,200/1,200 with zero timeout/cleanup failures. Aggregate `106631474430` failed closed on missing `OZONE_IOPS_PASS`; no acceptance is promoted. | The terminal packet is fully recorded; next code decision is a narrow TiDB autocommit CAS chunk plus hosted SQLite variance diagnosis, followed by a fresh exact-SHA packet. | Hosted runner/Ozone variability, TiDB/PD/TiKV and FoundationDB runtime latency, customer-like capacity and artifact service remain external gates. |
| 2026-09-22 — Terminal packet ledger/tracker refresh | Added the terminal run metrics, p95s, exact job/artifact identities and digests, marker boundary, production NO-GO decision and next-action split to `docs/w26-progress-ledger.md` and `WORK_TRACKER.md`. | ~0.75–1.25 h documentation; estimate remains provisional | Run `35691451007` remains diagnostic; W26.15 is open despite the PGlite pass. `git diff --check` and the separate docs commit/push remain required. | Push this documentation chunk before starting the TiDB implementation chunk so downstream threads have the correct build-on and evidence boundary. | CI-only qualification; customer Ozone secure topology, 99.99% SLO, five-minute RPO/RTO, backup/DR and release execution remain outside W26. |

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
