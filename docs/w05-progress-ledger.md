# W05 Cloudflare R2 progress ledger

Last updated: 2026-09-21 20:00 AEST (2026-09-21 10:00 UTC)

This is the working ledger for the W05 Cloudflare R2 workstream. Percentages
and time estimates are provisional. They separate implementation work from
provider, hosted-CI, and native-platform gates; a local pass does not close a
hosted or native gate.

## Overall position

**W05 functional completion: 100%; production-readiness completion: 35%
provisional.** The scoped CI credentials, fail-closed cost admission guard,
local live-provider coverage, Rust/Node SDK and CLI matrices, bounded
benchmark packet, and hosted Cloudflare R2 acceptance all pass. The
authoritative W05 run is `35579757447` at commit `3db491e`; its budget gate
accepted run `12/20` with an estimated maximum monthly envelope of `$80.00`,
and its live job passed the full packet and uploaded the benchmark artifact.

The current release decision is **NO-GO**. Production readiness requires the
dependency and release gates in the production-readiness register below, not
just the W05 provider pass. At this update, `origin/main` is `81ccc86`, which
includes the cross-platform release-gate repair `bb27fe0`; its current CI run
`35586215815` is pending, fault-injection run `35586215933` is queued, and
Live Cloudflare R2 run `35586216021` is pending. The preceding hosted run
`35585066458` at `9c098e5` exposed Windows-only Rust Clippy and Node probe
failures; the follow-up `bb27fe0` run was cancelled by a superseding push
before jobs executed, so neither is release evidence. No credential value is
stored in the repository or in this document. Native/platform gates remain
explicitly bounded: macOS native NFS qualification is local evidence; Linux
FUSE, Windows, and signed/activated FSKit acceptance are separate platform
workstreams.

## Work-item ledger

