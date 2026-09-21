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
| Snapshot base revision | `d619f93` (`origin/main` at this ledger refresh; the published W04 EPIPE fix remains `bdfcb11`) |
| Ledger publication | This current ledger revision is published on `origin/main`; the exact commit is recorded in Git history |
| Current-head local evidence revision | `caa9324` (published provider-matrix lockfile refresh on top of the N-API reconcile-provider teardown and active-multipart oracle fixes; local locked provider-matrix command passed) |
| Latest synced verification revision | `113724487e9efc172ab69254d995377cfcfab296` (workspace test and scoped Clippy evidence; unrelated W26/TiDB/CLI changes are included) |
| Latest full W04 gate revision | `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` (current `origin/main` after the chunked-shutdown fix; exact pinned oracle; full `scripts/test-pglite.sh` exited 0) |
| Latest published repository revision | `d619f93` (latest `origin/main` tip at this ledger refresh; the lockfile fix is included in its history) |
| Qualification evidence revision | `caa9324` on `origin/main`; main CI run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646) is the current published-revision qualification and remains in progress |
| Snapshot time | 2026-09-22 00:05 AEST |
| Tracker section | `WORK_TRACKER.md` § W04 — PGlite |
| Checklist completion | **100%**: 8 of 8 W04 checklist items are checked; W04.2 hosted acceptance is closed |
| Implementation/local qualification | **Complete for the recorded packet**; the fresh post-fix oracle-enabled W04 gate passed at `6d59d20`, and synced workspace tests plus scoped W04 Clippy passed at `1137244` |
| Hosted/native/provider acceptance | **W04.2 hosted acceptance complete; published-revision qualification in progress**: isolated run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864) passed the exact PGlite recovery step on Linux, macOS-latest, and macOS-15-intel. Candidate run [35608472226](https://github.com/andymac4182/mount-rs/actions/runs/35608472226) confirmed Windows Node cleanup/package distribution, and its Linux, ARM, and macOS-latest logs reached the PGlite subchecks but failed the enclosing step on the stale provider-matrix lockfile; the lockfile fix is now published as `caa9324`, and main CI run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646) is rerunning the matrix. Provider credentials/services remain explicit gates |
| Production rollout decision | **NO-GO**: the demo was successful, but production readiness still requires fresh hosted evidence plus artifact, durability, operational, and rollback gates |
| Current external blocker | The published candidate `98243d2` fixed the Windows handle lifetime and local HTTP-oracle parity; candidate run [35608472226](https://github.com/andymac4182/mount-rs/actions/runs/35608472226) then showed the exact PGlite subchecks passing but the enclosing Linux/ARM/macOS-latest verification steps failing on `tests/provider_matrix/Cargo.lock` being stale under `--locked`, while Windows Node [106361359489](https://github.com/andymac4182/mount-rs/actions/runs/35608472226/job/106361359489) completed package distribution successfully. Commit `caa9324` adds the missing `futures-util` lock entry; main CI run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646) is now the required rerun with Linux ARM [106365463598](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463598), Linux [106365463692](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463692), Windows [106365463630](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463630), macOS-latest [106365463795](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463795), and macOS-15-intel [106365463945](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463945). Production rollout remains NO-GO pending terminal rerun evidence, clean published-main package/consumer evidence, provider scope, persistence/rollback, observability, ownership, and release approval. |

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
| Native package/artifact and consumer validation | Qualification complete; published-main enforcement in progress | 85% | Qualification run [35594536095](https://github.com/andymac4182/mount-rs/actions/runs/35594536095) passed Windows Node package distribution [106316232053](https://github.com/andymac4182/mount-rs/actions/runs/35594536095/job/106316232053) and `aggregate-native` [106320575411](https://github.com/andymac4182/mount-rs/actions/runs/35594536095/job/106320575411). The exact hosted artifacts were packed, installed in a clean pnpm consumer with all five platform-package overrides, and the memory write/read/shutdown smoke passed. Main run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646) is exercising the lockfile-corrected published revision; no main-run package/consumer pass is claimed yet. | Inspect terminal Node/package logs, retain aggregate-native and clean-consumer evidence, and keep unrelated provider failures separate from the W04 conclusion. | 0.25–1h hosted verification | Hosted/package gate |
| Production-like persistence, restart, and version recovery | Hosted W04.2 complete; deployment policy pending | 80% | The isolated Linux, macOS-latest, and macOS-15-intel jobs all passed the PGlite integration/restart-recovery step; local evidence also covers disk restart and reconnect/version history. | Confirm the production data directory/configuration, version compatibility, backup/restore expectation, and rollback behavior for the actual deployment shape. | 1–2h | Engineering plus deployment decision |
| Provider and durability matrix | Explicitly bounded; not inferred | 40% for local scope / 0% for any unrun provider | R2 and TiDB/RustFS rows remain explicit credential/service skips; the ledger does not promote those skips to production acceptance. | If production will use those providers, obtain the real credentials/services and run the corresponding restart, cleanup, and recovery gates; otherwise record the launch scope as PGlite-only. | 1–4h plus provider wait | External provider gate |
| W04 provider-matrix lockfile reproducibility | Complete — implementation/local qualification | 100% | Candidate run [35608472226](https://github.com/andymac4182/mount-rs/actions/runs/35608472226) exposed the stale lockfile after hosted PGlite subchecks passed. Commit `caa9324` adds the existing `futures-util` dependency to `tests/provider_matrix/Cargo.lock`; the exact `./scripts/cargo-shared run --quiet --manifest-path tests/provider_matrix/Cargo.toml --locked` command now passes locally with `SUMMARY rust-sdk pass=5 skip=4 fail=0`. | Confirm the corrected lockfile in main CI run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646). | 0h remaining; hosted confirmation open | Engineering / reproducibility gate |
| Operational readiness: observability, runbook, limits, rollback, and ownership | Controlled template drafted; execution pending | 20% | [`W04-production-rollout.md`](W04-production-rollout.md) now defines the admission record, persistence/backup/restore/rollback procedure, provisional signal/limit matrix, controlled drills, and sign-off fields. This is planning/control evidence only; no alert, drill, owner, or production deployment is being claimed. | Bind the template to the actual deployment, replace provisional thresholds, execute D01–D06 with redacted evidence, connect collector/pager routes, and assign data/operator/release owners. | 1–2h engineering plus review and external execution | Production operations gate |
| Release approval | Blocked by the matrix above | 0% | Current decision is **NO-GO**. The positive demo is context, not a release sign-off. | Change to GO only after the fresh hosted logs, artifact/package evidence, production configuration/recovery evidence, provider boundary, and operational checklist are all signed off. | 0.5–1h review | Release decision |

### Production exit criteria

These are the W04 production-rollout conditions; a checked W04 implementation
item alone does not satisfy them.

- [x] Isolated published-revision CI run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864) completed the Linux Node,
  macOS-latest Node, and macOS-15-intel Node jobs successfully, with the exact
  `Verify PGlite integration and restart recovery` step passing in all three
  logs. Queued, skipped, cancelled, partial, or pre-fix evidence does not count.
- [x] The dependent native artifact aggregation and package-distribution
  checks pass, and a clean consumer install/smoke result is recorded for the
  supported release outputs. Qualification run [35594536095](https://github.com/andymac4182/mount-rs/actions/runs/35594536095)
  passed `aggregate-native` [106320575411](https://github.com/andymac4182/mount-rs/actions/runs/35594536095/job/106320575411)
  for all five native packages; a clean pnpm consumer installed the exact
  downloaded artifacts and passed the memory write/read/shutdown smoke. The
  new `test:distribution:consumer` CI step now enforces that check from the
  staged package.
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
| W04 production native package and clean-consumer gate | **Complete — qualification; hosted enforcement added** | **100%** | Run [35594536095](https://github.com/andymac4182/mount-rs/actions/runs/35594536095) passed Windows Node distribution/export coverage in job [106316232053](https://github.com/andymac4182/mount-rs/actions/runs/35594536095/job/106316232053) and five-package `aggregate-native` validation in job [106320575411](https://github.com/andymac4182/mount-rs/actions/runs/35594536095/job/106320575411). The exact hosted artifacts were downloaded, staged, packed, installed into a clean pnpm consumer with local platform-package overrides, and exercised through a memory write/read/shutdown smoke: `mount-rs clean consumer install/smoke: PASS`. | Run and retain the new consumer-smoke step on a published-main CI result; production release remains NO-GO until persistence/rollback, provider scope, operations, and ownership gates close. | 0.25–0.5h hosted enforcement; 0h package implementation | Hosted/package gate |
| W04 Windows hosted long-symlink gate | Complete — hosted qualification | 100% | Isolated run [35602906005](https://github.com/andymac4182/mount-rs/actions/runs/35602906005), Windows Rust job [106343197217](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197217), and exact test `windows_long_symlink_creation_uses_extended_path_fallback` passed after the missing-path trigger fix published as `900994a`. The local Windows-target check also passed. | None for this blocker; keep the broader native/package/Node gates open. | 0h remaining |
| W04 N-API reconcile-provider teardown | Complete — implementation and hosted qualification | 100% | Commit `88ecee2` clears the stored `Filesystem.reconcile` callback after successful shutdown, releasing the cloned `ChunkedFs`/SQLite handles before Windows temporary-directory removal. The N-API library suite passed 15 tests; candidate Windows Node job [106361359489](https://github.com/andymac4182/mount-rs/actions/runs/35608472226/job/106361359489) completed `Verify Windows package distribution` successfully. | Confirm the same cleanup on the lockfile-corrected main job [106365463630](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463630). | 0h remaining; main-run confirmation open |
| W04 HTTP parity oracle fixture | Complete — implementation and hosted candidate parity | 100% | Commit `98243d2` preserves the real mtime only for private active multipart-upload directories in `examples/http_oracle.rs`; local `check-http-parity.mjs` passed all 40 S3/WebDAV paired cases, the full S3 gateway suite passed 15 tests, and the Linux/ARM/macOS-latest logs in candidate run [35608472226](https://github.com/andymac4182/mount-rs/actions/runs/35608472226) reported the HTTP differential PASS before the later lockfile failure. | Confirm the HTTP-parity and exact PGlite verification steps on the lockfile-corrected main run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646). | 0h remaining; main-run confirmation open |
| W04 production-candidate hosted qualification | In progress — lockfile-corrected published-main rerun | 80% | Candidate run [35608472226](https://github.com/andymac4182/mount-rs/actions/runs/35608472226) showed Windows Node [106361359489](https://github.com/andymac4182/mount-rs/actions/runs/35608472226/job/106361359489) completed package distribution, while Linux [106361359694](https://github.com/andymac4182/mount-rs/actions/runs/35608472226/job/106361359694), Linux ARM [106361359780](https://github.com/andymac4182/mount-rs/actions/runs/35608472226/job/106361359780), and macOS-latest [106361359553](https://github.com/andymac4182/mount-rs/actions/runs/35608472226/job/106361359553) reached the hosted PGlite subchecks but failed the enclosing verification step on the stale lockfile; Intel remained in progress at the snapshot. Main run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646) from published `caa9324` is in progress with ARM [106365463598](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463598), Linux [106365463692](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463692), Windows [106365463630](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463630), macOS-latest [106365463795](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463795), and macOS-15-intel [106365463945](https://github.com/andymac4182/mount-rs/actions/runs/35609647646/job/106365463945). The exact recovery/package steps remain unproven. | Inspect terminal main-run logs, require exact `Verify PGlite integration and restart recovery` success on all advertised Node platforms, then retain aggregate/package/consumer/provider evidence. | 0.5–2h hosted runner time plus any evidence-backed remediation |
| W04 Windows Node packaging cleanup under hosted parity | In progress — source fix published, hosted confirmation pending | 75% | In run [35602906005](https://github.com/andymac4182/mount-rs/actions/runs/35602906005), Windows Node job [106343197149](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197149) failed before `Verify Windows package distribution` with `EBUSY: resource busy or locked, unlink ...\\blocks.sqlite`; the package step was skipped. Commit `88ecee2` clears the reconcile callback on shutdown, and local N-API tests plus the rebuilt debug addon passed. | Confirm the candidate Windows job reaches `Verify Windows package distribution`, then retain aggregate-native and clean-consumer results. | 0.25–1h hosted verification; remediation contingency 1–3h |
| W04 production rollout runbook and controlled-drill packet | Template drafted — production gate open | 20% | [`W04-production-rollout.md`](W04-production-rollout.md) records the launch boundary, evidence record, admission/preflight, staged rollout, backup/restore, rollback, observability/limits, drill matrix, and role sign-off fields. It intentionally leaves the decision **NO-GO** until the named deployment and operators execute the gates. | Choose the actual launch scope, bind the persistent data directory and version policy, execute backup/restore and rollback drills, connect telemetry/pager routes, measure thresholds, and obtain named-owner/release approval. | 1.5–3h engineering plus deployment/operator wait | Production operations gate |
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
- Qualification run `35594536095` on accepted W04 base `e515036` requalified
  the package path with the published Windows N-API aggregator fix. Linux
  Node `106316232279`, Linux ARM Node `106316232261`, macOS-latest Node
  `106316232225`, and macOS-15-intel Node `106316232268` all completed
  successfully; each exact `Verify PGlite integration and restart recovery`
  step and fragmented early-rejection step passed. Windows Node
  `106316232053` passed `Verify Windows package distribution` and uploaded
  `native-win32-x64-msvc` (artifact `10635493620`, SHA-256
  `037b91b7e7cd02232a4603044b48dc62f5a1f45ac5efa7e0a9f17b4d10f248ac`).
- The dependent `aggregate-native` job `106320575411` completed successfully:
  `Check packaging validation regressions` reported
  `mount-rs N-API artifact aggregation: PASS`, and
  `Validate all five native distribution packages without publishing` packed
  the root package plus all five platform packages. The exact downloaded
  artifacts were then used by the new clean-consumer smoke gate locally; a
  fresh pnpm install and memory write/read/shutdown test reported
  `mount-rs clean consumer install/smoke: PASS`.
- The overall `35594536095` run is not a release pass: unrelated
  `foundationdb-rustfs` job `106316231929` failed its provider gate, and the
  historical `rust (windows-latest)` job `106316232156` failed
  `windows_long_symlink_creation_uses_extended_path_fallback` with
  `FsError { code: Enoent, syscall: Some("symlink") }`. The failure was
  reproduced and the corrected trigger was published; the terminal hosted
  Windows Rust pass is recorded below rather than silently rewriting the
  historical result.
- The short-name Windows remediation `9189d52` was extended by published fix
  `900994a` to enter the extended/reparse fallback for `ERROR_FILE_NOT_FOUND`
  and `ERROR_PATH_NOT_FOUND` as well as `ERROR_FILENAME_EXCED_RANGE`. Isolated
  run [35602906005](https://github.com/andymac4182/mount-rs/actions/runs/35602906005)
  confirms the exact Windows Rust long-symlink test passed in job
  [106343197217](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197217).
- The same run does not close the Node/package gate: Windows Node job
  [106343197149](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197149)
  failed with a locked `blocks.sqlite` cleanup (`EBUSY`) before package
  distribution, while Linux, Linux ARM, macOS-latest, and macOS-15-intel jobs
  [106343197273](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197273),
  [106343197507](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197507),
  [106343197420](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197420),
  and [106343197472](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197472)
  failed the earlier `s3/multipart-upload-part-signed-chunks` HTTP-parity
  lane with `NoSuchUpload`; their exact PGlite steps were skipped. The
  dependent `aggregate-native` job [106347074679](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106347074679)
  was also skipped, so no current run promotes partial evidence to a package
  or W04 closure claim.
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
4. [x] Require the dependent native artifact aggregation/package checks and a
   clean supported-consumer smoke result before changing the production
   decision to GO; run `35594536095` and the local exact-artifact consumer
   smoke satisfy the qualification package gate, while the new CI step still
   awaits a published-main result.
5. Confirm the actual launch configuration's persistence, versioning,
   backup/restore, rollback, observability, limits, and ownership gates; keep
   R2/TiDB/RustFS skips explicit unless those providers are in launch scope.
6. [x] Publish the W04.2 and package-gate evidence with exact run/job links,
   run `git diff --check` and the relevant formatting check, commit the
   workflow/test/ledger chunk, and push it to `origin/main`.
7. If a hosted job fails, treat it as a new engineering chunk: capture the
   failure, patch the smallest evidence-backed root cause, run focused tests
   plus the relevant W04 gate, and commit/push before proceeding.
8. [ ] Confirm the published N-API teardown fix in main run `35609647646`:
   require Windows Node to reach `Verify Windows package distribution`, then
   retain artifact aggregation and clean-consumer results. The prior candidate
   Windows job passed this step, but is not the lockfile-corrected main run.
9. [ ] Confirm the published HTTP-oracle and lockfile fixes in main run
   `35609647646`: require every advertised Node job to complete its HTTP-parity
   lane and exact PGlite recovery step; record provider skips/failures
   separately.
10. [ ] Obtain or explicitly exclude the provider scope behind any remaining
    cross-platform `NoSuchUpload` or credential-gated failures before a
    production release decision.

## Provisional remaining effort and blockers

| Category | Estimate | Classification |
| --- | ---: | --- |
| Hosted macOS log inspection after runners start | Complete, ~0.5h actual | Engineering/verification |
| Tracker/dashboard closure, formatting, commit, and push | Complete, ~0.5h actual | Engineering/documentation |
| Potential remediation if hosted macOS exposes a regression | 2–6h | Engineering contingency |
| GitHub Actions queue delay | Resolved for W04.2 by isolated qualification branch; future shared-main churn remains external | External blocker; not engineering time |
| R2/TiDB local credential/service skips | Unknown | External provider prerequisites; not W04.2's macOS/Linux closure criterion |
| Native artifact aggregation and clean consumer smoke | Complete for qualification; 0.25–1h published-main enforcement remains | Hosted/package gate |
| Windows Node `EBUSY` cleanup remediation and hosted rerun | 1–3h | Engineering plus hosted/native gate |
| Cross-platform `NoSuchUpload` HTTP-parity lane | 0.25–1h published-main verification; provider fallback if it recurs | Hosted/provider boundary; local fixture fix passed the prior candidate parity lanes, lockfile-corrected main confirmation pending; not converted to a W04 pass |
| Production persistence/backup/rollback and version policy | 1–2h | Engineering plus deployment-owner decision |
| Runbook, observability, limits, and operational ownership | 1–2h | Production operations gate; review/ownership dependent |

Best-case remaining active engineering for the already-closed W04.2 gate is
approximately **0h**. Production rollout readiness still adds approximately
**3.5–8h** for the Windows Node cleanup remediation, hosted consumer
enforcement, persistence/rollback, and operational gates, excluding provider
setup, release-owner decisions, and hosted runner time. The Windows Rust
long-symlink blocker is complete; the Node cleanup and provider-parity failures
remain separate open tracks.

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
| 2026-09-21 21:20–21:42 AEST | Diagnosed and corrected the Windows N-API artifact aggregator's `.cmd` executable handling, then requalified the package path on accepted W04 base `e515036`. | Windows Node, all four Node platform jobs, and `aggregate-native` passed in run `35594536095`; the overall run remained failed only on unrelated provider and Windows Rust gates. | Engineering/hosted verification, ~0.75h; provider/native failures remain external/remediation tracks |
| 2026-09-21 21:43–21:58 AEST | Downloaded the exact hosted artifacts, assembled and packed the staged root plus five platform packages, added the reusable clean-consumer smoke script and CI step, and ran focused formatting/package checks. | Clean pnpm consumer install and memory write/read/shutdown smoke passed; `test:distribution:aggregate`, `node --check`, `git diff --check`, and shared Cargo formatting check passed. | Engineering/verification, ~0.5h; hosted enforcement will be rechecked after publication |
| 2026-09-21 22:20–22:47 AEST | Diagnosed the hosted Windows long-symlink failure, added the short-parent-path fallback, ran rustfmt/diff checks, Windows-target compilation, and the local host suite, then published the fix. Added the W04-specific production rollout/runbook template and linked it from this ledger. | Local checks passed; published fix `9189d52`; isolated qualification run `35600696969` is still the only hosted source for the remediation, with Linux Node HTTP-parity failures and PGlite skips recorded explicitly. Production decision remains NO-GO. | Engineering/documentation/hosted verification, ~1h; Windows/macOS runner evidence remains external |
| 2026-09-21 22:50–23:32 AEST | Added diagnostic instrumentation only on the isolated qualification branch, proved the first Windows symlink call returned `ERROR_PATH_NOT_FOUND` (`3`), corrected the fallback trigger, ran the hosted qualification, removed diagnostics, squashed the net source change, and rebased/pushed it as `900994a`. | Run `35602906005` passed Windows Rust job `106343197217` and the exact long-symlink test; Windows Node job `106343197149` failed `EBUSY` unlinking `blocks.sqlite`, and the four other Node jobs failed `NoSuchUpload` before their PGlite steps. Ledger remains NO-GO and records the next remediation chunks. | Engineering/hosted diagnosis, ~0.75h; Node/provider gates remain open |
| 2026-09-21 23:33–23:42 AEST | Diagnosed the Windows `EBUSY` cleanup as a retained N-API `Filesystem.reconcile` callback holding cloned SQLite-backed providers, changed the callback slot to clear during shutdown, rebuilt the addon, and ran the focused N-API suite. | Published as `88ecee2`; 15 N-API library tests passed and the local chunked integration path passed. The Windows GNU target remains a hosted/toolchain gate because this Mac lacks `x86_64-w64-mingw32-gcc`. | Engineering/verification, ~0.5h; hosted Windows confirmation remains external |
| 2026-09-21 23:43–23:50 AEST | Reproduced the local HTTP `NoSuchUpload` mismatch, traced it to the oracle's fixed multipart-directory mtime causing the reaper to delete an active upload, preserved the active directory mtime, and reran the differential and S3 gateway suites. | Published as `98243d2`; HTTP differential passed `40/40` and `mount-rs-s3` gateway passed 15 tests. | Engineering/verification, ~0.35h; hosted parity confirmation remains external |
| 2026-09-21 23:51–23:56 AEST | Published the candidate branch from `98243d2`, fetched the latest `origin/main`, started qualification run `35608472226`, and recorded the terminal-evidence boundary. | Windows, Linux, Linux ARM, macOS-latest, and macOS-15-intel Node jobs were in progress; the ARM addon build had completed, but no exact PGlite/package pass was claimed. | Release engineering/hosted monitoring, ~0.1h; runner time excluded |
| 2026-09-22 00:00–00:05 AEST | Inspected the first terminal production-candidate logs. The hosted PGlite subchecks and HTTP differential passed on Linux, Linux ARM, and macOS-latest, but the enclosing verification step attempted to rewrite the stale provider-matrix lockfile under `--locked`. | Reproduced locally, added the missing `futures-util` lock entry, verified the exact locked command, and published `caa9324`; main CI run `35609647646` is the lockfile-corrected rerun. | Engineering/hosted diagnosis, ~0.35h; main-run verification remains external |

## Publication note

This ledger revision records the W04.2 closure, the terminal hosted Windows
long-symlink/package pass, the local N-API and HTTP-oracle fixes, the stale
provider-matrix lockfile diagnosis and fix `caa9324`, and the in-progress
published-main qualification run `35609647646`. It is reviewed with
`git diff --check` and the relevant formatting check, then committed and
pushed to `origin/main`. It does **not** approve production: main-run Node
logs, provider scope, persistence/rollback policy, observability, runbook
execution, ownership, hosted enforcement of the new consumer step, and release
approval remain open.
