# W04 PGlite progress ledger

This ledger tracks the W04 workstream in `WORK_TRACKER.md`. It separates
implementation, deterministic/local qualification, hosted/native/provider
acceptance, and production-rollout readiness. Percentages and time estimates
are provisional: a queued or in-progress job is not a pass, a demo is not a
production acceptance, and local evidence does not substitute for the hosted
macOS/Linux gate required by W04.2.

## Snapshot

| Field | Current value |
| --- | --- |
| Snapshot base revision | `577fe6d` (`origin/main` before this hosted-acceptance closure update; W04 code fix remains `bdfcb11`) |
| Ledger publication | This current ledger revision is published on `origin/main`; the exact commit is recorded in Git history |
| Current-head local evidence revision | `bdfcb114033f77c3a454ecf84db7bffc52cef9d1` (the published EPIPE fixture-shutdown fix) |
| Latest synced verification revision | `113724487e9efc172ab69254d995377cfcfab296` (workspace test and scoped Clippy evidence; unrelated W26/TiDB/CLI changes are included) |
| Latest full W04 gate revision | `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` (current `origin/main` after the chunked-shutdown fix; exact pinned oracle; full `scripts/test-pglite.sh` exited 0) |
| Latest published repository revision | `577fe6d` (latest remote tip before this hosted-acceptance closure update; W04 code fix remains `bdfcb11`) |
| Snapshot time | 2026-09-21 20:55 AEST / 2026-09-21 10:55 UTC |
| Tracker section | `WORK_TRACKER.md` § W04 — PGlite |
| Checklist completion | **100%**: 8 of 8 W04 checklist items are checked; W04.2 hosted acceptance is closed |
| Implementation/local qualification | **Complete for the recorded packet**; the fresh post-fix oracle-enabled W04 gate passed at `6d59d20`, and synced workspace tests plus scoped W04 Clippy passed at `1137244` |
| Hosted/native/provider acceptance | **W04.2 hosted acceptance complete**: isolated run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864) passed the exact PGlite recovery step on Linux, macOS-latest, and macOS-15-intel; provider credentials/services remain explicit gates |
| Production rollout decision | **NO-GO**: the demo was successful, but production readiness still requires fresh hosted evidence plus artifact, durability, operational, and rollback gates |
| Current external blocker | W04.2 is no longer blocked by hosted Node execution. The dependent `aggregate-native` job [106305282002](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106305282002) was skipped, so production package/artifact acceptance remains open alongside deployment-owner, provider, rollback, and operational gates. |

The 100% figure is a checklist ratio, not a production-readiness claim. W04.2
hosted acceptance is closed; the remaining production matrix determines
whether the workstream is ready for rollout.

## Production rollout readiness (W04 scope)

The demo is complete and received positive feedback. That validates the
direction and user experience, but it does not establish a production release
decision. The production matrix below keeps implementation work separate from
hosted/native/provider gates and records the current decision as **NO-GO** until
every required gate has evidence on the published revision.

