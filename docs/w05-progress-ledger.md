# W05 Cloudflare R2 progress ledger

Last updated: 2026-09-21 20:37 AEST (2026-09-21 10:37 UTC)

This is the working ledger for the W05 Cloudflare R2 workstream. Percentages
and time estimates are provisional. They separate implementation work from
provider, hosted-CI, and native-platform gates; a local pass does not close a
hosted or native gate.

## Overall position

**W05 functional completion: 100%; production-readiness completion: 38%
provisional.** The scoped CI credentials, fail-closed cost admission guard,
local live-provider coverage, Rust/Node SDK and CLI matrices, bounded
benchmark packet, and hosted Cloudflare R2 acceptance all pass. The
authoritative W05 run is `35579757447` at commit `3db491e`; its budget gate
accepted run `12/20` with an estimated maximum monthly envelope of `$80.00`,
and its live job passed the full packet and uploaded the benchmark artifact.

The current release decision is **NO-GO**. Production readiness requires the
dependency and release gates in the production-readiness register below, not
just the W05 provider pass. The current code head is `f98cc7d`, which includes
the Windows platform repair `bb27fe0`, the release-package declaration fix
`d735814`, and the deterministic HTTP subprocess test isolation fix. On this
head, local locked Rust tests, strict Clippy, the rebuilt N-API package, the
pinned-oracle Node suite, and the PGlite/Rust/Node/CLI matrix pass. Hosted CI
`35589628584` is pending; the fault-injection run `35589628587` is queued; the
live R2 run `35589628621` failed closed at the budget gate with `count=29`
against `limit=20`; and AWS run `35589628578` failed before credentials were
configured because the `aws-s3-ci` security environment has no region/role
inputs. These are not release passes. No credential value is stored in the
repository or in this document. Native/platform gates remain explicitly
bounded: macOS native NFS qualification is local evidence; Linux FUSE,
Windows, and signed/activated FSKit acceptance are separate platform
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
| W05.7 Add usage-capped CI credentials, cost guard, and full hosted acceptance | Implementation + hosted/provider gate | Functional acceptance complete; monthly admission exhausted | 100% | `.github/workflows/cloudflare-r2.yml` uses the encrypted `r2-ci` environment and a fail-closed budget job. Final run `35579757447` accepted `12/20` for September with an estimated maximum monthly envelope of `$80.00`, then passed Rust backend, Rust SDK `9/0/0`, Node SDK `7/1/0`, CLI `14/1/0`, upstream `1200 passed/82 skipped`, five-seed local/PGlite trace coverage, bounded live R2 trace `621/621`, live CLI, N-API, service evidence, and benchmark artifact `10630468958`. Later attempts, including `35589628621`, were refused before secret use at `count=29 limit=20`; the Cloudflare account-wide `$80` alert remains configured. | Rotate the four `r2-ci` secrets before 2026-09-28. Do not request another live-R2 attempt until the UTC month resets and the rotated token is ready; then run exactly one bounded requalification on the release revision. | 0.5–1 h rotation/requalification setup; hosted wait separate | The run-count/per-run envelope is the enforced fail-closed control and is intentionally conservative because it counts workflow attempts. Cloudflare alerts notify but do not hard-pause account usage; a provider-side hard stop at exactly `$100` remains unavailable as an R2 alert feature. |
| W05.8 Repair cross-platform release gates found by hosted acceptance | Implementation + hosted/native gate | Implementation complete; hosted requalification pending | 70% | Hosted CI `35585066458` at `9c098e5` failed Windows Rust Clippy because the Unix-only `http_remote` diagnostic receiver/helper were dead code on Windows, and failed the Windows N-API test because the real auto probe exposed Rust's `windows` name instead of Node's public `win32`. `bb27fe0` adds platform-scoped dead-code allowances, maps `windows` to `win32`, adds a regression assertion, and passed format, targeted strict Clippy, and all 15 N-API Rust unit tests locally. | Obtain one unsuperseded current-head hosted CI pass covering Windows Rust and Windows Node, then retain the run ID here. | 1–2 h active engineering; hosted queue time separate | Requires a stable hosted revision under the repository's `cancel-in-progress` policy; Windows runner behavior and the remaining native mount/package gates cannot be replaced by macOS local tests. |
| W05.9 Requalify the release revision and make Node packaging deterministic | Implementation + local release gate | Local green; hosted/provider release gates open | 85% | On the `f98cc7d` content, `cargo fmt --all -- --check`, locked workspace tests, and strict locked workspace Clippy passed after the concurrent HTTP subprocess-root collision was fixed in `a39eeaf`. A clean locked N-API release build and full pinned-oracle package suite passed on the same code line; `pnpm build` left generated declarations/loaders clean. The real local PGlite matrix passed Rust SDK `6/0/0`, Node SDK `5/0/0`, and CLI `12/0/0`, with only explicit R2/TiDB/RustFS gates skipped. | Retain one green unsuperseded hosted CI run for `f98cc7d` or its ledger successor; rerun the live R2 lane only after the cap reset and token rotation; obtain AWS security-provisioned region/role inputs; close native/package/platform rows. | 1–2 h active engineering completed; 2–6 h hosted/provider follow-up | Hosted concurrency can cancel a run; R2 is blocked by the deliberate monthly cap; AWS OIDC role/region/bucket inputs are absent from `aws-s3-ci`; native mount and FSKit evidence remain host/signing gates. |