| Work item | Work type | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / boundaries |
| --- | --- | --- | ---: | --- | --- | --- | --- |
| W05.0 Land the R2 object-store driver and configurable endpoint support | Implementation | Complete | 100% | `integrations/mount-rs-r2`, endpoint validation, signed HTTP adapter tests, focused Rust tests and Clippy passed. | None for the implementation item. | 0 h | Full acceptance is tracked separately below. |
| W05.1 Unblock D01 and authenticate against actual Cloudflare R2 | Provider / CI configuration | Complete; rotation is scheduled maintenance | 100% | Bucket-scoped Object Read & Write credentials are stored as encrypted GitHub `r2-ci` environment secrets: `MOUNT_RS_R2_ENDPOINT`, `MOUNT_RS_R2_BUCKET`, `MOUNT_RS_R2_ACCESS_KEY_ID`, and `MOUNT_RS_R2_SECRET_ACCESS_KEY`. The active CI token is one-week TTL and limited to `mount-rs-integration-tests`. Final hosted budget and live jobs consumed the secrets successfully without repository plaintext. | Rotate the four secrets before 2026-09-28 and rerun the bounded lane after rotation if the workstream remains active. | 0.5–1 h provisional maintenance | The macOS Keychain item was not readable through the requested security authorization path; CI was provisioned through the authenticated Cloudflare UI and GitHub encrypted secrets instead. |
| W05.2 Verify immutable writes, ranges, retries, reconnect, cleanup, and concurrent publication with independent metadata providers | Implementation + provider gate | Complete locally and hosted | 100% | Local live R2 passed filesystem/CAS, SQLite-metadata/R2-block, PGlite-metadata/R2-block, fresh-client/reopen, concurrent publication, range, retry, prefix isolation, and exact cleanup cases. Hosted run `35579757447` passed the Rust backend gate, the PGlite/R2 block matrix, and the live bounded R2 trace; all cleanup-owned prefixes were verified by the packet. | None for the requested W05 acceptance. | 0–0.5 h provisional follow-up | Cloudflare HTTP latency is variable; hosted evidence remains distinct from local signed-S3-compatible evidence. |
| W05.3 Run Node SDK, Rust/Node CLI, N-API/native, parity, and benchmark lanes on live R2 | Mixed implementation + hosted/native/provider gates | Complete for requested hosted acceptance; native/platform boundaries remain separate | 100% | Local evidence: Rust SDK `pass=9 skip=0 fail=0`; Node SDK `pass=7 skip=1 fail=0`; CLI `pass=14 skip=1 fail=0`; upstream `4` files, `1200 passed`, `82 skipped`; five-seed trace parity passed; live R2 benchmark and full public-NAPI benchmark passed. Final hosted run `35579757447` also passed the Rust/Node/CLI matrices, bounded live R2 trace, live CLI, N-API, service evidence, and benchmark artifact. | None for the requested W05 acceptance. Rotate the short-lived CI token as scheduled maintenance. | 0 h acceptance work; 0.5–1 h provisional rotation maintenance | macOS native NFS qualification passed locally. Linux FUSE/privileged native acceptance is a separate platform boundary and is not claimed by this W05 ledger. |
| W05.4 Record redacted service identity and revision without credentials | Implementation / evidence hygiene | Complete | 100% | `scripts/r2-service-evidence.sh` reports service, endpoint authority, bucket, revision, and owned-prefix counts without secret values; embedded endpoint credentials are rejected. Local and final hosted service-evidence gates passed. | None. | 0 h | Hosted log review must stay redacted; secret-bearing environment values are masked by GitHub. |
| W05.5 Fix the live Node factory expected-byte assertion and guarantee unique fixture keys with exact cleanup | Implementation + provider gate | Complete locally and hosted | 100% | PGlite/R2 Node factory, DELETE/HEAD cleanup, chunked factories, restart/fencing, userspace FUSE, and full N-API test suite passed locally with the pinned oracle. Final hosted run `35579757447` passed the Node SDK/provider rows, hosted N-API suite, exact cleanup, and artifact upload. | None for the requested W05 acceptance. | 0 h | Hosted runner queue and Cloudflare latency can affect elapsed time, not the implementation result. |
| W05.6 Run the configuration-driven CLI gate against the canonical Cloudflare endpoint | Provider / CLI hosted gate | Complete locally and hosted | 100% | Standalone live CLI gate passed locally for PGlite-metadata/R2-block and SQLite-metadata/R2-block, ranged reads, auth isolation, graceful reopen, presence checks, and owned-prefix cleanup. Final hosted run `35579757447` passed the provider-matrix CLI rows and dedicated live Rust CLI script, including graceful shutdown and owned-prefix cleanup. | None. | 0 h | Requires the encrypted `r2-ci` secrets and a live GitHub runner; no credential is permitted in the CLI fixture or repository. |
| W05.7 Add usage-capped CI credentials, cost guard, and full hosted acceptance | Implementation + hosted/provider gate | Complete; rotation is scheduled maintenance | 100% | `.github/workflows/cloudflare-r2.yml` uses the encrypted `r2-ci` environment and a fail-closed budget job. Final run `35579757447` accepted `12/20` for September with an estimated maximum monthly envelope of `$80.00`, then passed Rust backend, Rust SDK `9/0/0`, Node SDK `7/1/0`, CLI `14/1/0`, upstream `1200 passed/82 skipped`, five-seed local/PGlite trace coverage, bounded live R2 trace `621/621`, live CLI, N-API, service evidence, and benchmark artifact `10630468958`. A Cloudflare account-wide `$80` budget alert is also configured. | Rotate the four `r2-ci` secrets before 2026-09-28 and rerun the bounded lane if the workstream remains active. No acceptance work remains. | 0 h acceptance work; 0.5–1 h provisional rotation maintenance | Cloudflare budget alerts notify but do not hard-pause account usage; the GitHub run-count/per-run envelope is the enforced cap for this CI lane, while the one-week bucket-scoped token limits credential lifetime and blast radius. A strict provider-side hard stop at exactly `$100` remains unavailable as an R2 alert feature. |
| W05.8 Repair cross-platform release gates found by hosted acceptance | Implementation + hosted/native gate | Implementation complete; hosted requalification pending | 70% | Hosted CI `35585066458` at `9c098e5` failed Windows Rust Clippy because the Unix-only `http_remote` diagnostic receiver/helper were dead code on Windows, and failed the Windows N-API test because the real auto probe exposed Rust's `windows` name instead of Node's public `win32`. `bb27fe0` adds platform-scoped dead-code allowances, maps `windows` to `win32`, adds a regression assertion, and passed format, targeted strict Clippy, and all 15 N-API Rust unit tests locally. | Obtain one unsuperseded current-head hosted CI pass covering Windows Rust and Windows Node, then retain the run ID here. | 1–2 h active engineering; hosted queue time separate | Requires a stable hosted revision under the repository's `cancel-in-progress` policy; Windows runner behavior and the remaining native mount/package gates cannot be replaced by macOS local tests. |