| Production gate | Status | Completion | Evidence | Remaining action | Provisional estimate | Classification |
| --- | --- | ---: | --- | --- | ---: | --- |
| W04 implementation packet and deterministic local qualification | Complete | 100% | The current packet includes the socket server, cleanup/fencing fixes, version metadata, mount-free VFS, native hosting tests, and the published fixture-shutdown EPIPE fix `bdfcb11`; the oracle-enabled local gate at `6d59d20` exited 0 with upstream `1,200 passed / 82 skipped` and trace `40/40`. | None unless a fresh hosted job exposes a code regression. | 0h | Engineering / local evidence |
| Hosted Linux Node acceptance | Complete | 100% | Isolated run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864), job [106298858958](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298858958), completed successfully; direct logs and job metadata show the exact PGlite recovery and fragmented early-rejection steps passed, with upstream/trace evidence and explicit provider skips. | None for the hosted Linux W04.2 gate. | 0h | Hosted gate |
| Hosted macOS-latest Node acceptance | Complete | 100% | Isolated run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864), job [106298859119](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298859119), completed successfully; direct logs and job metadata show the exact PGlite recovery and fragmented early-rejection steps passed. | None for the hosted macOS-latest W04.2 gate. | 0h | Hosted/native gate |
| Hosted macOS-15-intel Node acceptance | Complete | 100% | Isolated run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864), job [106298859135](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298859135), completed successfully; direct logs and job metadata show the exact PGlite recovery and fragmented early-rejection steps passed, with no recurrence of the historical EPIPE. | None for the hosted macOS-15-intel W04.2 gate. | 0h | Hosted/native gate |
| Native package/artifact and consumer validation | Blocked / not accepted | 0% | `ci.yml` builds the N-API addon on each Node platform and uploads `native-*` artifacts, but dependent `aggregate-native` job [106305282002](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106305282002) was skipped. | Obtain a completed aggregate-native result, inspect each supported artifact, and run/record a clean consumer install smoke check before production approval. | 0.5–1h after an uncancelled aggregate run | Hosted/package gate |
| Production-like persistence, restart, and version recovery | Hosted W04.2 complete; deployment policy pending | 80% | The isolated Linux, macOS-latest, and macOS-15-intel jobs all passed the PGlite integration/restart-recovery step; local evidence also covers disk restart and reconnect/version history. | Confirm the production data directory/configuration, version compatibility, backup/restore expectation, and rollback behavior for the actual deployment shape. | 1–2h | Engineering plus deployment decision |
| Provider and durability matrix | Explicitly bounded; not inferred | 40% for local scope / 0% for any unrun provider | R2 and TiDB/RustFS rows remain explicit credential/service skips; the ledger does not promote those skips to production acceptance. | If production will use those providers, obtain the real credentials/services and run the corresponding restart, cleanup, and recovery gates; otherwise record the launch scope as PGlite-only. | 1–4h plus provider wait | External provider gate |
| Operational readiness: observability, runbook, limits, rollback, and ownership | Not started as a production gate | 0% | No demo or local test is being treated as evidence for alerting, SLOs, capacity limits, incident response, rollback, or on-call ownership. | Record the launch runbook, health/metric/log signals, failure thresholds, rollback procedure, data protection/restore procedure, capacity limits, and named owner. | 1–2h engineering plus review | Production operations gate |
| Release approval | Blocked by the matrix above | 0% | Current decision is **NO-GO**. The positive demo is context, not a release sign-off. | Change to GO only after the fresh hosted logs, artifact/package evidence, production configuration/recovery evidence, provider boundary, and operational checklist are all signed off. | 0.5–1h review | Release decision |

### Production exit criteria

These are the W04 production-rollout conditions; a checked W04 implementation
item alone does not satisfy them.

- [x] Isolated published-revision CI run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864) completed the Linux Node,
  macOS-latest Node, and macOS-15-intel Node jobs successfully, with the exact
  `Verify PGlite integration and restart recovery` step passing in all three
  logs. Queued, skipped, cancelled, partial, or pre-fix evidence does not count.
- [ ] The dependent native artifact aggregation and package-distribution
  checks pass, and a clean consumer install/smoke result is recorded for the
  supported release outputs.
- [ ] The actual production PGlite configuration has an explicit persistence,
  version-compatibility, restart-recovery, backup/restore, and rollback result;
  local disk tests remain supporting evidence only.
- [ ] Any provider used by the launch has real credential/service evidence for
  its durability, cleanup, restart, and recovery contract. Unused provider
  rows are explicitly out of launch scope rather than silently treated as
  passes.
- [ ] The rollout runbook records observability signals, limits, failure
  thresholds, rollback steps, data-protection steps, and operational ownership.
- [ ] The release owner records a GO/NO-GO decision after reviewing the full
  matrix. W04.2 may close after its hosted acceptance evidence passes, but W04
  is not production-ready until the remaining production gates are also closed.

## Work-item ledger