## Production-readiness dependency register

This register expands W05 from a closed provider slice into the complete
production path. Percentages and estimates are provisional planning values,
not a weighted release score. Every row must either reach 100% with evidence
or be explicitly removed from the release scope by a recorded decision before
the final audit can issue a GO decision.

| ID / mapped workstreams | Work type | Status | Completion | Evidence now | Remaining actions / ship criterion | Provisional engineering time | External blockers / boundaries |
| --- | --- | --- | ---: | --- | --- | --- | --- |
| PR-00 / W05 | Implementation + provider + hosted CI | Complete W05 slice; requalification held by cap | 100% | Final live Cloudflare R2 run `35579757447` passed the Rust/Node SDK and CLI packet, live trace, N-API, service evidence, budget guard, and artifact upload. Current-head attempt `35589628621` failed closed before the live job at `count=29/20`, so it consumed no R2 secret or provider operation. | Rotate the four short-lived secrets before 2026-09-28; run one bounded current-release requalification after the UTC month reset. | 0.5–1 h maintenance/requalification setup | Provider alert is notification-only; the CI envelope is a fail-closed cost control, not a billing hard stop. |
| PR-01 / W01–W04 | Implementation + parity + storage/provider acceptance | Open dependency closure | 55% | Core, metadata/block split, memory/SQLite, and PGlite packets have substantial local evidence; the tracker still leaves W01 parity items, W02 mixed-provider/durability items, W03 migration/concurrency items, and W04 hosted reconnect work open. | Close all applicable parity and storage rows, including seeded cross-engine traces, stale writers/CAS, partial uploads, migrations, concurrent open/reopen, rollback, and hosted macOS/Linux reconnect. Ship criterion: no required W01–W04 row remains open or unscoped. | 8–16 h active engineering, plus hosted wait | Full API scope and provider semantics must be confirmed against `REQUIREMENTS.md`; local passes do not close hosted gates. |
| PR-02 / W06–W08 | Real-service/provider composition | Partial provider acceptance | 45% | RustFS service composition and bounded provider packets pass; W07 production lease/time semantics, Node/CLI/native/hosted acceptance, and W08 durable TiDB topology/restart/capacity work remain open in the tracker. | Run real FoundationDB/TiDB/RustFS compositions with restart, fencing, ambiguous-commit, capacity, Rust/Node/CLI, and hosted evidence. Ship criterion: every advertised production provider has a revision-matched hosted or explicitly supported deployment gate. | 8–20 h active engineering, plus service provisioning | FoundationDB/TiDB/RustFS services, durable topology, credentials, and hosted capacity are external/provider gates. |
| PR-03 / W09–W11 | SDK/API/CLI implementation + package/native artifacts | Packaging repair complete; hosted/package gates open | 75% | Public Rust SDK, Node/N-API package, Rust CLI, Node CLI, declaration/build packets, local PGlite matrix, and full pinned-oracle package suite pass. `d735814` makes clean release builds preserve the runtime-exported FUSE dirent declarations, and the post-build diff is clean. | Close clean-install artifacts, platform artifact aggregation/publication, Rust/Node CLI provider matrix on every advertised platform, native loading, and transport auto-selection. Ship criterion: a clean checkout can build, install, typecheck, and run the supported SDK/CLI flows for every advertised platform. | 4–12 h | Node native artifacts, package registries, platform toolchains, and supported-version policy are external boundaries. |
| PR-04 / W10, W12, W13, W15, W27 | Native and cross-platform acceptance | Open platform gate | 38% | Local macOS NFS read/write/teardown and mount-free FUSE/NFS/9P/WebDAV/S3 codec/session packets pass. `bb27fe0` closes the two identified Windows release-gate defects in implementation and adds a Windows platform-name regression test; current hosted CI `35589628584` is pending and therefore not evidence. FSKit is unsigned, Windows native runtime/mount and SQLite-hosting/VFS hosted gates remain open. | Execute revision-matched Linux FUSE/NFS/9P/WebDAV, macOS native/FSKit signing and activation, Windows Rust/Node runtime/mount, SQLite VFS multi-process/recovery, and crash/unmount gates. Ship criterion: each supported platform has a passing native gate, or the platform is explicitly excluded from the release support matrix. | 10–24 h active engineering, plus platform queue | `/dev/fuse`, macOS entitlements/signing, FSKit activation, Windows runners, kernel behavior, and physical host access cannot be replaced by local structural tests. |
| PR-05 / W14, W16, W17 | Versioning, adapters, HTTP server, multi-drive and deployment | Partial | 40% | Versioned-filesystem foundation, just-bash/Mastra local adapters, server/CLI local edge cases, and RustFS remote evidence exist; integration, cloud/TLS/deployment, per-drive authorization/isolation, and Cloudflare acceptance remain open. | Close version IDs/publication/pins/replay/CAS, adapter shared-state behavior, HTTP discovery/routing/ranges/TLS, multi-drive authorization and recovery, and real HTTP-client/provider acceptance. Ship criterion: documented deployment topology and security isolation pass from clean clients. | 6–14 h | TLS/domain/deployment configuration, remote services, multi-client concurrency, and per-drive tenancy need hosted validation. |
| PR-06 / W18–W20 | Performance, compression, dependency budget, CI, packaging and release controls | Local code-quality gate passed; hosted/release gate open | 48% | A bounded R2 benchmark and local benchmark packets pass; locked workspace tests, formatting, and strict locked workspace Clippy passed on the `f98cc7d` code content after the HTTP subprocess isolation repair. The N-API release build/package diff is clean. W18 remains partial, W19 is design-only, and W20.1–W20.6 remain unchecked. Current hosted CI `35589628584` is pending, fault injection `35589628587` is queued, R2 `35589628621` is denied at `count=29/20`, and AWS `35589628578` fails on missing security inputs. | Run one unsuperseded current hosted workflow to completion, then complete benchmark/dependency/license audits, package install/provenance checks, required hosted macOS/Linux/Windows jobs, and the final acceptance audit. Ship criterion: no required workflow is queued, cancelled, skipped, or failed on the release SHA. | 8–18 h active engineering, plus hosted/provider queue | Hosted runners, package registries, signing/provenance, AWS security provisioning, the R2 monthly cap, and release automation are external gates; pending/queued/denied runs are not release evidence. |
| PR-07 / W21, W28, W30 | Reference decisions, deterministic faults, observability and operations | Partial | 35% | Several reference reviews and local collector/failure/benchmark/macOS observability packets exist; remaining reviews, deterministic fault workloads, external collector/Linux/Windows evidence, and operational runbooks remain open. | Finish required reference/license review, inject and classify deterministic failures across providers/transports, validate external telemetry and alerting, and publish rollback/recovery/runbook evidence. Ship criterion: known failure modes are reproducible, observable, bounded, and recoverable. | 6–14 h | External collectors, failure-injection environments, and operational ownership are required for production evidence. |
| PR-08 / W22, W23, W29, W31 | Deferred/future scope decision | Scope decision required | 0% | Distributed cache, physical copy-on-write, lifecycle hooks, and per-drive mounts are marked deferred/future in the tracker; no production inclusion/exclusion decision is recorded in this W05 ledger. | Record whether each item is required for the first production release. Required items become implementation gates; deferred items must have a documented non-blocking rationale and follow-up. Ship criterion: no ambiguous requirement remains. | 1–3 h decision work, then variable implementation time | Product requirements and user approval are external blockers; the goal must not silently declare future scope irrelevant. |
| PR-09 / W24–W26 | Product surface and additional providers | Scope-dependent | 50% | The site/domain is live; AWS private test-resource and local Rust SDK/CLI evidence exists; Apache Ozone local/hosted qualification exists within its documented scope; AWS hosted run `35589628578` fails before execution because `aws-s3-ci` has no region/role inputs; production resource/metadata/DR gates remain open. | Decide advertised providers/surfaces, provision AWS security inputs through the approved security path, then close AWS/Ozone Rust/Node/CLI/restart/hosted tests or remove them from the production support matrix. Ship criterion: every advertised provider and public surface has a green acceptance packet and support statement. | 4–12 h per included provider | Cloud provider accounts, service deployment, package/domain ownership, security-provisioned workload identity, and hosted capacity are external gates. |
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
| CI admission guard | 20 workflow admissions per UTC month, `$4.00` assumed per run, `$80.00` maximum envelope, fail closed before secrets are used; final admitted run marker was `accepted_run=12/20`, `remaining_slots=8` | Prevents this workflow from admitting more than 20 live runs and leaves `$20` headroom to the requested `$100` ceiling; the final admitted run used the warning-free summary path | The guard conservatively counts workflow attempts, including attempts denied after the cap is exhausted. It is a cost envelope, not a provider billing hard stop; per-run workload changes must stay within the bounded packet. |
| Cloudflare alert | Account-wide R2 alert at `$80` | Provides early notification before the target ceiling | The alert does not automatically suspend R2 usage. |
| Workflow bounds | Live job timeout 120 minutes, unique run/prefix IDs, exact cleanup, benchmark artifact retained | Limits runaway job duration and isolates remote fixtures | Timeout is an execution bound, not a billing guarantee. |