## Production-readiness dependency register

This register expands W05 from a closed provider slice into the complete
production path. Percentages and estimates are provisional planning values,
not a weighted release score. Every row must either reach 100% with evidence
or be explicitly removed from the release scope by a recorded decision before
the final audit can issue a GO decision.

| ID / mapped workstreams | Work type | Status | Completion | Evidence now | Remaining actions / ship criterion | Provisional engineering time | External blockers / boundaries |
| --- | --- | --- | ---: | --- | --- | --- | --- |
| PR-00 / W05 | Implementation + provider + hosted CI | Complete W05 slice | 100% | Final live Cloudflare R2 run `35579757447` passed the Rust/Node SDK and CLI packet, live trace, N-API, service evidence, budget guard, and artifact upload. | Keep W05 green while the release revision changes; rotate the four short-lived secrets before 2026-09-28. | 0.5–1 h maintenance | Provider alert is notification-only; the CI envelope is a fail-closed cost control, not a billing hard stop. |
| PR-01 / W01–W04 | Implementation + parity + storage/provider acceptance | Open dependency closure | 55% | Core, metadata/block split, memory/SQLite, and PGlite packets have substantial local evidence; the tracker still leaves W01 parity items, W02 mixed-provider/durability items, W03 migration/concurrency items, and W04 hosted reconnect work open. | Close all applicable parity and storage rows, including seeded cross-engine traces, stale writers/CAS, partial uploads, migrations, concurrent open/reopen, rollback, and hosted macOS/Linux reconnect. Ship criterion: no required W01–W04 row remains open or unscoped. | 8–16 h active engineering, plus hosted wait | Full API scope and provider semantics must be confirmed against `REQUIREMENTS.md`; local passes do not close hosted gates. |
| PR-02 / W06–W08 | Real-service/provider composition | Partial provider acceptance | 45% | RustFS service composition and bounded provider packets pass; W07 production lease/time semantics, Node/CLI/native/hosted acceptance, and W08 durable TiDB topology/restart/capacity work remain open in the tracker. | Run real FoundationDB/TiDB/RustFS compositions with restart, fencing, ambiguous-commit, capacity, Rust/Node/CLI, and hosted evidence. Ship criterion: every advertised production provider has a revision-matched hosted or explicitly supported deployment gate. | 8–20 h active engineering, plus service provisioning | FoundationDB/TiDB/RustFS services, durable topology, credentials, and hosted capacity are external/provider gates. |
| PR-03 / W09–W11 | SDK/API/CLI implementation + package/native artifacts | Partial | 65% | Public Rust SDK, Node/N-API package, Rust CLI, Node CLI, declaration/build packets, and local oracle suites pass; platform/package gaps and provider/remote/hosted gates remain open. | Close remaining exports/factories/declarations, clean-install artifacts, Rust/Node CLI provider matrix, native loading, and transport auto-selection. Ship criterion: a clean checkout can build, install, typecheck, and run the supported SDK/CLI flows for every advertised platform. | 6–14 h | Node native artifacts, package registries, platform toolchains, and supported-version policy are external boundaries. |
| PR-04 / W10, W12, W13, W15, W27 | Native and cross-platform acceptance | Open platform gate | 38% | Local macOS NFS read/write/teardown and mount-free FUSE/NFS/9P/WebDAV/S3 codec/session packets pass. `bb27fe0` closes the two identified Windows release-gate defects in implementation and adds a Windows platform-name regression test; current hosted validation is pending. FSKit is unsigned, Windows native runtime/mount and SQLite-hosting/VFS hosted gates remain open. | Execute revision-matched Linux FUSE/NFS/9P/WebDAV, macOS native/FSKit signing and activation, Windows Rust/Node runtime/mount, SQLite VFS multi-process/recovery, and crash/unmount gates. Ship criterion: each supported platform has a passing native gate, or the platform is explicitly excluded from the release support matrix. | 10–24 h active engineering, plus platform queue | `/dev/fuse`, macOS entitlements/signing, FSKit activation, Windows runners, kernel behavior, and physical host access cannot be replaced by local structural tests. |
| PR-05 / W14, W16, W17 | Versioning, adapters, HTTP server, multi-drive and deployment | Partial | 40% | Versioned-filesystem foundation, just-bash/Mastra local adapters, server/CLI local edge cases, and RustFS remote evidence exist; integration, cloud/TLS/deployment, per-drive authorization/isolation, and Cloudflare acceptance remain open. | Close version IDs/publication/pins/replay/CAS, adapter shared-state behavior, HTTP discovery/routing/ranges/TLS, multi-drive authorization and recovery, and real HTTP-client/provider acceptance. Ship criterion: documented deployment topology and security isolation pass from clean clients. | 6–14 h | TLS/domain/deployment configuration, remote services, multi-client concurrency, and per-drive tenancy need hosted validation. |
| PR-06 / W18–W20 | Performance, compression, dependency budget, CI, packaging and release controls | Local code-quality gate passed; hosted/release gate open | 42% | A bounded R2 benchmark and local benchmark packets pass; isolated workspace tests, formatting, and strict locked workspace Clippy passed before `bb27fe0`, which fixed the Windows-only test dead-code diagnostics found by hosted Clippy. W18 remains partial, W19 is design-only, and W20.1–W20.6 remain unchecked. Current release head `81ccc86` has CI run `35586215815` pending, Live R2 run `35586216021` pending, and fault-injection run `35586215933` queued. | Run the current hosted workflows to completion, then complete benchmark/dependency/license audits, package install/provenance checks, required hosted macOS/Linux/Windows jobs, and the final acceptance audit. Ship criterion: no required workflow is queued, cancelled, skipped, or failed on the release SHA. | 8–18 h active engineering, plus hosted queue | Hosted runners, package registries, signing/provenance, and release automation are external gates; pending/queued runs are not evidence of release readiness. |
| PR-07 / W21, W28, W30 | Reference decisions, deterministic faults, observability and operations | Partial | 35% | Several reference reviews and local collector/failure/benchmark/macOS observability packets exist; remaining reviews, deterministic fault workloads, external collector/Linux/Windows evidence, and operational runbooks remain open. | Finish required reference/license review, inject and classify deterministic failures across providers/transports, validate external telemetry and alerting, and publish rollback/recovery/runbook evidence. Ship criterion: known failure modes are reproducible, observable, bounded, and recoverable. | 6–14 h | External collectors, failure-injection environments, and operational ownership are required for production evidence. |
| PR-08 / W22, W23, W29, W31 | Deferred/future scope decision | Scope decision required | 0% | Distributed cache, physical copy-on-write, lifecycle hooks, and per-drive mounts are marked deferred/future in the tracker; no production inclusion/exclusion decision is recorded in this W05 ledger. | Record whether each item is required for the first production release. Required items become implementation gates; deferred items must have a documented non-blocking rationale and follow-up. Ship criterion: no ambiguous requirement remains. | 1–3 h decision work, then variable implementation time | Product requirements and user approval are external blockers; the goal must not silently declare future scope irrelevant. |
| PR-09 / W24–W26 | Product surface and additional providers | Scope-dependent | 45% | The site/domain is live; private AWS S3 verification exists; Apache Ozone local composition exists; Rust tests, hosted provider results, and durable mixed-provider acceptance remain open. | Decide advertised providers/surfaces, then close AWS/Ozone Rust/Node/CLI/restart/hosted tests or remove them from the production support matrix. Ship criterion: every advertised provider and public surface has a green acceptance packet and support statement. | 4–12 h per included provider | Cloud provider accounts, service deployment, package/domain ownership, and hosted capacity are external gates. |
| PR-10 / W20.6 | Final audit and release decision | Not started | 0% | The tracker currently reports overall status `in progress; not release-ready`; the previous W05 close explicitly did not claim whole-product production readiness. | Audit `REQUIREMENTS.md`, `PORTING_STATUS.md`, API parity, tracker, CI artifacts, support matrix, security/license/dependency records, rollback plan, and every required test result. Issue GO only when all required rows are green on one revision; otherwise record NO-GO and exact blockers. | 2–4 h after dependencies close | Requires all required implementation, hosted, native, provider, packaging, and scope decisions to be complete. |