| Work item | Status | Completion | Evidence | Remaining actions | Provisional engineering estimate |
| --- | --- | ---: | --- | --- | --- |
| Land real socket-server integration and provider/factory coverage | Complete — implementation | 100% | W04 tracker checkbox; current tree contains the PGlite socket server, provider, factory, and integration test surfaces under `integrations/mount-rs-pglite/` and `tests/pglite/`. | None for this item. Keep regressions covered by the W04 gate. | 0h remaining |
| Fix transaction cleanup before releasing a connection slot | Complete — implementation and regression | 100% | Tracker records `29ffb3b`; deterministic slot-release suite passed during the recorded W04 local gate. | None for this item. | 0h remaining |
| Add injected cleanup-failure regression and fail closed | Complete — implementation and regression | 100% | Tracker records `cfce82a`; `server_cleanup_failure.mjs` reported `pglite cleanup failure fail-closed: ok`; the expected injected error was retained as diagnostic evidence. | None for this item. | 0h remaining |
| W04.1 full `scripts/test-pglite.sh` local gate | Complete — local qualification | 100% | The latest post-fix oracle-enabled run at `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` exited 0 and passed slot cleanup, injected failure, provider parity, reconnect, fencing, cancellation, disk restart, mixed stores, Node factories, chunked mounts, and userspace FUSE. R2 and TiDB/RustFS rows retained explicit skips. The synced workspace test gate also exited 0 at `1137244`; scoped PGlite/N-API Clippy passed with `-D warnings`. | No further local W04.1 action unless W04.2 exposes a regression. | 0h remaining |
| Fresh current-tree rerun, SDK/CLI users, upstream, and trace lanes | Complete — local qualification | 100% | The latest post-fix current-tree run with the exact pinned oracle `pithings/mountx@85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` exited 0: Rust SDK `pass=6 skip=3 fail=0`, Node SDK `pass=5 skip=3 fail=0`, CLI `pass=12 skip=2 fail=0`, upstream `1,200 passed / 82 skipped`, and trace `40/40` across five seeds and eight backends, including PGlite and chunked-PGlite. R2/TiDB/RustFS credentials/services remained explicit skips; hosted/provider claims are not inferred from this local result. | None for the current local result; hosted W04.2 remains separate. | 0h remaining |
| Teardown-race fix and bounded close/reopen safety | Complete — implementation and local regression | 100% | Tracker records the PostgreSQL Terminate-frame cleanup path, I/O-turn barrier, listener restoration, and tracked cleanup barrier. Readiness passed 10/10; bounded close/reopen passed 5/5 in the recorded gate; focused current-tree runs also passed the bounded regression repeatedly. | None unless hosted macOS reproduces the historical `Eio` failure. | 0h remaining; 2–6h contingency if hosted failure reproduces |
| W04.2 hosted macOS/Linux reconnect acceptance | **Complete — isolated external hosted gate** | **100%** | Isolated run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864) on `andymac4182/c/w04-production-gate` completed Linux Node job [106298858958](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298858958), macOS-latest Node job [106298859119](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298859119), and macOS-15-intel Node job [106298859135](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298859135) successfully. Direct logs and job metadata confirm the exact `Verify PGlite integration and restart recovery` step passed on all three platforms; fragmented early rejection also passed and the historical Intel EPIPE did not recur. | None for W04.2. Keep the production rollout matrix open: `aggregate-native` was skipped and production configuration, provider scope, rollback, observability, and ownership still need evidence. | 0h remaining; production gates remain 3–6h plus external/provider wait | Hosted acceptance |
| W04.3 versioning, mount-free VFS, and native SQLite-hosting tests | Complete — implementation and local qualification | 100% | Tracker records the rebased packet published through `90c33949`; durable PGlite version metadata, reconnect/version history, mount-free SQLite VFS, native SQLite-hosting tests, and the configuration-driven gate are present. Focused versioning/VFS, locked compilation, formatting, Clippy, and script checks passed in the recorded evidence. | None for W04.3; hosted reconnect remains W04.2's separate gate. | 0h remaining |

## Evidence boundaries

### Implementation and local evidence

- The FUSE compatibility follow-up was published as rebased remote commit
  `f0fb66b` (`fix(fuse): ignore malformed no-reply forget frames`). It was
  required to let the hosted Linux Node preflight reach the PGlite step; the
  focused `mount-rs-fuse` session tests and strict scoped Clippy passed.
- The standalone provider-matrix lockfile was refreshed and published as
  rebased remote commit `d0202fb` (`build(provider-matrix): refresh standalone
  lockfile`). The full local `scripts/test-pglite.sh` run then exited 0.
- The local run retained explicit credential-gated skips: R2 rows were skipped
  without R2 credentials, and TiDB/RustFS rows were skipped without the actual
  services. These are not converted to passes by the surrounding green local
  stages.
