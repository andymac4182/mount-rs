# W04 PGlite progress ledger

This ledger tracks the W04 workstream in `WORK_TRACKER.md`. It separates
implementation, deterministic/local qualification, and hosted/native/provider
acceptance. Percentages and time estimates are provisional: a queued job is
not a pass, and local evidence does not substitute for the hosted macOS/Linux
gate required by W04.2.

## Snapshot

| Field | Current value |
| --- | --- |
| Snapshot base revision | `fac9c7e004b4eb1d9e742c180f967c97f3d89648` (origin/main immediately before this ledger update) |
| Ledger publication | This current ledger revision is committed and pushed to `origin/main`; the exact commit is recorded in Git history |
| Current-head local evidence revision | `5424080afbc2027d5dcb0ab79a09ca9a4a32fdc8` (the docs-only `fac9c7e` sync landed after the gate) |
| Snapshot time | 2026-09-21 17:54 AEST / 2026-09-21 07:54 UTC |
| Tracker section | `WORK_TRACKER.md` § W04 — PGlite |
| Checklist completion | **87.5%**: 7 of 8 W04 checklist items are checked; W04.2 remains open |
| Implementation/local qualification | **Complete for the recorded packet**; a fresh current-head local gate also passed at `5424080` |
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
| W04.1 full `scripts/test-pglite.sh` local gate | Complete — local qualification | 100% | The current-head rerun at `5424080afbc2027d5dcb0ab79a09ca9a4a32fdc8` exited 0 and passed slot cleanup, injected failure, provider parity, reconnect, fencing, cancellation, disk restart, mixed stores, Node factories, chunked mounts, and userspace FUSE. R2 and TiDB/RustFS rows retained explicit skips. | No further local W04.1 action unless W04.2 exposes a regression. | 0h remaining |
| Fresh current-tree rerun, SDK/CLI users, upstream, and trace lanes | Complete — local qualification | 100% | The current-head `scripts/test-pglite.sh` run exited 0 after rebuilding the ignored N-API addon against the checked-out source. Results included Rust SDK `pass=6 skip=3 fail=0`, Node SDK `pass=5 skip=3 fail=0`, and CLI `pass=12 skip=2 fail=0`; the focused chunked N-API test also passed. The run did not execute upstream/trace lanes because `MOUNTX_SOURCE` was unset, and credential/service-gated rows remained explicit skips. Historical upstream/trace passes remain recorded separately; hosted/provider claims are not inferred from this local result. | None for the current local result; hosted W04.2 remains separate. | 0h remaining |
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

## Publication note

This ledger revision is a documentation chunk. It was reviewed with
`git diff --check`, committed, and pushed to `origin/main`. It does **not**
close W04.2; that requires the missing hosted macOS log evidence described
above.
