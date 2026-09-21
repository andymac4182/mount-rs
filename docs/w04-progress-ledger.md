# W04 PGlite progress ledger

This ledger tracks the W04 workstream in `WORK_TRACKER.md`. It separates
implementation, deterministic/local qualification, and hosted/native/provider
acceptance. Percentages and time estimates are provisional: a queued job is
not a pass, and local evidence does not substitute for the hosted macOS/Linux
gate required by W04.2.

## Snapshot

| Field | Current value |
| --- | --- |
| Snapshot base revision | `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` (origin/main immediately before this ledger update) |
| Ledger publication | This current ledger revision is committed and pushed to `origin/main`; the exact commit is recorded in Git history |
| Current-head local evidence revision | `5424080afbc2027d5dcb0ab79a09ca9a4a32fdc8` (the docs-only `fac9c7e` sync landed after the gate) |
| Latest synced verification revision | `113724487e9efc172ab69254d995377cfcfab296` (workspace test and scoped Clippy evidence; unrelated W26/TiDB/CLI changes are included) |
| Latest full W04 gate revision | `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` (current `origin/main` after the chunked-shutdown fix; exact pinned oracle; full `scripts/test-pglite.sh` exited 0) |
| Snapshot time | 2026-09-21 18:30 AEST / 2026-09-21 08:30 UTC |
| Tracker section | `WORK_TRACKER.md` § W04 — PGlite |
| Checklist completion | **87.5%**: 7 of 8 W04 checklist items are checked; W04.2 remains open |
| Implementation/local qualification | **Complete for the recorded packet**; the fresh post-fix oracle-enabled W04 gate passed at `6d59d20`, and synced workspace tests plus scoped W04 Clippy passed at `1137244` |
| Hosted/native/provider acceptance | **Incomplete**: hosted Linux Node evidence is green; hosted macOS Node evidence is still queued; R2/TiDB rows are explicit local skips |
| Overall release decision | **Not complete** until W04.2's post-fix macOS and Linux Node logs pass |
| Current external blocker | GitHub Actions jobs [106211636737](https://github.com/andymac4182/mount-rs/actions/runs/35560240894/job/106211636737) and [106211636695](https://github.com/andymac4182/mount-rs/actions/runs/35560240894/job/106211636695) have remained `queued` with no `started_at` |

The 87.5% figure is a checklist ratio, not a readiness claim. The open item
is the acceptance gate that determines whether the workstream can be called
fully working.

## Work-item ledger

| Work item | Status | Completion | Evidence | Remaining actions | Provisional engineering estimate |
| --- | --- | ---: | --- | --- | --- |
| Land real socket-server integration and provider/factory coverage | Complete — implementation | 100% | W04 tracker checkbox; current tree contains the PGlite socket server, provider, factory, and integration test surfaces under `integrations/mount-rs-pglite/` and `tests/pglite/`. | None for this item. Keep regressions covered by the W04 gate. | 0h remaining |
| Fix transaction cleanup before releasing a connection slot | Complete — implementation and regression | 100% | Tracker records `29ffb3b`; deterministic slot-release suite passed during the recorded W04 local gate. | None for this item. | 0h remaining |
| Add injected cleanup-failure regression and fail closed | Complete — implementation and regression | 100% | Tracker records `cfce82a`; `server_cleanup_failure.mjs` reported `pglite cleanup failure fail-closed: ok`; the expected injected error was retained as diagnostic evidence. | None for this item. | 0h remaining |
| W04.1 full `scripts/test-pglite.sh` local gate | Complete — local qualification | 100% | The latest post-fix oracle-enabled run at `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` exited 0 and passed slot cleanup, injected failure, provider parity, reconnect, fencing, cancellation, disk restart, mixed stores, Node factories, chunked mounts, and userspace FUSE. R2 and TiDB/RustFS rows retained explicit skips. The synced workspace test gate also exited 0 at `1137244`; scoped PGlite/N-API Clippy passed with `-D warnings`. | No further local W04.1 action unless W04.2 exposes a regression. | 0h remaining |
| Fresh current-tree rerun, SDK/CLI users, upstream, and trace lanes | Complete — local qualification | 100% | The latest post-fix current-tree run with the exact pinned oracle `pithings/mountx@85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` exited 0: Rust SDK `pass=6 skip=3 fail=0`, Node SDK `pass=5 skip=3 fail=0`, CLI `pass=12 skip=2 fail=0`, upstream `1,200 passed / 82 skipped`, and trace `40/40` across five seeds and eight backends, including PGlite and chunked-PGlite. R2/TiDB/RustFS credentials/services remained explicit skips; hosted/provider claims are not inferred from this local result. | None for the current local result; hosted W04.2 remains separate. | 0h remaining |
| Teardown-race fix and bounded close/reopen safety | Complete — implementation and local regression | 100% | Tracker records the PostgreSQL Terminate-frame cleanup path, I/O-turn barrier, listener restoration, and tracked cleanup barrier. Readiness passed 10/10; bounded close/reopen passed 5/5 in the recorded gate; focused current-tree runs also passed the bounded regression repeatedly. | None unless hosted macOS reproduces the historical `Eio` failure. | 0h remaining; 2–6h contingency if hosted failure reproduces |
| W04.2 hosted macOS/Linux reconnect acceptance | **Open — external hosted gate** | **0% of this item** | Required run: [35560240894](https://github.com/andymac4182/mount-rs/actions/runs/35560240894). Linux Node job [106211636722](https://github.com/andymac4182/mount-rs/actions/runs/35560240894/job/106211636722) completed successfully; direct logs include slot-release success, cleanup-failure fail-closed success, `mount-rs N-API PGlite integration: PASS`, Rust SDK `pass=6 skip=3 fail=0`, Node SDK `pass=5 skip=2 fail=0`, and CLI `pass=12 skip=2 fail=0`. macOS jobs [106211636737](https://github.com/andymac4182/mount-rs/actions/runs/35560240894/job/106211636737) and [106211636695](https://github.com/andymac4182/mount-rs/actions/runs/35560240894/job/106211636695) remain queued, so their required `Verify PGlite integration and restart recovery` logs do not yet exist. | Keep monitoring the two queued jobs. When each completes, inspect its log for the exact PGlite recovery step. If both pass, update W04.2 and the dashboard, run formatting/diff checks, commit, rebase if needed, and push. If either fails, isolate the failure, fix it, rerun the focused and full relevant gates, and publish that fix as the next chunk. | 0.25–0.5h log audit once runners start; 0.5–1h tracker/publish work if green; 2–6h if a code regression requires remediation. External queue time is unknown and is not engineering time. |
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

- The canonical hosted Linux Node job passed the PGlite integration and its
  benchmark/trace work on run `35560240894`. This is sufficient Linux evidence
  for W04.2's two-platform requirement but not sufficient to close W04.2.
- The two macOS Node jobs have remained queued since `2026-09-21T04:14:51Z`
  with no `started_at`. Queued is an external wait, not a failure and not a
  pass. The active heartbeat monitor is responsible for rechecking them.
- The same CI run contains unrelated non-W04 failures in other jobs. They must
  remain visible in CI review but do not change the W04-specific conclusion.

## Remaining-action checklist

1. Monitor jobs `106211636737` and `106211636695` for a status transition.
2. For every started/completed macOS job, retrieve the job log and locate the
   exact `Verify PGlite integration and restart recovery` step.
3. Require both macOS jobs and the already-green Linux Node job to pass before
   changing W04.2 to `[x]`.
4. If green, update `WORK_TRACKER.md` with the exact run/job links, preserve
   the explicit unrelated failures/skips, run `git diff --check` and the
   relevant formatting check, commit the documentation closure, and push it
   to `origin/main`.
5. If a hosted job fails, treat it as a new engineering chunk: capture the
   failure, patch the smallest evidence-backed root cause, run focused tests
   plus the relevant W04 gate, and commit/push before proceeding.

## Provisional remaining effort and blockers

| Category | Estimate | Classification |
| --- | ---: | --- |
| Hosted macOS log inspection after runners start | 0.25–0.5h | Engineering/verification |
| Tracker/dashboard closure, formatting, commit, and push if both macOS jobs pass | 0.5–1h | Engineering/documentation |
| Potential remediation if hosted macOS exposes a regression | 2–6h | Engineering contingency |
| GitHub Actions queue delay | Unknown | External blocker; not engineering time |
| R2/TiDB local credential/service skips | Unknown | External provider prerequisites; not W04.2's macOS/Linux closure criterion |

Best-case remaining active engineering is approximately **0.75–1.5h after
runner availability**. A hosted regression would expand that estimate to
approximately **2.75–7.5h**, excluding external queue time.

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

## Publication note

This ledger revision is a documentation chunk. It was reviewed with
`git diff --check`, committed, and pushed to `origin/main`. It does **not**
close W04.2; that requires the missing hosted macOS log evidence described
above.