## Evidence boundary matrix

| Surface | Result | Acceptance meaning |
| --- | --- | --- |
| Repository implementation | `f98cc7d` pushed to `origin/main` | The current main includes the W05 ledger, CI workflow, budget guard, hosted orchestration, shared Cargo wrapper use, provider-matrix R2 CLI coverage, lease-refresh fix, strict-Clippy TiDB fix, Windows release-gate repair `bb27fe0`, AWS provider path, deterministic HTTP subprocess test isolation, and release-package declaration fix `d735814`. |
| Isolated current-head Rust gates | Passed locally on the `f98cc7d` code content | `cargo fmt --all -- --check`, locked workspace tests, and strict locked workspace Clippy exited 0 after `a39eeaf` fixed the concurrent HTTP temporary-root collision. Live-provider/native opt-ins remain separately classified. |
| Local Rust/provider | PGlite/local providers passed; prior authenticated R2 pass retained | The current local PGlite/provider matrix passed; the authoritative authenticated Cloudflare R2 evidence remains `35579757447` at `3db491e`. The current hosted R2 requalification is cap-blocked, so no current-head live-R2 claim is made. |
| Local Node SDK/N-API | Passed with pinned `mountx` oracle and rebuilt release artifact | Full package suite passed after the current N-API release build; the PGlite matrix passed its Node SDK rows, while R2/FDB/native opt-ins remain explicit skips. This is local evidence, not hosted runner evidence. |
| Local Rust and Node CLI | Passed on PGlite/SQLite/memory matrix; live R2 current-head gate blocked | Current PGlite matrix reported Rust SDK `6/0/0`, Node SDK `5/0/0`, CLI `12/0/0`; the earlier live R2 CLI/N-API packet remains authoritative at `35579757447`. |
| Local native | macOS NFS structural read/write/teardown passed | Native local qualification only; Linux FUSE and other host prerequisites remain separate. |
| Hosted run `35570596593` | Budget passed; live job canceled | Partial hosted evidence only. It must not be called a release or W05 acceptance pass. |
| Hosted diagnostic run `35575940720` | Reached the Rust CLI and reported redacted `ESTALE: stale file handle, metadata lease` during graceful shutdown | Failure evidence that identified the lease-refresh defect; not an acceptance pass. |
| Hosted run `35577687152` | Passed the full hosted packet after `c71c8ee` refreshed an expired unfenced lease during shutdown | Hosted acceptance evidence before the final budget-summary formatting correction. |
| Final hosted run `35579757447` | Passed at `3db491e`; budget `12/20`, full SDK/CLI/N-API/service/trace packet, and artifact `10630468958` | Authoritative W05 hosted acceptance. |
| Release revision `f98cc7d` hosted workflows | CI `35589628584` pending; Live R2 `35589628621` failed closed at `count=29/20`; AWS `35589628578` failed before credential configuration; fault injection `35589628587` queued | Current-release qualification is not complete until required jobs conclude green on one unsuperseded revision. The prior `9c098e5` run exposed two Windows defects; the implementation fix is present in the current ancestry but is not yet backed by a green hosted Windows run. |
| Hosted R2 cap boundary `35589628621` | Budget job failed with `count=29 limit=20`; live integration was skipped | This is a successful safety refusal and proves no further current-month R2 job is admitted or receives the encrypted secrets; it is not provider acceptance evidence. |
| Hosted AWS security boundary `35589628578` | `aws-region` input was empty; `aws-s3-ci` has no visible region/role environment inputs | This records an external security-provisioning blocker; it is not an AWS provider failure and must not be called a hosted pass. |