### Production exit criteria

The goal is complete only when all of the following are true on one release
revision: (1) every required register row is 100% or explicitly excluded by
decision; (2) required CI, fault-injection, provider, native, and packaging
jobs are green rather than queued, cancelled, skipped, or merely locally
passing; (3) Rust and Node SDK/CLI artifacts build and install from a clean
checkout; (4) supported-platform and unsupported-platform behavior is
documented; (5) credentials, provenance, license/dependency records,
rollback/recovery procedures, and operational telemetry are reviewed; and
(6) W20.6 records a written GO decision. W05's successful R2 packet is a
necessary provider gate, not a substitute for these release criteria.

## Cost and credential control

| Control | Current setting / evidence | What it proves | Limitation |
| --- | --- | --- | --- |
| GitHub environment | `r2-ci`, four encrypted secrets, no repository plaintext values | CI receives credentials only at the live job boundary | A maintainer with environment administration can still change the secrets or policy. |
| Token scope | Object Read & Write, only `mount-rs-integration-tests`, one-week TTL | Limits the test credential to the test bucket and a short rotation window | Cloudflare token scope is not a dollar quota. |
| CI admission guard | 20 accepted runs per UTC month, `$4.00` assumed per run, `$80.00` maximum envelope, fail closed before secrets are used; final run marker was `accepted_run=12/20`, `remaining_slots=8` | Prevents this workflow from admitting more than 20 live runs and leaves `$20` headroom to the requested `$100` ceiling; the final accepted run used the warning-free summary path | It is a cost envelope, not a provider billing hard stop; per-run workload changes must stay within the bounded packet. |
| Cloudflare alert | Account-wide R2 alert at `$80` | Provides early notification before the target ceiling | The alert does not automatically suspend R2 usage. |
| Workflow bounds | Live job timeout 120 minutes, unique run/prefix IDs, exact cleanup, benchmark artifact retained | Limits runaway job duration and isolates remote fixtures | Timeout is an execution bound, not a billing guarantee. |