- The fresh current-head local run was executed at `5424080` immediately before
  the docs-only `fac9c7e` W08 ledger sync. It is therefore valid local
  qualification for the W04 code, but it is not hosted macOS evidence.
- The first attempt at that rerun used a stale ignored `.node` addon and
  stopped at the chunked default-identity assertion. Rebuilding the current
  addon with the shared Cargo target made the focused chunked test and the
  complete local gate pass; no source change was needed for that environment
  mismatch.
- At synced revision `1137244`,
  `./scripts/cargo-shared test --workspace --all-targets --locked` exited 0.
  The W04-scoped `clippy -p mount-rs-pglite -p mount-rs-napi --all-targets
  --locked -- -D warnings` gate also passed. A full workspace Clippy run was
  not promoted to a W04 failure: it stopped on the unrelated newly merged
  TiDB test `integrations/mount-rs-tidb/tests/ambiguous_commit.rs:211` with
  `clippy::single_match`; that unrelated change was preserved.
- At the published revision `9a86c00`, a second complete
  `./scripts/test-pglite.sh` run exited 0. Its explicit skips were unchanged:
  R2 credentials, TiDB/RustFS services, and the absent `MOUNTX_SOURCE` lanes.
- At published revision `1187a90`, the complete gate was rerun with
  `MOUNTX_SOURCE` set to the exact CI oracle checkout
  `pithings/mountx@85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`. Upstream reported
  `1,200 passed / 82 skipped`, and trace parity reported `40/40` passes across
  five seeds and eight backends; this is local oracle qualification, not
  hosted macOS evidence.
- After the unrelated `c71c8ee` chunked-shutdown fix landed, the N-API addon
  was rebuilt against the rebased tree and the tracked generated declaration
  file was restored without source changes. The post-fix gate at `6d59d20`
  reproduced the same green upstream and trace results, so the local evidence
  covers the current chunked shutdown implementation as well.

### Hosted/native/provider boundary

- Isolated run `35588994864` is the hosted W04.2 evidence source. Linux,
  macOS-latest, and macOS-15-intel Node jobs all completed successfully, and
  their exact PGlite recovery steps passed. Their logs also retain the
  benchmark/trace results and explicit provider skips.
- In the historical run `35560240894`, macOS-latest eventually passed the
  PGlite step while macOS-15-intel failed the earlier HTTP early-rejection
  harness with `EPIPE`; the required PGlite step was therefore skipped on
  Intel. The EPIPE listener fix is published as `bdfcb11`; the isolated run
  demonstrates that this failure no longer recurs on Intel.
- The active heartbeat monitor inspected each isolated job's completed log and
  exact PGlite step before this closure. The dependent `aggregate-native` job
  was skipped, so artifact/package production acceptance remains separate.
- As a diagnostic only, newer run `35578377706` started its Ubuntu Node jobs
  while its `macos-latest` and `macos-15-intel` Node jobs remained queued. This
  corroborated the historical macOS runner-capacity/platform queue; it is not
  substituted for the isolated run and does not change the completed W04.2
  evidence.
- The same CI run contains unrelated non-W04 failures in other jobs. They must
  remain visible in CI review but do not change the W04-specific conclusion.

## Remaining-action checklist

1. [x] Monitor isolated run `35588994864`, jobs `106298858958`,
   `106298859119`, and `106298859135`; all three completed successfully.
2. [x] Retrieve every completed Node log and confirm the exact
   `Verify PGlite integration and restart recovery` step passed on Linux,
   macOS-latest, and macOS-15-intel; record benchmark/trace/provider skips.
3. [x] Change W04.2 to `[x]` only after all three jobs passed; the historical
   Intel EPIPE and all queued/cancelled runs were excluded.
4. Require the dependent native artifact aggregation/package checks and a
   clean supported-consumer smoke result before changing the production
   decision to GO.
5. Confirm the actual launch configuration's persistence, versioning,
   backup/restore, rollback, observability, limits, and ownership gates; keep
   R2/TiDB/RustFS skips explicit unless those providers are in launch scope.
6. Publish the W04.2 closure with the exact run/job links, run
   `git diff --check` and the relevant formatting check, commit the
   documentation closure, and push it to `origin/main`.
7. If a hosted job fails, treat it as a new engineering chunk: capture the
   failure, patch the smallest evidence-backed root cause, run focused tests
   plus the relevant W04 gate, and commit/push before proceeding.