## Remaining action plan

1. The requested W05 acceptance is complete: final hosted run `35579757447`
   is successful and the evidence is recorded here and in `WORK_TRACKER.md`.
   Current-head requalification is a separate open action because the
   fail-closed R2 cap has been exhausted.
2. Execute PR-01 through PR-07 in dependency order, updating this ledger
   after each implementation or hosted/native/provider chunk and pushing the
   corresponding commit to `origin/main`.
3. Resolve PR-08 scope decisions before the final audit; do not silently treat
   deferred work as either required or irrelevant.
4. Before the CI token expires on 2026-09-28, rotate the four `r2-ci` secrets
   through the approved security path. Do not request a live run until the
   UTC-month cap resets; then run one bounded acceptance packet on the final
   release revision. Do not copy secret values into issues, logs, this ledger,
   or chat.
5. Keep macOS native NFS evidence distinct from Linux FUSE, Windows, and
   signed/activated FSKit gates; those remain separate platform workstreams.
6. Keep the unrelated accidental read-only all-buckets Cloudflare token
   outside this workstream until its deletion is explicitly authorized.
7. Provision `MOUNT_RS_AWS_S3_TEST_REGION`, `MOUNT_RS_AWS_S3_TEST_BUCKET`,
   and `MOUNT_RS_AWS_S3_CI_ROLE_ARN` through security/OIDC administration, then
   rerun the bounded AWS hosted gate without placing credentials in source or
   logs.

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
| 2026-09-21 10:00–10:09 | Advanced through concurrent AWS/FoundationDB/W08 changes; ran the current locked workspace gates and diagnosed the first `http_subprocess` `EEXIST` failure as a concurrent temporary-root collision | Local production gate / failure analysis | Clippy passed; the full test gate failed only at test-directory creation, with no product assertion failure. |
| 2026-09-21 10:09–10:20 | Installed pinned Node/PGlite/oracle dependencies, rebuilt the N-API release artifact, and reran the PGlite/Rust/Node/CLI matrix plus the full pinned-oracle Node package suite | Local SDK/CLI/package gate | PGlite matrix passed Rust SDK `6/0/0`, Node SDK `5/0/0`, CLI `12/0/0`; full Node suite passed; R2/TiDB/RustFS/native opt-ins remained explicit skips. |
| 2026-09-21 10:20–10:27 | Fixed repeatable N-API declaration loss in `postbuild.mjs`, synced generated docs, verified a clean release-build diff, committed/pushed `d735814` | Implementation / packaging | Runtime-exported FUSE dirent declarations now survive clean release builds; local package build and tests passed. |
| 2026-09-21 10:27–10:37 | Added the atomic temporary-root sequence, reran focused/full Rust tests and strict Clippy, committed/rebased/pushed `f98cc7d`, and reviewed current hosted failures | Implementation / release engineering / hosted gate | Full Rust workspace and Clippy passed; R2 `35589628621` failed closed at `count=29/20`; AWS `35589628578` failed on missing security inputs; current CI remained pending and fault injection queued. |

Estimated active engineering time for the completed W05 continuation before
this production program: **about 4–4.5 h total active work so far**. The
production-readiness register currently represents **about 60–140 h** of
provisional active engineering and review across the mapped rows, excluding
hosted queues, provider provisioning, signing, and other external wait time.
These estimates are planning ranges, not commitments; they will be revised
with evidence after each chunk.