## Evidence boundary matrix

| Surface | Result | Acceptance meaning |
| --- | --- | --- |
| Repository implementation | `81ccc86` pushed to `origin/main` | The current main includes the W05 ledger, CI workflow, budget guard, hosted orchestration, shared Cargo wrapper use, provider-matrix R2 CLI coverage, lease-refresh fix, strict-Clippy TiDB fix, Windows release-gate repair `bb27fe0`, and the concurrent AWS provider path. |
| Isolated current-head Rust gates | Targeted repair gates passed; full `81ccc86` requalification pending | Before the concurrent AWS commit, isolated workspace tests, format, and strict Clippy passed; after `bb27fe0`, targeted strict Clippy for `mount-rs-cli`'s `http_remote` test and all 15 N-API Rust unit tests passed. The current AWS-containing revision still requires its own full locked gate. Live-provider/native opt-ins remain separately classified. |
| Local Rust/provider | Passed with authenticated Cloudflare R2 | Strong provider behavior evidence on the development host; not hosted CI evidence. |
| Local Node SDK/N-API | Passed with pinned `mountx` oracle and live R2/PGlite rows | Public addon and SDK behavior are qualified locally; not hosted runner evidence. |
| Local Rust and Node CLI | Passed with live R2/PGlite configuration and reopen/cleanup | Configuration-driven CLI behavior is qualified locally; not hosted runner evidence. |
| Local native | macOS NFS structural read/write/teardown passed | Native local qualification only; Linux FUSE and other host prerequisites remain separate. |
| Hosted run `35570596593` | Budget passed; live job canceled | Partial hosted evidence only. It must not be called a release or W05 acceptance pass. |
| Hosted diagnostic run `35575940720` | Reached the Rust CLI and reported redacted `ESTALE: stale file handle, metadata lease` during graceful shutdown | Failure evidence that identified the lease-refresh defect; not an acceptance pass. |
| Hosted run `35577687152` | Passed the full hosted packet after `c71c8ee` refreshed an expired unfenced lease during shutdown | Hosted acceptance evidence before the final budget-summary formatting correction. |
| Final hosted run `35579757447` | Passed at `3db491e`; budget `12/20`, full SDK/CLI/N-API/service/trace packet, and artifact `10630468958` | Authoritative W05 hosted acceptance. |
| Release revision `81ccc86` hosted workflows | CI `35586215815` pending; Live R2 `35586216021` pending; fault injection `35586215933` queued | Current-release qualification is not complete until these jobs conclude and any required follow-up is pushed. The prior `9c098e5` run exposed two Windows defects; the fix is present in the current ancestry but is not yet backed by a green unsuperseded hosted run. |