## Provisional remaining effort and blockers

| Category | Estimate | Classification |
| --- | ---: | --- |
| Hosted macOS log inspection after runners start | Complete, ~0.5h actual | Engineering/verification |
| Tracker/dashboard closure, formatting, commit, and push | 0.5–1h | Engineering/documentation |
| Potential remediation if hosted macOS exposes a regression | 2–6h | Engineering contingency |
| GitHub Actions queue delay | Resolved for W04.2 by isolated qualification branch; future shared-main churn remains external | External blocker; not engineering time |
| R2/TiDB local credential/service skips | Unknown | External provider prerequisites; not W04.2's macOS/Linux closure criterion |
| Native artifact aggregation and clean consumer smoke | 0.5–1h | Hosted/package gate; depends on completed CI artifacts |
| Production persistence/backup/rollback and version policy | 1–2h | Engineering plus deployment-owner decision |
| Runbook, observability, limits, and operational ownership | 1–2h | Production operations gate; review/ownership dependent |

Best-case remaining active engineering for W04.2 is approximately
**0.75–1.5h after runner completion**. Production rollout readiness adds
approximately **3–6h** for artifact/consumer, persistence/rollback, and
operational gates, excluding provider setup and hosted runner time. A hosted
regression would add approximately **2–6h** to the W04 engineering estimate.

## Session time log

This is a provisional work log, not a billing record. Historical entries are
rounded from the agent session/tool record and repository evidence; the goal
runtime reported approximately 6,470 seconds (about 1h 48m) before the ledger
snapshot, while the broader work includes time spent waiting on hosted CI.

