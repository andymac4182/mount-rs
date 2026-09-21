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
| Snapshot base revision | `299c127` (`origin/main` immediately before this policy chunk; it includes the published W04 FUSE/rootmode packet, the concurrent transport-hook work, and later mainline commits; qualification run `35620669973` used the earlier isolated source `f54590c`) |
| Ledger publication | This current ledger revision records the W04 PGlite-only launch-config policy chunk and is published on `origin/main`; the exact commit is recorded in Git history |
| Current-head local evidence revision | `a55bd64` plus concurrent metadata correction `aa3dae3` and assertion alignment `9ef982d`; focused locked FUSE tests and formatting passed locally |
| Latest synced verification revision | `113724487e9efc172ab69254d995377cfcfab296` (workspace test and scoped Clippy evidence; unrelated W26/TiDB/CLI changes are included) |
| Latest full W04 gate revision | `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` (current `origin/main` after the chunked-shutdown fix; exact pinned oracle; full `scripts/test-pglite.sh` exited 0) |
| Latest published repository revision | `299c127` is the base of this policy chunk; the current ledger/code commit is the next published `origin/main` tip, with unrelated concurrent commits preserved |
| Qualification evidence revision | Candidate source `f54590c83b74f86110086104accf798fedb00763` on `andymac4182/c/w04-provider-requalification-current-head-20260922`; run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973) is terminal overall **failure**, but all four Node jobs passed their exact PGlite/restart step: Linux [106402628455](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628455), ARM [106402628499](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628499), macOS-latest [106402628531](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628531), and macOS-15-intel [106402628195](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628195). Windows packaging [106402628382](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628382) and aggregate/clean-consumer [106412877853](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106412877853) also passed. FoundationDB/RustFS [106402628095](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628095) passed its durable/restart/configured native path. Ozone compositions [106402628025](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628025), Ozone/TiDB [106402628026](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628026), and Ozone/FoundationDB [106402628170](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628170) failed only their hard 1,000-IOPS provider thresholds. The unrelated Ubuntu Rust job [106402628328](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628328) failed pre-current-main `clippy::collapsible_if`; the fix is already present in current main via `5a7940b`, so a fresh post-tip full CI result is still required for release evidence. Companion Fault Injection [35620669906](https://github.com/andymac4182/mount-rs/actions/runs/35620669906) completed successfully on Linux, macOS, and Windows |
| Snapshot time | 2026-09-22 02:27 AEST |
| Tracker section | `WORK_TRACKER.md` § W04 — PGlite |
| Checklist completion | **100%**: 8 of 8 W04 checklist items are checked; W04.2 hosted acceptance is closed |
| Implementation/local qualification | **Complete for the recorded packet**; the fresh post-fix oracle-enabled W04 gate passed at `6d59d20`, synced workspace tests plus scoped W04 Clippy passed at `1137244`, the focused locked `mount-rs-fuse` suite passed after `a55bd64`, and the new W04 PGlite-only launch-config policy passed its positive/negative fixtures plus CLI `--check` locally |
| Hosted/native/provider acceptance | **W04.2 and the current Node/package candidate gates are complete; production provider scope remains open**: run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973) passed all four Node exact PGlite/restart steps, Windows package distribution, five-package aggregation, and clean-consumer smoke. FoundationDB/RustFS passed durable chunk/reopen, configured FUSE, N-API, service restart, and cleanup markers. Ozone functional composition/restart/fault checks passed, but Ozone, Ozone/TiDB, and Ozone/FoundationDB failed their required 1,000-IOPS thresholds; the run is therefore not a release pass. Fault Injection run [35620669906](https://github.com/andymac4182/mount-rs/actions/runs/35620669906) passed on Linux, macOS, and Windows. |
| Production rollout decision | **NO-GO**: the demo was successful and the credential-free PGlite-only config policy is implemented, but production readiness still requires hosted policy evidence plus artifact, deployment durability, operational, and rollback gates |
| Current external blocker | The W04 production-relevant failures in run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973) are configured Ozone provider thresholds of 5.79 and 53.18 IOPS in `ozone-compositions`, 14.28 in `ozone-tidb`, and 32.74 in `ozone-foundationdb`, each below the hard 1,000 target despite successful lifecycle operations and cleanup. The same run also contains an unrelated Ubuntu Rust job failure from a pre-tip `clippy::collapsible_if` in `transports/mount-rs-fuse/src/mount.rs:1043`; current `origin/main` already contains the collapsed form via `5a7940b`, but a fresh post-tip full CI result is not yet evidence. Production remains NO-GO pending a provider launch-scope decision or passing performance evidence, deployment-specific persistence/backup/rollback, observability/runbook/ownership, and release approval. |

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
| W04 implementation packet and deterministic local qualification | Complete — current Node/package candidate green; provider/operations follow-up open | 100% implementation / 100% current Node/package qualification | The current packet includes the socket server, cleanup/fencing fixes, version metadata, mount-free VFS, native hosting tests, the published fixture-shutdown EPIPE fixes `bdfcb11` and `41d5514`, and the privileged-FUSE rootmode correction `a55bd64`; the oracle-enabled local gate at `6d59d20` exited 0 with upstream `1,200 passed / 82 skipped` and trace `40/40`. Run `35620669973` then passed all four Node exact PGlite/restart steps, Windows package distribution, aggregate-native, and clean-consumer smoke. | Decide launch provider scope and complete deployment-specific persistence/rollback and operations evidence; the terminal provider IOPS failures remain external/performance gates. | 0.25–0.75h engineering; hosted/provider wait external | Engineering plus hosted gate |
| Hosted Linux Node acceptance | Complete — current candidate confirmation | 100% | Isolated run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864), job [106298858958](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298858958), and current run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973), job [106402628455](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628455), completed successfully; exact PGlite/restart and fragmented early-rejection steps passed, with PGlite-inclusive trace and explicit provider skips. | None for the hosted Linux W04.2 gate. | 0h | Hosted gate |
| Hosted macOS-latest Node acceptance | Complete — current candidate confirmation | 100% | Current run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973), job [106402628531](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628531), completed successfully; exact PGlite/restart and fragmented early-rejection steps passed, and the log recorded `providersFailed: 0` plus PGlite/chunked-PGlite trace passes. | None for the hosted macOS-latest W04.2 gate. | 0h | Hosted/native gate |
| Hosted macOS-15-intel Node acceptance | Complete — current candidate confirmation | 100% | Current run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973), job [106402628195](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628195), completed successfully at `2026-09-21T16:09:09Z`; the exact PGlite/restart step passed after the long Intel trace, early rejection passed, and the historical socket EPIPE did not recur. | None for the hosted macOS-15-intel W04.2 gate. | 0h | Hosted/native gate |
| Native package/artifact and consumer validation | Complete — current candidate | 100% | Current Windows Node job [106402628382](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628382) uploaded `native-win32-x64-msvc.zip` as artifact `10648916857`, SHA-256 `b939bb7007823f022e22af5c9457d85b9dd7d17633dca6c219f84a8fd8200388`. Aggregate job [106412877853](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106412877853) passed artifact aggregation, all five native distribution validations, and `mount-rs clean consumer install/smoke: PASS`. | Retain the exact artifact/hash in the release record; production release remains NO-GO until persistence/rollback, provider scope, operations, and ownership gates close. | 0.25h evidence publication; 0h package implementation | Hosted/package gate |
| Production-like persistence, restart, and version recovery | Hosted candidate qualification complete; local backup/rollback rehearsal green; deployment evidence pending | 90% hosted qualification / 35% implementation and local rehearsal / 0% production drill | Current run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973) passed all four exact PGlite/restart steps and PGlite-inclusive five-seed/eight-backend traces; FoundationDB/RustFS [106402628095](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628095) passed durable chunk/reopen, service restart, configured FUSE, and cleanup markers. The new `scripts/verify-w04-pglite-production-config.mjs` policy passed its positive/negative fixtures and CLI `--check` locally. The focused lifecycle file passed both graceful restart and `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`; these checks demonstrate configuration shape and local control flow, not the actual production deployment. | Retain hosted policy output, bind the actual PGlite server data directory/configuration and version policy, and execute an approved encrypted backup/restore and rollback drill with measured RPO/RTO and owner approval. | 1–2h engineering plus deployment-owner wait | Engineering plus deployment decision |
| Provider and durability matrix | PGlite/FDB-RustFS functional paths green; Ozone performance gate open | 75% for qualified functional scope / 0% for unapproved providers | Current run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973) passed FoundationDB/RustFS [106402628095](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628095), Ozone lifecycle/composition checks, and the companion Fault Injection run [35620669906](https://github.com/andymac4182/mount-rs/actions/runs/35620669906). The configured Ozone performance rows failed their hard 1,000-IOPS threshold: 5.79 and 53.18 in `ozone-compositions` [106402628025](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628025), 14.28 in Ozone/TiDB [106402628026](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628026), and 32.74 in Ozone/FoundationDB [106402628170](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628170); lifecycle operations completed successfully and benchmark artifacts were retained. | Obtain a release-owner decision that Ozone is out of launch scope or remediate/qualify the 1,000-IOPS requirement with provider owners; retain explicit R2/TiDB/RustFS credential/service skips where not configured. | 1–4h engineering/provider work plus hosted wait | External provider/performance gate |
| W04 provider-matrix lockfile reproducibility | Complete — implementation and isolated hosted qualification | 100% | Candidate run [35608472226](https://github.com/andymac4182/mount-rs/actions/runs/35608472226) exposed the stale lockfile after hosted PGlite subchecks passed. Commit `caa9324` adds the existing `futures-util` dependency to `tests/provider_matrix/Cargo.lock`; the exact locked command passes locally, and all five Node jobs in [35610813385](https://github.com/andymac4182/mount-rs/actions/runs/35610813385) completed their provider-matrix/PGlite steps successfully. The shared-main confirmation run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646) was cancelled, so it is not counted. | None for the W04 provider-matrix lockfile; Ozone has a separate stale `tests/ozone/Cargo.lock` gate if that provider enters launch scope. | 0h remaining | Engineering / reproducibility gate |
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
  supported release outputs. Current qualification run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973)
  passed `aggregate-native` [106412877853](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106412877853)
  for all five native packages and `mount-rs clean consumer install/smoke:
  PASS`; Windows Node [106402628382](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628382)
  retained `native-win32-x64-msvc.zip` artifact `10648916857` with SHA-256
  `b939bb7007823f022e22af5c9457d85b9dd7d17633dca6c219f84a8fd8200388`.
- [x] The companion fault-injection qualification [35620669906](https://github.com/andymac4182/mount-rs/actions/runs/35620669906)
  completed successfully on Linux [106402627672](https://github.com/andymac4182/mount-rs/actions/runs/35620669906/job/106402627672),
  macOS [106402627530](https://github.com/andymac4182/mount-rs/actions/runs/35620669906/job/106402627530),
  and Windows [106402627687](https://github.com/andymac4182/mount-rs/actions/runs/35620669906/job/106402627687);
  the fault-injection tests reported zero failures and strict package Clippy
  completed on each platform. This is failure-behavior evidence, not a
  production durability or rollback sign-off.
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
| W04 production native package and clean-consumer gate | **Complete — current production candidate** | **100%** | Current run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973) passed Windows Node distribution in job [106402628382](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628382), uploaded `native-win32-x64-msvc.zip` artifact `10648916857` with SHA-256 `b939bb7007823f022e22af5c9457d85b9dd7d17633dca6c219f84a8fd8200388`, and passed five-package `aggregate-native` plus clean-consumer validation in job [106412877853](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106412877853). | Retain the exact artifact/hash in the release record; production release remains NO-GO until persistence/rollback, provider scope, operations, and ownership gates close. | 0.25h evidence publication; 0h package implementation | Hosted/package gate |
| W04 Windows hosted long-symlink gate | Complete — hosted qualification | 100% | Isolated run [35602906005](https://github.com/andymac4182/mount-rs/actions/runs/35602906005), Windows Rust job [106343197217](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197217), and exact test `windows_long_symlink_creation_uses_extended_path_fallback` passed after the missing-path trigger fix published as `900994a`. The local Windows-target check also passed. | None for this blocker; keep the broader native/package/Node gates open. | 0h remaining |
| W04 N-API reconcile-provider teardown | Complete — implementation and hosted qualification | 100% | Commit `88ecee2` clears the stored `Filesystem.reconcile` callback after successful shutdown, releasing the cloned `ChunkedFs`/SQLite handles before Windows temporary-directory removal. The N-API library suite passed 15 tests; isolated Windows Node job [106369156167](https://github.com/andymac4182/mount-rs/actions/runs/35610813385/job/106369156167) completed `Verify Windows package distribution` successfully, and aggregate job [106379759443](https://github.com/andymac4182/mount-rs/actions/runs/35610813385/job/106379759443) passed the clean consumer gate. | None for this cleanup fix; retain the hosted log with the release record. | 0h remaining |
| W04 HTTP parity oracle fixture | Complete — implementation and isolated hosted candidate parity | 100% | Commit `98243d2` preserves the real mtime only for private active multipart-upload directories in `examples/http_oracle.rs`; local `check-http-parity.mjs` passed all 40 S3/WebDAV paired cases, the full S3 gateway suite passed 15 tests, and all four Unix Node jobs in candidate run [35610813385](https://github.com/andymac4182/mount-rs/actions/runs/35610813385) completed the HTTP differential lane before their exact PGlite recovery step. | None for the W04 HTTP parity fix; provider-scope failures are tracked separately. | 0h remaining |
| W04 production-candidate hosted qualification | Partial — Node/package/FDB-RustFS gates green; Ozone performance and full-tree CI gates open | 95% | Current run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973) passed Linux [106402628455](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628455), ARM [106402628499](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628499), macOS-latest [106402628531](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628531), and macOS-15-intel [106402628195](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628195) exact PGlite/restart steps; the logs show SDK/CLI `fail=0`, PGlite-inclusive trace `PASS`, and explicit credential/service skips. Windows package [106402628382](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628382), aggregate/package/clean-consumer [106412877853](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106412877853), and FoundationDB/RustFS [106402628095](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628095) passed. The overall run remains failure because Ozone provider thresholds failed; the Ubuntu Rust job failed a pre-tip Clippy lint against source `f54590c`, while current main already contains the collapsed-if correction in `5a7940b`. | Obtain current-main full CI confirmation, decide whether Ozone is in launch scope, and either remediate/qualify its 1,000-IOPS requirement or explicitly exclude it with release-owner approval; retain NO-GO until production gates close. | 0.5–2h scope/approval plus hosted/provider wait |
| W04 credential-free PGlite-only launch-config policy | Complete — implementation; hosted policy output pending | 100% implementation / 0% hosted policy | `scripts/verify-w04-pglite-production-config.mjs` now validates an absolute normalized mountpoint, splitstore PGlite metadata/blocks, durable providers, external `MOUNT_RS_PGLITE_URL` references, distinct scoped volume keys, bounded chunking, owner metadata, and fail-closed unknown/inline-secret cases. The positive and two negative fixtures passed locally, and the positive fixture passed the CLI `--check` parser without opening a provider. CI now runs the same policy and negative fixtures without credentials or network. | Retain terminal CI policy output, then bind the actual PGlite server data directory, version policy, backup/restore, rollback, RPO/RTO, and owners to a deployment; this policy is shape-only. | 0.5h implementation; hosted wait and deployment review external | Production config gate |
| W04 local backup/restore/rollback rehearsal | Complete — local supporting evidence; production drill open | 100% harness / 0% production | `tests/pglite_server_lifecycle.rs` now copies a quiesced disk-backed PGlite data directory into an isolated restore directory, verifies a fresh-client read, restores the pre-candidate copy after a bad-release marker, and emits `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`; both ignored lifecycle tests passed locally. | Replace the filesystem copy with the approved encrypted backup mechanism, execute D02/D03 against the named deployment, measure RPO/RTO, and retain redacted backup/restore/rollback evidence. | 0.5–1h harness implementation complete; production execution external | Production durability/recovery gate |
| W04 native R2 key path in configuration-driven FoundationDB/RustFS test | Complete — current hosted confirmation; production provider scope pending | 100% implementation / 100% current hosted qualification | Run [35620669973](https://github.com/andymac4182/mount-rs/actions/runs/35620669973), FoundationDB/RustFS job [106402628095](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628095), passed the relative-key configuration path, durable chunk/reopen checks, configured FUSE mount/reopen, N-API bounded readdir, service restart, and cleanup markers after `d07c711` and the rootmode packet. | Keep the provider as an explicit launch-scope decision; this hosted result does not establish customer production credentials, replication, backup, or DR. | 0.25h evidence retention; external provider/owner gate |
| W04 macOS early-rejection socket EPIPE guard | Complete — current hosted confirmation | 100% | Intel job [106402628195](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628195) passed fragmented early rejection and the exact PGlite/restart step after commit `41d5514`; the long trace and PGlite run completed without the historical underlying-socket EPIPE. | None for the published socket guard; retain the hosted log in the release record. | 0h remaining |
| W04 privileged Linux FUSE `rootmode` boundary | Complete — current hosted confirmation | 100% implementation / 100% current hosted qualification | FoundationDB/RustFS job [106402628095](https://github.com/andymac4182/mount-rs/actions/runs/35620669973/job/106402628095) passed `cli_foundationdb_rustfs_config_binary_mounts_and_reopens`, `FOUNDATIONDB_CLI_PASS`, N-API bounded readdir, service restart, and RustFS cleanup after `a55bd64`, `aa3dae3`, and `9ef982d`. | None for the rootmode/config-mount fix; provider production scope and performance thresholds remain separate gates. | 0h remaining |
| W04 Windows Node packaging cleanup under hosted parity | Complete — hosted qualification | 100% | In isolated run [35610813385](https://github.com/andymac4182/mount-rs/actions/runs/35610813385), Windows Node job [106369156167](https://github.com/andymac4182/mount-rs/actions/runs/35610813385/job/106369156167) completed `Verify Windows package distribution` successfully after commit `88ecee2` cleared the reconcile callback during shutdown. `aggregate-native` [106379759443](https://github.com/andymac4182/mount-rs/actions/runs/35610813385/job/106379759443) then passed the staged-package and clean-consumer gates. | None for this cleanup fix; production artifact provenance and release approval remain open. | 0h remaining |
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
- Isolated production-candidate run `35610813385` on source `d55080550cd4`
  requalified the lockfile-corrected W04 packet without shared-main
  cancellation. Linux Node `106369155618`, Linux ARM Node `106369155597`,
  macOS-latest Node `106369155745`, and macOS-15-intel Node `106369155579`
  all completed successfully; each exact `Verify PGlite integration and
  restart recovery` step passed. The logs recorded Rust SDK `pass=6 skip=3
  fail=0`, Node SDK `pass=5 skip=3 fail=0`, CLI `pass=12 skip=2 fail=0`,
  upstream `1,200 passed / 82 skipped`, and PGlite-inclusive trace evidence
  with `providersFailed: 0` and `providersSkipped: 0`. Windows Node
  `106369156167` passed package distribution. Dependent `aggregate-native`
  `106379759443` passed artifact aggregation, all five native package
  validations, and `mount-rs clean consumer install/smoke: PASS`.
- Corrected isolated provider requalification run `35615617217` used source
  `a2fda1167d2dd26e57924f3c75d122255b323422` and is terminal overall failure.
  Linux Node `106385420039`, Linux ARM Node `106385419798`, and macOS-latest
  Node `106385419751` passed their exact `Verify PGlite integration and
  restart recovery` steps; the macOS-latest trace included PGlite and
  chunked-PGlite across five seeds with `providersFailed: 0` and
  `providersSkipped: 0`. Windows Node `106385419710` passed package
  distribution and uploaded `native-win32-x64-msvc.zip` as artifact
  `10645563906` with SHA-256
  `beeba86bd2873e51b1c3090f6cab81fd8174866dabba0f846417ca56da351b01`.
  Aggregate-native `106391712172` was skipped.
- In the same corrected run, Ozone `106385419146`, Ozone/TiDB
  `106385419571`, and Ozone/FoundationDB `106385419306` passed their real
  restart/composition gates. Ozone compositions `106385419752` emitted
  `OZONE_SQLITE_CHUNKED_COMPOSITION_PASS`,
  `OZONE_PGLITE_CHUNKED_COMPOSITION_PASS`, `SUMMARY node-sdk pass=7 skip=1
  fail=0`, and successful cleanup, but its required 1,000-IOPS performance
  gate failed at 48.07 IOPS for split SQLite/R2 and 83.25 IOPS for split
  PGlite/R2; each provider completed all 400 iterations and 1,200 lifecycle
  operations successfully. The artifact-preserved benchmark is
  `10646598227`.
- FoundationDB/RustFS `106385419421` reached the durable cluster and RustFS
  chunk/reopen checks, then failed only at the configuration-driven native
  FUSE test because the test-generated absolute object key was rejected as
  an unsafe R2 state key. The relative-key correction is published as
  `d07c711`; this run predates that fix and is not a current-head provider
  acceptance result.
- Intel Node `106385419824` passed build, native NFS, virtual-fs, upstream,
  and HTTP differential stages but failed `scripts/test-http-early-rejection.mjs`
  with `Error: write EPIPE` from the underlying request socket before its
  exact PGlite step; that step and its benchmark are therefore skipped. The
  focused socket guard is published as `41d5514`.
- Current-head provider run `35617849558` moved the FoundationDB/RustFS
  configuration-driven test past the unsafe object-key fixture and durable
  chunk/reopen checks, then exposed the privileged Linux FUSE mount boundary:
  the driver mode's permission bits were incorrectly included in `rootmode`,
  so `mount(2)` returned `EINVAL`. Published fix `a55bd64` masks that value to
  `S_IFMT`, with a Linux-only serialization regression assertion and a focused
  locked FUSE suite passing locally. The fresh qualification run
  `35619576365` is queued; no hosted requalification is claimed yet.
- Rootmode qualification run `35619576365` is excluded from current-candidate
  acceptance: Linux, ARM, macOS-latest, macOS-15-intel, and Windows Node jobs
  failed the same pre-PGlite `integrations/mount-rs-napi/test/distribution.mjs`
  export assertion (`./webdav` expected the branch's old `index.js` mapping,
  while the staged package contained `webdav.cjs`). The branch predates the
  concurrent package/export commits now present on `origin/main`; no exact
  PGlite or package acceptance was inferred. Current-head run `35620669973`
  from the combined published tip is the valid requalification.
- Current-head run `35620669973` is terminal overall failure but provides a
  complete current Node/package evidence packet. Linux `106402628455`, ARM
  `106402628499`, macOS-latest `106402628531`, and macOS-15-intel `106402628195`
  all passed the exact `Verify PGlite integration and restart recovery` and
  fragmented early-rejection steps. Each Node log retained PGlite and
  chunked-PGlite trace passes, SDK/CLI `fail=0`, and explicit R2/TiDB/RustFS
  credential/service skips. Intel completed its PGlite step at
  `2026-09-21T16:08:53Z` after the long trace and did not reproduce the
  historical socket EPIPE.
- The same run's Windows Node job `106402628382` passed distribution and
  uploaded `native-win32-x64-msvc.zip` as artifact `10648916857` with SHA-256
  `b939bb7007823f022e22af5c9457d85b9dd7d17633dca6c219f84a8fd8200388`.
  Aggregate-native `106412877853` downloaded five artifacts, passed
  `mount-rs N-API artifact aggregation: PASS`, validated all five native
  packages, and passed `mount-rs clean consumer install/smoke: PASS`.
- FoundationDB/RustFS job `106402628095` passed the corrected relative-key and
  privileged-rootmode path: `FOUNDATIONDB_RUSTFS_CHUNKED_PASS`, configured
  FUSE mount/reopen, `FOUNDATIONDB_CLI_PASS`, N-API bounded readdir,
  `FOUNDATIONDB_SERVICE_RESTART_PASS`, RustFS fault/restart/reopen markers,
  and owned cleanup. Ozone functional composition, restart, fault-window,
  and cleanup markers also passed, but its performance threshold did not:
  `ozone-compositions` `106402628025` measured 5.79 and 53.18 IOPS,
  `ozone-tidb` `106402628026` measured 14.28 IOPS, and
  `ozone-foundationdb` `106402628170` measured 32.74 IOPS, all against the
  hard 1,000 target. The retained artifacts are `10650071502`, `10650360893`,
  and `10649956621` respectively.
- The same run's unrelated `rust (ubuntu-latest)` job `106402628328` failed
  `clippy::collapsible_if` at `transports/mount-rs-fuse/src/mount.rs:1043`
  against the earlier candidate source `f54590c`. Current `origin/main`
  already contains the collapsed-if form in `5a7940b`; this historical failure
  is not promoted to a W04 provider failure, but a fresh post-tip full CI run
  is still required if full-workspace CI is part of release evidence.
- Companion Fault Injection run `35620669906` completed successfully on Linux
  `106402627672`, macOS `106402627530`, and Windows `106402627687`. The
  fault-injection test suites reported zero failures and strict package Clippy
  passed on all three platforms. This is controlled failure-behavior evidence,
  not a production backup/restore or rollback drill.
- Earlier provider requalification run `35615133570` is excluded because its
  source predated `4840347` and the ARM Node addon could not compile the
  newly required `drain_timeout` field. Fresh run `35617849558` is the
  current-head requalification from `41d5514`; while queued or in progress,
  it supplies no acceptance evidence.
- The isolated run is not globally green: Ozone jobs
  `106369155190`, `106369155571`, `106369155784`, and `106369155910` failed
  their real contract entry on stale `tests/ozone/Cargo.lock` under
  `--locked`; `foundationdb-rustfs` job `106369155935` reached real
  FoundationDB/RustFS composition but failed
  `cli_foundationdb_rustfs_config_binary_mounts_and_reopens` with
  `config-backed fuse did not mount; output: []`. These failures remain
  provider-scope blockers and are not converted to W04 PGlite passes.
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
   decision to GO; current run `35620669973`, aggregate job `106412877853`,
   Windows artifact `10648916857`, and its clean-consumer step satisfy the
   hosted qualification package gate. The production decision remains NO-GO
   for the independent persistence, provider, operations, and release gates.
5. Confirm the actual launch configuration's persistence, versioning,
   backup/restore, rollback, observability, limits, and ownership gates; keep
   R2/TiDB/RustFS skips explicit unless those providers are in launch scope.
6. [x] Publish the W04.2 and package-gate evidence with exact run/job links,
   run `git diff --check` and the relevant formatting check, commit the
   workflow/test/ledger chunk, and push it to `origin/main`.
7. If a hosted job fails, treat it as a new engineering chunk: capture the
   failure, patch the smallest evidence-backed root cause, run focused tests
   plus the relevant W04 gate, and commit/push before proceeding.
8. [x] Confirm the published N-API teardown fix in isolated candidate run
   `35610813385`: Windows Node `106369156167` reached `Verify Windows package
   distribution`, and aggregate job `106379759443` retained the package and
   clean-consumer results. Shared-main run `35609647646` was cancelled and is
   not counted.
9. [x] Confirm the published HTTP-oracle and provider-matrix lockfile fixes
   in isolated candidate run `35610813385`: all four Unix Node jobs completed
   their HTTP parity lanes and exact PGlite recovery steps; their provider
   matrices were `6/3/0`, `5/3/0`, and `12/2/0` pass/skip/fail, with R2 and
   TiDB/RustFS rows explicitly skipped.
10. [ ] Obtain or explicitly exclude the provider scope behind the Ozone
    composition performance failure and FoundationDB/RustFS configuration
    gate before a production release decision. A PGlite-only launch may
    exclude those services only after the release owner records that scope.
11. [x] Complete current-head run `35620669973` from candidate source
    `f54590c`; Linux, ARM, macOS-latest, and macOS-15-intel exact
    PGlite/restart steps, Windows packaging, aggregate-native, clean-consumer,
    and corrected FoundationDB/RustFS provider evidence all passed. The run is
    still not a production qualification because its configured Ozone IOPS
    rows failed and its full-tree Ubuntu Rust job used a pre-tip Clippy lint.
    Runs `35617849558` and `35619576365` remain partial/stale diagnostics.
    Queued, skipped, cancelled, partial, or pre-fix evidence does not count.
12. [ ] The corrected FoundationDB/RustFS configuration-driven mount now
    passes in job `106402628095`, but the Ozone 1,000-IOPS threshold remains a
    production launch decision. Retain the functional-pass/performance-fail
    split until a provider/release owner explicitly excludes Ozone from launch
    scope or accepts/remediates the threshold.

## Provisional remaining effort and blockers

| Category | Estimate | Classification |
| --- | ---: | --- |
| Hosted macOS log inspection after runners start | Complete, ~0.5h actual | Engineering/verification |
| Tracker/dashboard closure, formatting, commit, and push | Complete, ~0.5h actual | Engineering/documentation |
| Potential remediation if hosted macOS exposes a regression | 2–6h | Engineering contingency |
| Current Intel socket EPIPE requalification | Complete; current run `35620669973` exact early-rejection and PGlite/restart steps passed | Hosted/native gate; 0h remaining |
| GitHub Actions queue delay | Resolved for W04.2 by isolated qualification branch; future shared-main churn remains external | External blocker; not engineering time |
| R2/TiDB local credential/service skips | Unknown | External provider prerequisites; not W04.2's macOS/Linux closure criterion |
| Native artifact aggregation and clean consumer smoke | Complete in current run `35620669973`; 0.25h release-record retention remains | Hosted/package gate |
| Windows Node `EBUSY` cleanup remediation and hosted rerun | Complete; 0h remaining | Engineering plus hosted/native gate |
| Cross-platform `NoSuchUpload` HTTP-parity lane | Complete in isolated candidate; 0.25h release-record retention remains | Hosted/provider boundary; all four Unix Node HTTP parity lanes passed in `35610813385`; provider-scope failures are separately recorded |
| Privileged Linux FUSE `rootmode` correction and hosted confirmation | Complete; current run `35620669973` passed configured FUSE mount/reopen and restart markers | Engineering fix plus hosted native/provider evidence; `a55bd64` masks `rootmode` to `S_IFMT`, `aa3dae3` separates privileged helper metadata, and `9ef982d` aligns the assertion |
| Ozone composition IOPS threshold | Open; current run measured 5.79 and 53.18 in `ozone-compositions`, 14.28 in Ozone/TiDB, and 32.74 in Ozone/FoundationDB versus target 1,000; 0.5–2h to benchmark/remediate or obtain scope decision | External provider/performance gate; functional lifecycle and cleanup passed |
| FoundationDB/RustFS configuration-driven FUSE requalification | Complete for current hosted path; 0h engineering remaining, production provider scope still open | Provider/native gate; current job `106402628095` passed relative-key, configured mount/reopen, N-API, service restart, and cleanup evidence |
| Credential-free PGlite-only launch-config policy | Complete locally; hosted policy output pending; 0.25h evidence retention remaining | Engineering implementation plus hosted policy gate; no provider connection or secret was used |
| Local backup/restore/rollback rehearsal | Complete; 0.5–1h harness implementation complete | Production execution remains external; local copy is not a production backup or RPO/RTO result |
| Full post-tip repository CI confirmation | Open; current candidate's Ubuntu Rust failure was pre-tip `clippy::collapsible_if`, already corrected in `5a7940b`; 0.25–1h hosted wait and evidence review | Hosted/full-repository release gate |
| Production persistence/backup/rollback and version policy | 1–2h | Engineering plus deployment-owner decision |
| Runbook, observability, limits, and operational ownership | 1–2h | Production operations gate; review/ownership dependent |

Best-case remaining active engineering for the already-closed W04.2 and
isolated Node/package qualification is approximately **0–1h** for evidence
retention and launch-scope recording. Production rollout readiness still adds
approximately **3–8h** for persistence/rollback, provider-scope decision or
provider remediation, and operational/release gates, excluding external
provider setup, owner decisions, and hosted runner time. The Windows Rust
long-symlink, N-API cleanup, HTTP parity, lockfile, Node packaging, and clean
consumer gates are no longer open blockers in the isolated candidate.

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
| 2026-09-22 00:10–00:45 AEST | Ran the isolated production-candidate qualification from source `d55080550cd4`, waited for the Intel macOS PGlite step to finish, and inspected every terminal Node/package log plus the provider failures. | Run `35610813385`: Linux, ARM, macOS-latest, macOS-15-intel, Windows Node, and aggregate-native/package/clean-consumer gates passed. Intel PGlite/restart took 7m02s and passed. The overall run failed only in provider jobs: four Ozone variants hit the stale Ozone lockfile and FoundationDB/RustFS failed the config-backed FUSE mount assertion. | Hosted qualification/production boundary, ~0.6h; provider setup and runner time external |
| 2026-09-22 00:45–01:18 AEST | Inspected corrected run `35615617217`, separated the functional Ozone/TiDB/FoundationDB passes from the Ozone 1,000-IOPS threshold failure, identified the pre-`d07c711` unsafe R2 state-key fixture, and diagnosed Intel's socket-level EPIPE before PGlite. Added and published the socket guard as `41d5514`, then started fresh isolated run `35617849558` from the published tip. | Linux, ARM, and macOS-latest exact PGlite/restart steps passed; Intel failed before PGlite, aggregate-native was skipped, Ozone composition remained performance-failed, and the current-head run is queued/in progress with no acceptance claim. | Engineering/hosted diagnosis/documentation, ~0.55h; provider and runner time external |
| 2026-09-22 01:18–01:33 AEST | Inspected completed logs from `35617849558`, separated its passing durable FoundationDB/RustFS chunk/reopen evidence from the privileged config-mount `EINVAL`, traced the error to serializing full driver permissions as Linux FUSE `rootmode`, added the mask and Linux-only regression assertion, ran formatting/diff/focused locked FUSE checks, rebased onto concurrent main commits, and published `a55bd64`. Started rootmode-fix qualification CI `35619576365` plus Fault Injection `35619576289`. | The focused local suite passed; both new hosted runs are queued at snapshot time, so no provider or production acceptance is claimed. Production remains NO-GO. | Engineering/hosted diagnosis/documentation, ~0.35h; hosted provider and runner time external |
| 2026-09-22 01:33–01:41 AEST | Reviewed concurrent privileged-FUSE metadata commit `aa3dae3`, corrected its Linux-only expected `rootmode` value in `9ef982d`, ran macOS focused tests, Linux-target `cargo check --tests`, formatting, and diff checks, then pushed the correction. Inspected rootmode run `35619576365` and found all Node jobs stopped at a stale package/export assertion before PGlite; published a fresh combined-tip branch and started CI `35620669973` plus Fault Injection `35620669906`. | The concurrent metadata behavior and assertion now agree with the rootmode mask; current-head requalification is queued and no stale-run acceptance was promoted. Production remains NO-GO. | Engineering/hosted diagnosis/documentation, ~0.3h; hosted provider and runner time external |
| 2026-09-22 01:41–02:00 AEST | Monitored current-head run `35620669973` with bounded waits, inspected live Intel progress, and retrieved terminal logs as the four Node jobs completed. | Linux, ARM, macOS-latest, and macOS-15-intel exact PGlite/restart steps passed; Intel completed after the long trace without EPIPE. Windows package, aggregate-native, clean-consumer, and FoundationDB/RustFS gates also passed. | Hosted qualification, ~0.35h engineering; runner time external |
| 2026-09-22 02:00–02:10 AEST | Inspected the terminal aggregate, artifact, provider, and Fault Injection logs; separated functional Ozone/restart passes from the hard IOPS failures and recorded the unrelated pre-tip Ubuntu Rust Clippy failure. | Ozone thresholds were 5.79/53.18, 14.28, and 32.74 versus 1,000; Fault Injection passed on Linux/macOS/Windows; current artifact `10648916857` and SHA-256 were retained. | Hosted evidence/production boundary, ~0.25h engineering; provider runner time external |
| 2026-09-22 02:10–02:15 AEST | Rebased the clean checkout onto current `origin/main` (`2aaae52`) and expanded this ledger with the current qualification packet, artifact provenance, provider blockers, fault evidence, production actions, estimates, and session time. | Ledger is ready for formatting/diff verification and publication; production decision remains NO-GO. | Engineering/documentation, ~0.25h |
| 2026-09-22 02:15–02:27 AEST | Implemented the credential-free W04 PGlite-only launch-config policy, added positive and fail-closed negative fixtures, wired the policy into CI, documented its boundary in the rollout runbook, and ran local syntax, policy, CLI `--check`, formatting, and diff checks. | Positive policy and CLI parser checks passed; non-durable and inline-secret fixtures were rejected; hosted CI policy output and deployment-specific persistence/backup/rollback evidence remain open. | Engineering/documentation, ~0.35h; hosted CI and deployment-owner review external |
| 2026-09-22 02:27–02:40 AEST | Extended the disk-backed PGlite lifecycle harness with a quiesced isolated backup restore and bad-release rollback rehearsal; ran the focused lifecycle file with both ignored tests. | Both tests passed, including `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS isolated_restore=true candidate_removed=true`; this is local supporting evidence only and does not close the production backup/restore, RPO/RTO, or owner gates. | Engineering/verification, ~0.35h; production backup and deployment execution external |

## Publication note

This ledger revision records the W04.2 closure, the terminal current-candidate
Node/package evidence from run `35620669973`, the exact Windows artifact and
clean-consumer result, corrected FoundationDB/RustFS/rootmode qualification,
the cross-platform Fault Injection pass `35620669906`, and the retained Ozone
performance artifacts and thresholds. It also preserves the stale
pre-PGlite distribution failure in `35619576365` and the pre-tip Ubuntu Rust
Clippy failure in `35620669973`; neither is silently promoted to a current
W04 code failure. This revision also records the new credential-free PGlite-only
launch-config policy and its local positive/negative evidence; hosted policy
output is still pending. It also records the local disk-backed
`PGLITE_BACKUP_RESTORE_ROLLBACK_PASS` rehearsal; that filesystem copy is not a
production backup or rollback approval. It is reviewed with `git diff --check`
and the relevant formatting check, then committed and pushed to `origin/main`. It does
**not** approve production: Ozone launch scope/performance, deployment-specific
persistence/backup/rollback, observability, runbook execution, ownership,
post-tip full CI, and release approval remain open.