## Remaining action plan

1. No active W05 acceptance action remains: final hosted run
   `35579757447` is successful and the evidence is recorded here and in
   `WORK_TRACKER.md`.
2. Execute PR-01 through PR-07 in dependency order, updating this ledger
   after each implementation or hosted/native/provider chunk and pushing the
   corresponding commit to `origin/main`.
3. Resolve PR-08 scope decisions before the final audit; do not silently treat
   deferred work as either required or irrelevant.
4. Before the CI token expires on 2026-09-28, rotate the four `r2-ci` secrets
   and rerun the bounded acceptance packet if the workstream remains active.
   Do not copy secret values into issues, logs, this ledger, or chat.
5. Keep macOS native NFS evidence distinct from Linux FUSE, Windows, and
   signed/activated FSKit gates; those remain separate platform workstreams.
6. Keep the unrelated accidental read-only all-buckets Cloudflare token
   outside this workstream until its deletion is explicitly authorized.

## Session time log

Times below are provisional wall-clock/activity estimates. Hosted waiting is
shown separately from active engineering time.

| UTC time | Activity | Classification | Result / next state |
| --- | --- | --- | --- |
| 2026-09-21 06:56–07:11 | Workflow `35570596593` queued and budget job admitted run `2/20` | Hosted wait / cost gate | Budget passed; live job scheduled. |
| 2026-09-21 07:15–07:25 | Hosted setup, locked installs, public Node addon build, Rust backend, PGlite SDK/CLI and upstream tests | Hosted gate | All reported passing before the later cancellation. |
| 2026-09-21 07:25–07:31 | Hosted trace parity and cancellation diagnosis | Hosted failure analysis | Five-seed local/PGlite trace passed; redundant remote trace was canceled mid-second R2 seed; no assertion failure was reported. |
| 2026-09-21 07:32–07:35 | Retrieved redacted logs and identified duplicated remote/local trace work | Active engineering | Chosen fix: R2-only bounded trace plus shared Cargo wrapper. |
| 2026-09-21 07:35–07:36 | Patched shell/Node helpers; ran shell syntax, Node syntax, and diff checks | Implementation | Checks passed. |
| 2026-09-21 07:36–07:38 | Committed, fetched, rebased over remote main, and pushed `d4f7281` | Release engineering | New workflow `35573697664` created. |
| 2026-09-21 07:39–07:48 | Created and pushed the initial ledger as `c773294`; reviewed the hosted queue and redacted logs | Documentation / hosted wait | Replacement run was still the open acceptance action. |
| 2026-09-21 08:00–08:23 | Inspected diagnostic run `35575940720`; retained redacted stderr and reproduced the exact hosted `ESTALE` shutdown failure | Hosted failure analysis | Classified the defect as expired-lease shutdown handling, not a credential or provider-auth failure. |
| 2026-09-21 08:23–08:33 | Patched `ChunkedFs::shutdown`, added the expired-unfenced-lease regression test, ran focused Rust tests, rebased, and pushed `c71c8ee` | Implementation / release engineering | Focused test passed; replacement hosted run queued. |
| 2026-09-21 08:32–08:44 | Hosted run `35577687152` executed the full packet | Hosted provider/native gate | Budget, SDK/CLI matrices, trace, live CLI, N-API, service evidence, and artifact all passed. |
| 2026-09-21 08:44–09:01 | Removed shell command-substitution warnings from the budget summary, ran syntax/diff checks, rebased, and pushed `3db491e` | Implementation / release engineering | Final authoritative run queued at the corrected head. |
| 2026-09-21 09:01–09:16 | Final hosted run `35579757447` completed and logs/artifact were reviewed | Hosted provider gate / evidence | Budget marker `12/20`, `$80` envelope, full W05 packet, and artifact `10630468958` passed. |
| 2026-09-21 09:16–09:23 | Answered the production-readiness question from current tracker and live GitHub Actions state | Release status review | W05 is green, but whole-repository status remains NO-GO; current main had queued CI/fault-injection work and open W20/platform gates. |
| 2026-09-21 09:23–09:31 | Created the continuation goal and designed the production-readiness dependency register | Goal setup / planning | PR-00 through PR-10 now enumerate implementation, hosted, native, provider, packaging, scope, and final-audit work. |
| 2026-09-21 09:31–09:35 | Ran isolated full locked workspace tests after clearing shared-target collisions | Local production gate | All workspace targets passed; live-provider/native opt-ins remained explicit skips. |
| 2026-09-21 09:35–09:38 | Ran strict locked workspace Clippy | Local production gate | Found one real `clippy::single_match` failure in TiDB ambiguous-commit test. |
| 2026-09-21 09:38–09:43 | Replaced the single-pattern match, ran format, targeted Clippy/test checks, committed/rebased/pushed `2e66f94` | Implementation / release engineering | Targeted checks passed; fresh CI, Live R2, and fault-injection workflows started. |
| 2026-09-21 09:43–09:51 | Watched hosted CI `35585066458` at `9c098e5`; retrieved redacted Windows diagnostics for Rust Clippy dead code and the Node `windows`/`win32` probe mismatch | Hosted failure analysis | Classified two implementation defects; Linux Rust, Linux native FUSE/NFS/9P/WebDAV, TiDB, Ozone, RustFS and several Node paths passed before the workflow finished. |
| 2026-09-21 09:51–09:55 | Patched the Unix-only HTTP test fields/helpers and normalized the Windows auto-probe platform name; ran format, targeted strict Clippy and 15 N-API unit tests; committed/pushed `bb27fe0` | Implementation / release engineering | Local repair gates passed; hosted run `35585966602` was cancelled before execution by a superseding push. |
| 2026-09-21 09:55–10:00 | Rebased onto concurrent `b7e2758` and `81ccc86` main updates; recorded current CI/live-R2/fault-injection run IDs | Release ledger / hosted wait | Current revision is `81ccc86`; goal remains active and release decision remains NO-GO pending unsuperseded hosted qualification and the remaining production register. |

Estimated active engineering time for the completed W05 continuation before
this production program: **about 2–2.5 h**. The production-readiness register
currently represents **about 60–140 h** of provisional active engineering and
review across the mapped rows, excluding hosted queues, provider provisioning,
signing, and other external wait time. These estimates are planning ranges,
not commitments; they will be revised with evidence after each chunk.