| Session phase | Work performed | Result / evidence | Time classification |
| --- | --- | --- | --- |
| 2026-09-21 — W04 audit and scope extraction | Read `WORK_TRACKER.md`, identified W04.2 as the only unchecked W04 item, and separated implementation/local/provider/hosted claims. | W04 checklist and closure rule captured above. | Engineering, ~0.5h |
| 2026-09-21 — teardown and FUSE compatibility packet | Inspected the PGlite cleanup path and FUSE session handling; added malformed no-reply FORGET compatibility and regression coverage. | Focused Rust/Node tests, formatting, and strict scoped Clippy passed; published as `f0fb66b` after rebase/push. | Engineering, ~1h |
| 2026-09-21 — dependency/build and local qualification | Installed pinned PGlite/N-API prerequisites, built the N-API addon with the shared Cargo target, restored generated declarations, and ran the full PGlite gate. | Full local `scripts/test-pglite.sh` exited 0; explicit R2/TiDB skips retained. | Engineering/verification, ~1.5h |
| 2026-09-21 — provider-matrix lockfile packet | Diagnosed `--locked` metadata failure, regenerated the standalone provider-matrix lockfile offline, reran the full gate, and published the lockfile update. | Published as `d0202fb`; local gate exited 0. | Engineering, ~0.75h |
| 2026-09-21 — hosted Linux evidence | Inspected run `35560240894`, confirmed Linux Node completion, and retrieved direct job logs. | Job `106211636722` success; PGlite/SDK/CLI/trace evidence recorded above. | Hosted verification, ~0.5h |
| 2026-09-21 — hosted macOS monitoring | Repeatedly polled jobs `106211636737` and `106211636695`; created the quiet heartbeat monitor after the queue persisted. | Both jobs still queued; no macOS log exists yet. | External wait/monitoring, ongoing; excluded from engineering estimate |
| 2026-09-21 17:41–17:44 AEST | Synced the clean checkout and created the ledger. | Ledger published under `docs/w04-progress-ledger.md` through `5424080`; W04.2 remained explicitly open. | Engineering/documentation, ~0.25h |
| 2026-09-21 17:45–17:54 AEST | Rebuilt the current N-API addon after the first rerun exposed a stale ignored binary, ran the focused chunked test, and reran `scripts/test-pglite.sh`. | Focused chunked integration passed; the full gate exited 0 with Rust SDK `6/3/0`, Node SDK `5/3/0`, and CLI `12/2/0` pass/skip/fail summaries. | Engineering/verification, ~0.25h |
| 2026-09-21 18:00–18:14 AEST | Rebasing onto the latest `origin/main`, running the full locked workspace tests, and running both full and W04-scoped Clippy gates. | Workspace tests exited 0; W04 PGlite/N-API Clippy passed with warnings denied. Full workspace Clippy exposed one unrelated TiDB `single_match` warning, which was left untouched. | Engineering/verification, ~0.25h |
| 2026-09-21 18:15–18:18 AEST | Reran `scripts/test-pglite.sh` on the exact published `origin/main` tree. | Full W04 gate exited 0; Rust SDK `6/3/0`, Node SDK `5/3/0`, and CLI `12/2/0` pass/skip/fail summaries remained green, with provider prerequisites explicitly skipped. | Engineering/verification, ~0.25h |
| 2026-09-21 18:19–18:24 AEST | Fetched the exact pinned mountx oracle, installed the pinned oracle/upstream dependencies, and reran the full gate with `MOUNTX_SOURCE` enabled. | Upstream `1,200 passed / 82 skipped`; trace `40/40` across five seeds and eight backends; all W04 lifecycle/provider-matrix stages remained green with explicit provider skips. | Engineering/verification, ~0.25h |
| 2026-09-21 18:25–18:30 AEST | Rebased onto the unrelated `c71c8ee` chunked-shutdown fix, rebuilt the N-API addon with the shared target, restored generated declarations, and reran the oracle-enabled gate. | Post-fix gate at `6d59d20` exited 0 with upstream `1,200 passed / 82 skipped` and trace `40/40`; no tracked build artifact changes remained. | Engineering/verification, ~0.25h |
| 2026-09-21 18:31–18:33 AEST | Compared the required run with the newest CI run after the next push. | Target macOS jobs remained queued; diagnostic run `35578377706` showed Ubuntu Node jobs in progress while both macOS Node jobs were queued. | Hosted verification, ~0.1h; external queue remains non-engineering time |
| 2026-09-21 18:34–19:05 AEST | Investigated the persistent Actions queue, preserved the required W04 run, cancelled only re-verified obsolete queued runs, and added concurrency cancellation to the fault-injection workflow. | Queue pressure was confirmed as an external runner/backlog issue; the recovery workflow change was published as `82e38b1`. | Engineering/release operations, ~0.5h; hosted queue time excluded |
| 2026-09-21 19:06–19:24 AEST | Inspected the first macOS execution evidence and reproduced the Intel-only `EPIPE` in `scripts/test-http-early-rejection.mjs` 1 time in 20 runs. | The prior Intel job failed before PGlite; macOS-latest passed its PGlite step. The failure was isolated to repeated expected child-stdin shutdown errors. | Hosted diagnosis plus engineering, ~0.3h |
| 2026-09-21 19:25–19:45 AEST | Installed a persistent stdin error listener for expected shutdown errors, reran the focused test, and ran 20 repetitions with no EPIPE; published the fix as `bdfcb11`. | Focused and repeated local regression passed; formatting and diff checks passed. | Engineering/verification, ~0.35h |
| 2026-09-21 20:06–20:14 AEST | Expanded this ledger from demo/local/hosted tracking to a production rollout matrix with explicit exit criteria, package/artifact gates, persistence/rollback, provider boundaries, and operational readiness; then retargeted it to the CI run created by the publication push. | Latest run `35587575994` is in progress on Linux, macOS-latest, and macOS-15-intel; release decision remains NO-GO until the matrix closes. | Engineering/documentation, ~0.35h; hosted execution remains external |
| 2026-09-21 20:20–20:55 AEST | Isolated the hosted gate from shared-main churn, inspected the completed Linux, macOS-latest, and macOS-15-intel logs, and verified the exact PGlite recovery step plus early-rejection step on each platform. | Run `35588994864` passed all three Node jobs; W04.2 is closed. The dependent `aggregate-native` job was skipped, so production artifact/package acceptance remains open and the rollout decision stays NO-GO. | Hosted verification/documentation, ~0.75h; remaining production gates are separate |

## Publication note

This ledger revision records the hosted W04.2 closure. It is reviewed with
`git diff --check` and the relevant formatting check, then committed and
pushed to `origin/main`. It closes W04.2 only; it does **not** approve
production, because the artifact/package, deployment, provider, rollback, and
operational evidence described above is still required.
