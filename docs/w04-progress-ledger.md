# W04 PGlite progress ledger

This ledger tracks the W04 workstream in `WORK_TRACKER.md`. It separates
implementation, deterministic/local qualification, hosted/native/provider
acceptance, and production-rollout readiness. Percentages and time estimates
are provisional: a queued or in-progress job is not a pass, a demo is not a
production acceptance, and local evidence does not substitute for the hosted
macOS/Linux gate required by W04.2.

## Live current status (authoritative)

The historical snapshots below remain part of the audit trail. This section is
the current status for the active production push and must be read first when a
later mainline push has made an older snapshot stale.

| Work item | Status | Completion | Evidence | Remaining action | Provisional estimate | External blocker or gate |
| --- | --- | ---: | --- | --- | ---: | --- |
| FUSE bounded-unmount lifecycle chunk | Published implementation chunk; hosted Linux/native confirmation pending | 100% code/local checks / 0% hosted-kernel confirmation | Published candidate `1bdd6adf` adds the result-publication race fix, bounded session-stop grace, and a narrow confirmed-`EBUSY` forced-detach handoff. The synchronized tree's host `mount-rs-fuse` suite passed 14/14; `cargo fmt --all -- --check` and `git diff --check` passed. | Inspect Ubuntu Rust and native-FUSE logs from the replacement exact-tip run; keep this separate from production acceptance until the hosted kernel behavior passes. | 0.25–0.75h engineering; runner wait external | Hosted Linux kernel/FUSE gate |
| N-API clean-build declaration normalization | Published implementation; current-tip hosted confirmation pending | 100% local implementation/build/typecheck / 0% current-tip hosted confirmation | Published tree `819c663e` retains the P9 normalization from `9cb2eef1`, preserving `P9SessionStats.messages` as `Map<string, number>` after a clean `pnpm --dir integrations/mount-rs-napi build`; the release build completed, generated declarations retained `Map<string, number>`, and the focused typecheck passed. The canceled run below is explicitly excluded because its `0c31c167` tree still materialized `Record<string, number>`. | Require the replacement hosted Node matrix to complete the generated-type and exact W04 recovery steps on every advertised platform. | 0.25–0.5h engineering; hosted wait external | Hosted package/Node matrix |
| PGlite production policy and local recovery rehearsal | Local controls green; deployment gate open | 100% repository-local control / 0% deployment approval | `verify-w04-pglite-production-config.mjs` passed the positive fixture and rejected durable=false, missing TTL, and inline-secret fixtures. `pglite_server_lifecycle` passed 2/2 ignored tests, including `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS isolated_restore=true candidate_removed=true`; the measured local test time was 7.74s. These are shape/control-flow evidence only. | Bind the approved configuration to the real persistent volume and execute encrypted backup/isolated restore/rollback with measured RPO/RTO, retention, ownership, and release approval. | 1–2h engineering plus deployment-owner execution | Production persistence, backup, rollback, and ownership gates |
| Artifact/package production rehearsal | Local fixture aggregation green; all-target clean consumer remains hosted-dependent | 100% local aggregation/typecheck / 0% all-target artifact acceptance | `node integrations/mount-rs-napi/test/distribution-aggregate.mjs` passed; generated typecheck and diff checks passed. The local checkout has only the Darwin ARM native binary, so the five-platform clean-consumer path cannot be claimed from this workspace. | Retain hosted artifact digests, run aggregate-native, and complete the clean-consumer install/smoke on the exact published candidate. | 0.25–0.75h engineering; hosted artifact wait external | Native artifact matrix and package gate |
| WebDAV provider-network cleanup race | Published test hardening; hosted confirmation pending | 100% local implementation/stress/full-suite / 0% hosted confirmation | Published as `819c663e`: temporary-tree cleanup now retries only the final `rm` after provider shutdown. Five fresh concurrency-64 NodeFs/SQLite runs passed; the prior 20-run stress packet also passed after the fix; the full pinned-oracle N-API suite passed all runnable tests. | Confirm the cleanup behavior in the next exact-tip hosted Node matrix and retain the job log as platform evidence. | 0.25–0.5h engineering; hosted wait external | Hosted macOS/Linux/Windows Node matrix |
| Failed-publication shutdown lease release | Published implementation; local and fault-injection workflow green; native-FUSE confirmation pending | 100% implementation/local regression / 0% current hosted full-matrix confirmation | Published rebased commit `3de49e33` skips unsafe pending-atime publication after a failed-closed mutation so shutdown can release the provider lease. The focused regression passed, the full `mount-rs-chunked` library suite passed 21/21, package Clippy passed with `-D warnings`, and Fault injection run [35679778367](https://github.com/andymac4182/mount-rs/actions/runs/35679778367) passed all macOS-latest, Ubuntu-latest, and Windows-latest jobs. | Confirm the fix through the current full CI/native-FUSE run; retain the earlier native-FUSE failure as the motivating diagnostic and do not promote local or fault-wrapper evidence to production acceptance. | 0.5h engineering complete; hosted wait external | Hosted Linux FUSE and current full qualification |
| Replacement exact-tip qualification run | Terminal cancelled; diagnosis complete; no acceptance promoted | 0% terminal current-tip matrix | Run [35675591961](https://github.com/andymac4182/mount-rs/actions/runs/35675591961) targeted published head `0c31c167024874f68dac1b04b4fd0aceb343c347` and ended **cancelled** after native FUSE [106581350258](https://github.com/andymac4182/mount-rs/actions/runs/35675591961/job/106581350258) exceeded its configured timeout. The four Unix Node jobs and Windows Node [106581349897](https://github.com/andymac4182/mount-rs/actions/runs/35675591961/job/106581349897) failed before W04 recovery at `test/types.test.ts(655,9)` with `Record<string, number>` versus `Map<string, number>`; no exact recovery-step PASS is claimable. | This run is permanently excluded; use current published-head run `35679778373` for the next hosted qualification and inspect every Node, native-FUSE, aggregate/package, provider, and artifact result. | 0.5–1h engineering; hosted runner/provider wait external | GitHub-hosted runner scheduling and current package/native/provider gates |
| Previous exact-tip qualification run | Node recovery matrix complete; mixed native/provider result; aggregate still queued; no full acceptance promoted | 100% four-Node exact recovery steps / 0% full qualification | Run [35678123095](https://github.com/andymac4182/mount-rs/actions/runs/35678123095) targets head `0b6e8c4f01ebdd9254c7d6c61595628e0ce4a824`. Ubuntu [106588864184](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864184), ARM [106588864228](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864228), macOS-15-intel [106588864181](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864181), and macOS-latest [106588864034](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864034) all completed successfully and their exact `Verify PGlite integration and restart recovery` steps passed. Ubuntu Rust [106588864166](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864166) and Windows Node [106588864128](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864128) also passed; native FUSE [106588864049](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864049) failed at the metadata-publish/WAL cleanup shutdown path, Ozone/TiDB [106588863658](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588863658), Ozone compositions [106588863893](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588863893), FoundationDB/RustFS [106588863916](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588863916), and Ozone/FoundationDB [106588863931](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588863931) failed provider/composition gates, and aggregate-native [106594162715](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106594162715) remains queued. | Inspect the aggregate job if it materializes, but require the new current-head run below for the published shutdown fix; retain provider failures as production blockers. | 0.25h diagnosis complete; hosted scheduler/provider wait external | Native FUSE, provider performance/composition, aggregate artifact, and production-owner gates |
| Current published-head qualification | Pending; no jobs materialized; no acceptance promoted | 0% current-head hosted matrix | Push-triggered CI run [35679778373](https://github.com/andymac4182/mount-rs/actions/runs/35679778373) targets published head `3de49e333ee9f42c51c1a6e304e253608862e824` and remains pending. Exact-tip Fault injection run [35679778367](https://github.com/andymac4182/mount-rs/actions/runs/35679778367) completed successfully across macOS-latest [106594017507](https://github.com/andymac4182/mount-rs/actions/runs/35679778367/job/106594017507), Ubuntu-latest [106594017625](https://github.com/andymac4182/mount-rs/actions/runs/35679778367/job/106594017625), and Windows-latest [106594017662](https://github.com/andymac4182/mount-rs/actions/runs/35679778367/job/106594017662); the independent W04 policy run [35679778395](https://github.com/andymac4182/mount-rs/actions/runs/35679778395) also passed. | Wait for the current-head CI jobs to materialize; inspect all four Node exact recovery steps plus native FUSE, aggregate/package, provider, observability, and artifact logs before changing acceptance or production status. | 0.5–1h engineering; hosted runner/provider wait external | GitHub-hosted scheduling and current native/package/provider/production gates |
| Production rollout decision | NO-GO | 0% final release approval | The demo, historical W04.2 evidence, local policy/recovery rehearsal, and local artifact checks remain supporting evidence only. Deployment-specific persistence/backup/rollback, current published artifact/package evidence, provider scope/performance, observability/runbook execution, named ownership, and release approval remain open. | Keep the rollout decision NO-GO until the replacement exact-tip evidence and production matrix are signed by deployment, operations, provider, and release owners. | 3–8h engineering plus owner/provider execution | Provider IOPS, deployment environment, operational owners, release authority |

## Snapshot

| Field | Current value |
| --- | --- |
| Live qualification state | Current push-triggered CI run [35679778373](https://github.com/andymac4182/mount-rs/actions/runs/35679778373) targets published `3de49e333ee9f42c51c1a6e304e253608862e824` and is **pending with no jobs materialized**. The previous run [35678123095](https://github.com/andymac4182/mount-rs/actions/runs/35678123095) completed all four Node exact recovery steps successfully but has a failed native/provider mix and a queued aggregate job. No current-head or full-run acceptance is claimed. |
| Current recovery state | Previous exact-tip run [35678123095](https://github.com/andymac4182/mount-rs/actions/runs/35678123095) completed Ubuntu [106588864184](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864184), ARM [106588864228](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864228), macOS-latest [106588864034](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864034), and macOS-15-intel [106588864181](https://github.com/andymac4182/mount-rs/actions/runs/35678123095/job/106588864181) with exact PGlite/restart PASS, plus Ubuntu Rust and Windows Node PASS. Its native-FUSE job failed on metadata-publish/WAL shutdown cleanup, Ozone/TiDB and Ozone compositions failed provider/composition gates, and aggregate-native remains queued. Current-head CI [35679778373](https://github.com/andymac4182/mount-rs/actions/runs/35679778373) is pending with no jobs materialized; this is mixed/pre-fix evidence only. |
| Snapshot base revision | `3de49e333ee9f42c51c1a6e304e253608862e824` (published failed-publication shutdown lease-release fix, rebased onto the latest `origin/main`) |
| Ledger publication | This revision records the stale-run recovery, the published WebDAV and Ozone test chunks, the current qualification run, the explicit split-store lease-TTL control, the current package/provider evidence, and the remaining production blockers; it is published on `origin/main` with the exact commit recorded in Git history |
| Current-head local evidence revision | `3de49e33` includes the published WebDAV cleanup retry and failed-publication shutdown lease-release fix; the focused shutdown regression passed, the full `mount-rs-chunked` library suite passed 21/21, package Clippy passed with `-D warnings`, host-safe FUSE tests remain 14/14, the PGlite policy/lifecycle rehearsals and N-API artifact aggregation remain green, and formatting/diff checks passed. |
| Latest synced verification revision | `113724487e9efc172ab69254d995377cfcfab296` (workspace test and scoped Clippy evidence; unrelated W26/TiDB/CLI changes are included) |
| Latest full W04 gate revision | `6d59d204af80c883bf47a59ffb5a4b77829f8` (last complete exact-pinned-oracle `scripts/test-pglite.sh` run before the current failed-publication shutdown fix; it exited 0) |
| Latest published repository revision | `3de49e33` (`fix(chunked): release lease after failed publication`), pushed to `origin/main`; the next documentation chunk records the exact hosted fault-injection and pending full-CI evidence. |
| Latest accepted W04 qualification evidence | Manual-dispatch run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) at head `d2db74dd1c647ef5b6eebcb0790be4bdd87b3c3f`, terminal overall **failure** only on provider/performance rows. All four Node jobs passed the exact `Verify PGlite integration and restart recovery` step: ARM [106450522980](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522980), Linux [106450522757](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522757), macOS-latest [106450522868](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522868), and macOS-15-intel [106450522845](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522845). The macOS logs recorded PGlite/chunked-PGlite trace PASS, `providersFailed: 0`, and `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`; storage artifacts were `10656542212` (macOS-latest, digest `45307dd2be19b2621a72f9661dd5f6cbe3f2a112d920835d321e1ffc9a843473`) and `10656562978` (macOS-15-intel, digest `b014760610218685ebf9aef58203fc126604113a7c8eb1634d48a09013ff3386`). Ubuntu Rust [106450522426](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522426) passed its full workspace tests, including the repaired FUSE suite; aggregate-native [106456609397](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106456609397) passed artifact aggregation and clean-consumer smoke; Windows Node [106450522875](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522875) uploaded `native-win32-x64-msvc.zip` artifact `10656486511` with digest `380a7f936477db1948cfac9089d0ccc1170e787f9b4500537bcc0581122e1c58`. Ozone compositions [106450522623](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522623) measured 47.48 and 57.85 IOPS, Ozone/TiDB [106450522541](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522541) measured 15.37, and Ozone/FoundationDB [106450522420](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522420) measured 32.86, all below the hard 1,000 target. TiDB/RustFS [106450522915](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522915) failed its native Node mount assertion after the provider soak; W26 evidence [106454477027](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106454477027) correctly failed because no `OZONE_IOPS_PASS` marker existed. |
| Snapshot time | 2026-09-22 12:35 AEST |
| Prior current-tip requalification | Manual run [35666527609](https://github.com/andymac4182/mount-rs/actions/runs/35666527609) was dispatched at head `9b2dabd7bf3f4bf985970c576d71193ac763a49c`, before the subsequently published Ozone cleanup fix `23c0ba7e`; its ARM/Windows partial evidence and provider IOPS failures are retained historically and do not qualify the current published head. |
| Stale-run recovery | Run [35664315098](https://github.com/andymac4182/mount-rs/actions/runs/35664315098) remained queued after its macOS-latest runner never materialized; it was explicitly canceled before this snapshot. Its terminal logs were retained for diagnosis only: macOS-15-intel failed WebDAV `PROPFIND response` before PGlite because native/oracle mtimes differed, Ozone/TiDB and Ozone/FoundationDB failed the test cleanup path with `FsError { code: Enoent, syscall: Some("block object") }`, and Ubuntu Rust failed the pre-`bc292709` forced-unmount tests. None of these queued, partial, or pre-fix results is acceptance evidence. |
| Latest isolated current-main qualification | Manual run [35654281185](https://github.com/andymac4182/mount-rs/actions/runs/35654281185) was dispatched at head `595c5c852cd6664347a809d2a82a46e59f2b19de` and ended terminal **failure**. ARM [106513916713](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513916713), Linux [106513917128](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513917128), macOS-latest [106513917371](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513917371), and macOS-15-intel [106513917006](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513917006) all failed before the exact recovery step at `s3/multipart-complete-signed-trailer`, with TypeScript ETag `483eafaaf0c86330cf62199b2d5cb4d5-1` and Rust ETag `22801ba05185c8cc8aad97c7b9d1ed6c-1`; all four `Verify PGlite integration and restart recovery` steps were skipped. Ubuntu Rust [106513916656](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513916656) failed the forced-unmount deadline test at `404.004757ms`; native FUSE [106513917115](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513917115) hit its 15-minute timeout and was cancelled during rootless operations. Ozone compositions [106513916924](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513916924) measured 92.53/84.02 IOPS, Ozone/TiDB [106513916852](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513916852) measured 5.70, and Ozone/FoundationDB [106513916479](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106513916479) measured 34.07, all below 1,000 despite cleanup markers; W26 [106517960256](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106517960256) failed closed on missing `OZONE_IOPS_PASS`. Windows/package, macOS Rust/NFS/WebDAV, TiDB/RustFS, and observability jobs passed; aggregate-native [106519603529](https://github.com/andymac4182/mount-rs/actions/runs/35654281185/job/106519603529) was skipped. |
| Latest Windows peer-fault diagnosis | Pre-fix manual run [35650347479](https://github.com/andymac4182/mount-rs/actions/runs/35650347479), head `0d06abb10899f307ce83cd5fa198bab9c5f156a9`, Windows job [106500944953](https://github.com/andymac4182/mount-rs/actions/runs/35650347479/job/106500944953) built successfully but failed the parity step with the exact `N-API server integration (S3 peer-fault callback (+19754ms)) timed out after 20000ms` marker. This is the evidence for the test race, not a production acceptance pass. |
| Current fixed-tip hosted qualification | Manual run [35651055621](https://github.com/andymac4182/mount-rs/actions/runs/35651055621) was dispatched from head `3589a142964da53e962239fc2cee6480a7fb6e93` (which contains `093565d9`) and ended in terminal **failure**. Windows job [106503290630](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290630) completed successfully at `2026-09-21T20:31:35Z`: its exact N-API server integration, S3 restart/multipart recovery, structural lifecycle, package distribution, clean-consumer, and artifact aggregation checks passed; PGlite/R2/FDB/native-mount rows remained explicit skips where prerequisites were absent. ARM Node [106503290868](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290868), Linux Node [106503290885](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290885), macOS-latest Node [106503290828](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290828), and macOS-15-intel Node [106503290797](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290797) all failed at `s3/multipart-complete-signed-trailer`: the TypeScript ETag was `483eafaaf0c86330cf62199b2d5cb4d5-1` and Rust returned `22801ba05185c8cc8aad97c7b9d1ed6c-1`. This follows the W01/S3 multipart-finalization marker change and is not evidence against the W04 peer-fault fix, but it prevents current-tip Node acceptance. Ubuntu Rust [106503290545](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290545) failed the FUSE forced-unmount deadline test at `404.993546ms`; native FUSE [106503290699](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290699) had two native-unmount timeouts; Ozone compositions [106503290345](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290345), Ozone/TiDB [106503290375](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290375), and Ozone/FoundationDB [106503290535](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290535) failed their configured provider/performance compositions; TiDB/RustFS [106503290924](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290924) failed the native Node CLI mount with `EAGAIN: resource temporarily unavailable, TiDB acquire writer`; aggregate-native [106507082737](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106507082737) was skipped; and W26 evidence [106508142092](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106508142092) failed closed because `OZONE_IOPS_PASS` was missing. No full-run or release acceptance is claimed. |
| Tracker section | `WORK_TRACKER.md` § W04 — PGlite |
| Checklist completion | **100%**: 8 of 8 W04 checklist items are checked; W04.2 hosted acceptance is closed |
| Implementation/local qualification | **Complete for the recorded packet**; the fresh post-fix oracle-enabled W04 gate passed at `6d59d20`, the focused Linux-container FUSE suite passed 14/14 on `d2db74dd`, the published `113a1fbc` TTL chunk passed its focused CLI/SDK/provider-matrix checks, and the N-API peer-fault race was corrected in `093565d9`. After a shared-target release rebuild, the full pinned-oracle `pnpm --dir integrations/mount-rs-napi test` passed all runnable tests; PGlite/R2/FDB/native-mount rows remained explicit skips where prerequisites were absent. The dedicated W04 PGlite-only launch-config policy and local backup/rollback rehearsal remain green with their explicit shape-only/local boundaries. |
| Hosted/native/provider acceptance | **W04.2, the recorded current Node/package gates, and the fixed-tip Windows peer-fault/package sub-gate are complete; the fixed-tip full qualification is terminally mixed and production remains open**: run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) passed all four Node exact PGlite/restart steps, Windows package distribution, five-package aggregation, and clean-consumer smoke. The pre-fix diagnostic Windows job [106500944953](https://github.com/andymac4182/mount-rs/actions/runs/35650347479/job/106500944953) failed only at the S3 peer-fault callback phase; fixed-tip Windows job [106503290630](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290630) completed successfully through the repaired server, restart, structural, package, and artifact checks. The enclosing fixed-tip run [35651055621](https://github.com/andymac4182/mount-rs/actions/runs/35651055621) later ended in terminal failure: all four Unix Node jobs failed the same `s3/multipart-complete-signed-trailer` ETag mismatch from the newer W01/S3 finalization-marker path; Ubuntu Rust/native-FUSE and provider jobs also failed, aggregate-native was skipped, and W26 failed closed on its missing marker. FoundationDB/RustFS [106450522539](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522539) passed durable chunk/reopen, service restart, N-API, VFS restart, and cleanup markers. Ozone lifecycle checks completed and cleanup passed, but the hard IOPS rows failed; TiDB/RustFS also failed a native Node mount assertion. No full-run or release pass is claimed. |
| Production rollout decision | **NO-GO**: the demo was successful and the credential-free PGlite-only config policy now has terminal hosted evidence for its bounded, explicit lease-TTL control, but production readiness still requires artifact, deployment durability, operational, provider-scope, ownership, and rollback gates |
| Current external blocker | The pre-fix qualification [35674630831](https://github.com/andymac4182/mount-rs/actions/runs/35674630831) was canceled after ARM exposed the generated-declaration mismatch; the replacement exact-tip run has not yet been dispatched from the next published ledger commit. Production remains NO-GO pending current hosted evidence plus deployment-specific persistence/backup/rollback, provider scope/performance, observability/runbook/ownership, and release approval. |
| Historical external blocker | The fixed-tip run [35651055621](https://github.com/andymac4182/mount-rs/actions/runs/35651055621) exposed the W01/S3 `s3/multipart-complete-signed-trailer` ETag mismatch before the W04 recovery step; the published multipart allocation-order fix is recorded in the subsequent qualification history. Ubuntu Rust/native-FUSE and Ozone/TiDB/RustFS provider failures remain explicit hosted gates; the stale Ozone cleanup and WebDAV metadata failures above were repaired but still require current-tip requalification. |

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
| Current recovery-run hosted evidence | Blocked — pre-fix run canceled, replacement pending | 100% local repair / 0% current-tip hosted matrix | Qualification [35674630831](https://github.com/andymac4182/mount-rs/actions/runs/35674630831) at head `ba20d29d` failed ARM Node [106578408127](https://github.com/andymac4182/mount-rs/actions/runs/35674630831/job/106578408127) at `TS2740` after clean-build regeneration; it was canceled as pre-`d0a67b22` evidence. Local current-tree clean release build, generated typecheck, policy, lifecycle rehearsal, and artifact aggregation are green. | Dispatch and inspect a replacement from the next published `origin/main` tip. Require all four Node exact recovery steps, Ubuntu Rust/native FUSE, package/consumer, provider, and aggregate evidence. | 0.5–1h evidence work; hosted runner/provider wait external | Hosted/provider/package gate |
| W04 recovery chunks and hosted requalification control | Implementation chunks published; exact-tip requalification pending | 100% local repair / 0% current-tip hosted requalification | WebDAV session parity (`9b2dabd7`), Ozone cleanup deduplication (`23c0ba7e`), FUSE busy-unmount (`1bdd6adf`), and clean-build P9 stats normalization (`d0a67b22`) are published. Local focused checks passed; stale qualification `35664315098` and pre-fix qualification `35674630831` are excluded from acceptance. | Dispatch the replacement run, require exact early-rejection and PGlite/restart PASS on Linux, ARM, macOS-latest, and macOS-15-intel, then inspect Ubuntu Rust/native-FUSE, package, aggregate, and provider gates; preserve NO-GO for partial or queued evidence. | 0.5–1h engineering; hosted runner/provider wait external | Release engineering / hosted gate |
| W04 implementation packet and deterministic local qualification | Complete — implementation and accepted historical candidate green; current-tip recovery pending | 100% implementation / 100% accepted historical Node/package/Rust qualification / 0% current recovery run | The packet includes the socket server, cleanup/fencing fixes, version metadata, mount-free VFS, native hosting tests, the published fixture-shutdown EPIPE fixes `bdfcb11` and `41d5514`, the privileged-FUSE rootmode correction, test-framing fix `d2db74dd`, split-store `lease_ttl_ms` enforcement `68c087d1`, WebDAV metadata pin `9b2dabd7`, and Ozone cleanup deduplication `23c0ba7e`; host-safe FUSE tests passed 14/14, the Ozone R2 suite passed 18/18, and accepted run `35635114595` passed all four Node exact PGlite/restart steps plus the Ubuntu Rust workspace gate. | Finish the current recovery run from the published tip, then decide launch provider scope and complete deployment-specific persistence/rollback and operations evidence; provider IOPS and TiDB/RustFS native-mount failures remain external/performance/provider gates. | 0.5–1.5h engineering; hosted/provider wait external | Engineering plus hosted gate |
| Hosted Linux Node acceptance | Complete — accepted W04.2 evidence; fresh current-tip confirmation pending | 100% accepted historical gate / 0% fresh replacement run | Run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595), job [106450522757](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522757), completed successfully; the exact PGlite/restart and fragmented early-rejection steps passed, with PGlite-inclusive trace and explicit provider skips. | Inspect the Linux Node job in the replacement run; do not replace the accepted W04.2 record until the current candidate is terminal. | 0.1h engineering; hosted runner wait external | Hosted gate |
| Hosted macOS-latest Node acceptance | Complete — accepted W04.2 evidence; fresh current-tip confirmation pending | 100% accepted historical gate / 0% fresh replacement run | Run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522868), completed successfully at `2026-09-21T18:07:05Z`; the exact PGlite/restart and fragmented early-rejection steps passed, the log recorded `providersFailed: 0`, PGlite/chunked-PGlite trace PASS, `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`, and storage artifact `10656542212`. | Inspect the macOS-latest job in the replacement run; queued/in-progress evidence is not acceptance. | 0.1h engineering; hosted runner wait external | Hosted/native gate |
| Hosted macOS-15-intel Node acceptance | Complete — accepted W04.2 evidence; fresh current-tip confirmation pending | 100% accepted historical gate / 0% fresh replacement run | Run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595), job [106450522845](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522845), completed successfully at `2026-09-21T18:13:03Z`; the exact PGlite/restart step passed after the long Intel trace at `18:12:45Z`, early rejection passed, the log recorded `providersFailed: 0`, PGlite/chunked-PGlite trace PASS, `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`, and storage artifact `10656562978`; the historical socket EPIPE did not recur. | Inspect the macOS-15-intel job in the replacement run; require the exact early-rejection and PGlite/restart steps to pass before promoting current-tip evidence. | 0.1h engineering; hosted runner wait external | Hosted/native gate |
| Native package/artifact and consumer validation | Complete — current-tip candidate | 100% | Windows Node job [106450522875](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522875) uploaded `native-win32-x64-msvc.zip` as artifact `10656486511`, SHA-256 `380a7f936477db1948cfac9089d0ccc1170e787f9b4500537bcc0581122e1c58`. Aggregate job [106456609397](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106456609397) passed five-package artifact aggregation and `mount-rs clean consumer install/smoke: PASS`. | Retain the exact artifact/hash in the release record; production release remains NO-GO until persistence/rollback, provider scope, operations, ownership, and release approval close. | 0.1h evidence publication; 0h package implementation | Hosted/package gate |
| Windows structural N-API peer-fault/package requalification | Complete — implementation, local oracle, and fixed-tip hosted Windows gate | 100% implementation/local / 100% fixed-tip Windows hosted acceptance | Diagnostic run [35650347479](https://github.com/andymac4182/mount-rs/actions/runs/35650347479), Windows job [106500944953](https://github.com/andymac4182/mount-rs/actions/runs/35650347479/job/106500944953), reproduced the pre-fix `S3 peer-fault callback` timeout. Commit `093565d9` replaces the reset race with an explicit `resetAndDestroy()` path; the rebuilt full pinned-oracle N-API package suite passed locally. Fixed-tip run [35651055621](https://github.com/andymac4182/mount-rs/actions/runs/35651055621), Windows job [106503290630](https://github.com/andymac4182/mount-rs/actions/runs/35651055621/job/106503290630), completed successfully with the exact N-API server integration PASS, S3 restart/multipart recovery, structural lifecycle, package distribution, clean-consumer, and artifact aggregation checks. This closes the Windows peer-fault/package sub-gate only; it does not promote the partial overall run to release evidence. | None for the Windows sub-gate; retain the overall run's Linux/ARM/native-FUSE and provider failures as separate qualification gates. | 0h engineering; remaining hosted wait external | Hosted/native/package gate |
| Production-like persistence, restart, and version recovery | Hosted candidate qualification complete; current-tip dedicated hosted config policy and local backup/rollback rehearsal green; deployment evidence pending | 90% hosted qualification / 60% config-and-local rehearsal / 0% production drill | Run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) passed all four exact PGlite/restart steps and PGlite-inclusive five-seed/eight-backend traces; both macOS logs emitted `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS` and retained storage artifacts `10656542212` and `10656562978`. FoundationDB/RustFS [106450522539](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522539) passed durable chunk/reopen, service restart, VFS restart, and cleanup markers. The manually dispatched dedicated policy run [35642541696](https://github.com/andymac4182/mount-rs/actions/runs/35642541696), job [106475151000](https://github.com/andymac4182/mount-rs/actions/runs/35642541696/job/106475151000), completed successfully on current main head `6c80eccb`, emitted `W04_PGLITE_PRODUCTION_CONFIG_POLICY_PASS ... lease_ttl=explicit-bounded`, and rejected non-durable, inline-secret, invalid-TTL, and missing-TTL fixtures. The push-triggered run `35642363293` was cancelled before any job materialized and is excluded. These checks demonstrate configuration shape and local/hosted control flow, not the actual production deployment. | Bind the actual PGlite server data directory/configuration and version policy, and execute an approved encrypted backup/restore and rollback drill with measured RPO/RTO and owner approval. | 1–2h engineering plus deployment-owner wait | Engineering plus deployment decision |
| Provider and durability matrix | PGlite/FDB-RustFS functional paths green; Ozone performance and TiDB/RustFS native-mount gates open | 75% for qualified functional scope / 0% for unapproved providers | Current run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) passed FoundationDB/RustFS [106450522539](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522539), including durable chunk/reopen, service restart, VFS restart, N-API, and cleanup markers. Ozone lifecycle/composition checks completed and cleanup passed, but its hard performance rows failed: 47.48 and 57.85 IOPS in `ozone-compositions` [106450522623](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522623), 15.37 in Ozone/TiDB [106450522541](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522541), and 32.86 in Ozone/FoundationDB [106450522420](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522420), each versus 1,000. TiDB/RustFS [106450522915](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522915) failed a native Node mount assertion after a zero-error provider soak. | Obtain a release-owner decision that Ozone is out of launch scope or remediate/qualify the 1,000-IOPS requirement with provider owners; resolve or explicitly exclude the TiDB/RustFS native-mount scope; retain explicit R2/TiDB/RustFS credential/service skips where not configured. | 1–4h engineering/provider work plus hosted wait | External provider/performance gate |
| W04 provider-matrix lockfile reproducibility | Complete — implementation and isolated hosted qualification | 100% | Candidate run [35608472226](https://github.com/andymac4182/mount-rs/actions/runs/35608472226) exposed the stale lockfile after hosted PGlite subchecks passed. Commit `caa9324` adds the existing `futures-util` dependency to `tests/provider_matrix/Cargo.lock`; the exact locked command passes locally, and all five Node jobs in [35610813385](https://github.com/andymac4182/mount-rs/actions/runs/35610813385) completed their provider-matrix/PGlite steps successfully. The shared-main confirmation run [35609647646](https://github.com/andymac4182/mount-rs/actions/runs/35609647646) was cancelled, so it is not counted. | None for the W04 provider-matrix lockfile; Ozone has a separate stale `tests/ozone/Cargo.lock` gate if that provider enters launch scope. | 0h remaining | Engineering / reproducibility gate |
| Operational readiness: observability, runbook, limits, rollback, and ownership | Controlled template drafted; execution pending | 20% | [`W04-production-rollout.md`](W04-production-rollout.md) now defines the admission record, persistence/backup/restore/rollback procedure, provisional signal/limit matrix, controlled drills, and sign-off fields. This is planning/control evidence only; no alert, drill, owner, or production deployment is being claimed. | Bind the template to the actual deployment, replace provisional thresholds, execute D01–D06 with redacted evidence, connect collector/pager routes, and assign data/operator/release owners. | 1–2h engineering plus review and external execution | Production operations gate |
| Release approval | Blocked by the matrix above | 0% | Current decision is **NO-GO**. The positive demo is context, not a release sign-off. | Change to GO only after the fresh hosted logs, artifact/package evidence, production configuration/recovery evidence, provider boundary, and operational checklist are all signed off. | 0.5–1h review | Release decision |

### Production exit criteria

These are the W04 production-rollout conditions; a checked W04 implementation
item alone does not satisfy them.

- [x] Current-tip qualification run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) completed the Linux Node,
  ARM Node, macOS-latest Node, and macOS-15-intel Node jobs successfully, with
  the exact `Verify PGlite integration and restart recovery` step passing in
  all four logs. This supersedes the isolated W04.2 closure evidence in run
  `35588994864`; queued, skipped, cancelled, partial, or pre-fix evidence does
  not count.
- [x] The dependent native artifact aggregation and package-distribution
  checks pass, and a clean consumer install/smoke result is recorded for the
  supported release outputs. Current-tip run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595)
  passed `aggregate-native` [106456609397](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106456609397)
  for all five native packages and `mount-rs clean consumer install/smoke:
  PASS`; Windows Node [106450522875](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522875)
  retained `native-win32-x64-msvc.zip` artifact `10656486511` with SHA-256
  `380a7f936477db1948cfac9089d0ccc1170e787f9b4500537bcc0581122e1c58`.
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
| Current recovery-run terminal evidence | Partial — ARM exact PGlite/restart green; provider performance and required platform gates remain open | 25% of current required Node/platform evidence | ARM Node job `106553420990` passed the exact recovery step and early rejection; Windows package job `106553420495` passed; Ozone/TiDB and Ozone/FoundationDB failed only their hard IOPS thresholds after functional markers; Intel macOS is still running and macOS-latest/Linux Node are queued. | Wait for all required platform jobs, inspect exact logs, and re-run from the current `origin/main` after the pre-fix Ozone run terminates. | 0.25–0.75h engineering; hosted runner/provider wait external |
| WebDAV exact session parity metadata stabilization | Complete — implementation and focused local verification; hosted confirmation pending | 100% implementation/local / 0% current-tip hosted | Commit `9b2dabd7` pins the native and oracle directory/file mtimes after the method PUT so exact PROPFIND XML comparison is deterministic; `node --check`, 30 focused local repetitions, formatting, and `git diff --check` passed. The stale macOS-15-intel job in run `35664315098` failed before the fix with a derived ETag-only body mismatch. | Inspect the WebDAV and exact PGlite/restart steps in recovery run `35666527609`; no queued or partial result closes this item. | 0.1h engineering; hosted runner wait external |
| Ozone block-contract cleanup de-duplication | Complete — implementation and local verification; hosted requalification pending | 100% implementation/local / 0% current-tip hosted | Commit `23c0ba7e` adds the repeated first PUT ID to the cleanup set once and avoids deleting an identical second PUT ID twice. The local `mount-rs-r2` suite passed 18/18 and locked standalone Ozone workspace compilation passed. Stale run `35664315098` failed both Ozone contracts at cleanup with `FsError { code: Enoent, syscall: Some("block object") }`. | Re-run Ozone/TiDB and Ozone/FoundationDB from a revision containing `23c0ba7e`, then keep the separate 1,000-IOPS provider gate open. | 0.25h engineering; provider runner wait external |
| Linux FUSE forced-unmount handoff bound | Fix published; hosted post-fix verification pending | 100% implementation / 100% host-safe local checks / 0% current Linux-kernel gate | Commit `bc292709` bounds session-task handoff before lazy detach. The stale run `35664315098` Ubuntu Rust job failed the two forced-unmount tests before that commit (`405.209085ms` extra teardown and a one-second bounded-phase timeout); the current macOS host passes 14 host-safe FUSE tests, but Linux-gated tests are not compiled on this host. | Inspect the Ubuntu Rust job in a current-tip qualification; if the post-fix Linux tests still fail, reproduce in the hosted Linux environment and publish a separate fix before claiming the FUSE gate. | 0.5–1.5h engineering if hosted failure reproduces; runner wait external |
| Land real socket-server integration and provider/factory coverage | Complete — implementation | 100% | W04 tracker checkbox; current tree contains the PGlite socket server, provider, factory, and integration test surfaces under `integrations/mount-rs-pglite/` and `tests/pglite/`. | None for this item. Keep regressions covered by the W04 gate. | 0h remaining |
| Fix transaction cleanup before releasing a connection slot | Complete — implementation and regression | 100% | Tracker records `29ffb3b`; deterministic slot-release suite passed during the recorded W04 local gate. | None for this item. | 0h remaining |
| Add injected cleanup-failure regression and fail closed | Complete — implementation and regression | 100% | Tracker records `cfce82a`; `server_cleanup_failure.mjs` reported `pglite cleanup failure fail-closed: ok`; the expected injected error was retained as diagnostic evidence. | None for this item. | 0h remaining |
| W04.1 full `scripts/test-pglite.sh` local gate | Complete — local qualification | 100% | The latest post-fix oracle-enabled run at `6d59d204af80c883bf47a59ffb5a4b77829f8ec8` exited 0 and passed slot cleanup, injected failure, provider parity, reconnect, fencing, cancellation, disk restart, mixed stores, Node factories, chunked mounts, and userspace FUSE. R2 and TiDB/RustFS rows retained explicit skips. The synced workspace test gate also exited 0 at `1137244`; scoped PGlite/N-API Clippy passed with `-D warnings`. | No further local W04.1 action unless W04.2 exposes a regression. | 0h remaining |
| Fresh current-tree rerun, SDK/CLI users, upstream, and trace lanes | Complete — local qualification | 100% | The latest post-fix current-tree run with the exact pinned oracle `pithings/mountx@85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` exited 0: Rust SDK `pass=6 skip=3 fail=0`, Node SDK `pass=5 skip=3 fail=0`, CLI `pass=12 skip=2 fail=0`, upstream `1,200 passed / 82 skipped`, and trace `40/40` across five seeds and eight backends, including PGlite and chunked-PGlite. R2/TiDB/RustFS credentials/services remained explicit skips; hosted/provider claims are not inferred from this local result. | None for the current local result; hosted W04.2 remains separate. | 0h remaining |
| Teardown-race fix and bounded close/reopen safety | Complete — implementation and local regression | 100% | Tracker records the PostgreSQL Terminate-frame cleanup path, I/O-turn barrier, listener restoration, and tracked cleanup barrier. Readiness passed 10/10; bounded close/reopen passed 5/5 in the recorded gate; focused current-tree runs also passed the bounded regression repeatedly. | None unless hosted macOS reproduces the historical `Eio` failure. | 0h remaining; 2–6h contingency if hosted failure reproduces |
| W04.2 hosted macOS/Linux reconnect acceptance | **Complete — current-tip external hosted gate** | **100%** | Current-tip run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) completed Linux Node [106450522757](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522757), macOS-latest Node [106450522868](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522868), and macOS-15-intel Node [106450522845](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522845) successfully; ARM Node [106450522980](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522980) also passed. Direct logs and job metadata confirm the exact `Verify PGlite integration and restart recovery` step passed on all four Node platforms; both macOS logs recorded PGlite/chunked-PGlite trace PASS, `providersFailed: 0`, backup/restore/rollback PASS, and no Intel EPIPE recurrence. | None for W04.2. Keep the production rollout matrix open: production configuration, provider scope, rollback, observability, and ownership still need deployment evidence. | 0h remaining; production gates remain 3–6h plus external/provider wait | Hosted acceptance |
| W04 production native package and clean-consumer gate | **Complete — current production candidate** | **100%** | Current-tip run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) passed Windows Node distribution in job [106450522875](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522875), uploaded `native-win32-x64-msvc.zip` artifact `10656486511` with SHA-256 `380a7f936477db1948cfac9089d0ccc1170e787f9b4500537bcc0581122e1c58`, and passed five-package `aggregate-native` plus clean-consumer validation in job [106456609397](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106456609397). | Retain the exact artifact/hash in the release record; production release remains NO-GO until persistence/rollback, provider scope, operations, ownership, and release approval close. | 0.1h evidence publication; 0h package implementation | Hosted/package gate |
| W04 Windows hosted long-symlink gate | Complete — hosted qualification | 100% | Isolated run [35602906005](https://github.com/andymac4182/mount-rs/actions/runs/35602906005), Windows Rust job [106343197217](https://github.com/andymac4182/mount-rs/actions/runs/35602906005/job/106343197217), and exact test `windows_long_symlink_creation_uses_extended_path_fallback` passed after the missing-path trigger fix published as `900994a`. The local Windows-target check also passed. | None for this blocker; keep the broader native/package/Node gates open. | 0h remaining |
| W04 N-API reconcile-provider teardown | Complete — implementation and hosted qualification | 100% | Commit `88ecee2` clears the stored `Filesystem.reconcile` callback after successful shutdown, releasing the cloned `ChunkedFs`/SQLite handles before Windows temporary-directory removal. The N-API library suite passed 15 tests; isolated Windows Node job [106369156167](https://github.com/andymac4182/mount-rs/actions/runs/35610813385/job/106369156167) completed `Verify Windows package distribution` successfully, and aggregate job [106379759443](https://github.com/andymac4182/mount-rs/actions/runs/35610813385/job/106379759443) passed the clean consumer gate. | None for this cleanup fix; retain the hosted log with the release record. | 0h remaining |
| W04 HTTP parity oracle fixture | Complete — implementation and isolated hosted candidate parity | 100% | Commit `98243d2` preserves the real mtime only for private active multipart-upload directories in `examples/http_oracle.rs`; local `check-http-parity.mjs` passed all 40 S3/WebDAV paired cases, the full S3 gateway suite passed 15 tests, and all four Unix Node jobs in candidate run [35610813385](https://github.com/andymac4182/mount-rs/actions/runs/35610813385) completed the HTTP differential lane before their exact PGlite recovery step. | None for the W04 HTTP parity fix; provider-scope failures are tracked separately. | 0h remaining |
| W04 production-candidate hosted qualification | Partial — accepted W04 packet is green; isolated current-main qualification is terminally mixed and the production candidate remains blocked | 90% | Accepted run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595) passed all four Node exact PGlite/restart steps, Windows package distribution, aggregate/clean-consumer smoke, Ubuntu Rust, and FoundationDB/RustFS. Later manual run [35654281185](https://github.com/andymac4182/mount-rs/actions/runs/35654281185) at head `595c5c85` ended terminal failure: all four Node jobs hit the same W01/S3 multipart-complete ETag mismatch, Ubuntu Rust failed the 404.004757ms forced-unmount deadline, native FUSE timed out, Ozone measured 92.53/84.02, 5.70, and 34.07 IOPS against 1,000, W26 failed closed on the missing marker, and aggregate-native was skipped. | Keep the candidate NO-GO; assign the W01/S3 parity regression and native/provider/FUSE failures to their owning workstreams, decide whether Ozone/TiDB/RustFS are in launch scope, and requalify the exact release revision before any production approval. | 1–4h engineering/scope work plus hosted/provider wait |
| W04 credential-free PGlite-only launch-config policy | Complete — implementation and terminal current-tip dedicated hosted workflow | 100% implementation / 100% current-tip policy workflow | `scripts/verify-w04-pglite-production-config.mjs` validates an absolute normalized mountpoint, splitstore PGlite metadata/blocks, durable providers, external `MOUNT_RS_PGLITE_URL` references, distinct scoped volume keys, bounded chunking, an explicit positive safe-integer `lease_ttl_ms` no greater than 24 hours, owner metadata, and fail-closed unknown/inline-secret cases. The positive, non-durable, inline-secret, invalid-TTL, and missing-TTL fixtures passed in manually dispatched hosted run [35642541696](https://github.com/andymac4182/mount-rs/actions/runs/35642541696), job [106475151000](https://github.com/andymac4182/mount-rs/actions/runs/35642541696/job/106475151000), at current main head `6c80eccb`; the exact log emitted `W04_PGLITE_PRODUCTION_CONFIG_POLICY_PASS ... lease_ttl=explicit-bounded` plus the four expected rejection markers. Push-triggered run `35642363293` was cancelled before any job materialized (`jobs=[]`) and is explicitly excluded. This remains configuration-shape evidence only. | Bind the actual PGlite server data directory, version policy, backup/restore, rollback, RPO/RTO, and owners to a deployment; this policy does not connect to PGlite or approve production. | 0.1h evidence retention; deployment review external | Production config gate |
| W04 explicit split-store lease TTL configuration | Complete — implementation, local qualification, and hosted policy confirmation | 100% implementation / 100% focused local qualification / 100% current-tip policy | Commit `113a1fbc` adds optional `driver.storage.lease_ttl_ms` to the shared Rust and Node CLI schema, preserves the 30-second default for non-production callers, maps the value to the public SDK/N-API chunked driver, and documents the recovery tradeoff. The W04 production policy requires the field explicitly. Evidence: mount-rs CLI 46/46, SDK 3/3, Node CLI reopen with `120000`, provider-matrix CLI 10 pass / 3 explicit skips / 0 failures, positive and four invalid fixtures locally, and manual hosted policy run [35642541696](https://github.com/andymac4182/mount-rs/actions/runs/35642541696) job [106475151000](https://github.com/andymac4182/mount-rs/actions/runs/35642541696/job/106475151000); `cargo fmt --check` and `git diff --check` passed. | Select and record the deployment-specific TTL against measured provider latency and stale-writer recovery objectives, and include it in the production configuration/recovery drill; this hosted policy result does not prove production behavior. | 0.5–1h deployment/documentation; owner decision external | Engineering plus hosted/deployment gate |
| W04 local backup/restore/rollback rehearsal | Complete — local supporting evidence; production drill open | 100% harness / 0% production | `tests/pglite_server_lifecycle.rs` now copies a quiesced disk-backed PGlite data directory into an isolated restore directory, verifies a fresh-client read, restores the pre-candidate copy after a bad-release marker, and emits `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`; both ignored lifecycle tests passed locally. | Replace the filesystem copy with the approved encrypted backup mechanism, execute D02/D03 against the named deployment, measure RPO/RTO, and retain redacted backup/restore/rollback evidence. | 0.5–1h harness implementation complete; production execution external | Production durability/recovery gate |
| W04 native R2 key path in configuration-driven FoundationDB/RustFS test | Complete — current hosted confirmation; production provider scope pending | 100% implementation / 100% current hosted qualification | Current-tip run [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595), FoundationDB/RustFS job [106450522539](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522539), passed the relative-key configuration path, durable chunk/reopen checks, configured FUSE mount/reopen, N-API bounded readdir, service restart, VFS restart, and cleanup markers. | Keep the provider as an explicit launch-scope decision; this hosted result does not establish customer production credentials, replication, backup, or DR. | 0.25h evidence retention; external provider/owner gate |
| W04 macOS early-rejection socket EPIPE guard | Implementation/local remediation complete — current-tip hosted confirmation in progress | 100% implementation/local / 0% current-tip hosted | The historical Intel job [106450522845](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522845) passed the earlier guard `41d5514`, but run [35661798263](https://github.com/andymac4182/mount-rs/actions/runs/35661798263), macOS-15-intel job [106538515099](https://github.com/andymac4182/mount-rs/actions/runs/35661798263/job/106538515099), reproduced Node 24 `write EPIPE` before PGlite. Commit `d955042b` replaces the `http.ClientRequest` writer with an explicit fragmented TCP/HTTP writer/parser and narrowly absorbs only post-response EPIPE/reset errors; local stress passed 30 fail-fast runs and the pinned-oracle HTTP differential passed 40/40. Current-tip run [35664315098](https://github.com/andymac4182/mount-rs/actions/runs/35664315098) is the hosted confirmation and is not yet terminal. | Inspect both macOS logs and require the exact early-rejection and PGlite/restart steps to pass on the published revision; do not promote queued/skipped evidence. | 0.25–0.5h engineering complete; hosted runner wait external |
| W04 privileged Linux FUSE `rootmode` boundary | Complete — current hosted confirmation | 100% implementation / 100% current hosted qualification | FoundationDB/RustFS job [106450522539](https://github.com/andymac4182/mount-rs/actions/runs/35635114595/job/106450522539) passed `cli_foundationdb_rustfs_config_binary_mounts_and_reopens`, `FOUNDATIONDB_CLI_PASS`, N-API bounded readdir, service restart, VFS restart, and RustFS cleanup after `a55bd64`, `aa3dae3`, and `9ef982d`. | None for the rootmode/config-mount fix; provider production scope and performance thresholds remain separate gates. | 0h remaining |
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
   decision to GO; current-tip run `35635114595`, aggregate job
   `106456609397`, Windows artifact `10656486511`, and its clean-consumer step
   satisfy the hosted qualification package gate. The production decision
   remains NO-GO for the independent persistence, provider, operations, and
   release gates.
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
11. [x] Complete current-tip run `35635114595` from published source
    `d2db74dd`; Linux, ARM, macOS-latest, and macOS-15-intel exact
    PGlite/restart steps, Windows packaging, aggregate-native, clean-consumer,
    Ubuntu Rust, and corrected FoundationDB/RustFS provider evidence all
    passed. The run is still not a production qualification because its
    configured Ozone IOPS rows failed, TiDB/RustFS failed a native Node mount
    assertion, and W26 could not find the required Ozone marker. Earlier runs
    `35617849558`, `35619576365`, `35620669973`, and `35630660185` remain
    useful historical/diagnostic evidence but do not override the latest
    terminal result. Queued, skipped, cancelled, partial, or pre-fix evidence
    does not count.
12. [ ] The corrected FoundationDB/RustFS configuration-driven mount now
    passes in job `106450522539`, but the Ozone 1,000-IOPS threshold and the
    TiDB/RustFS native-mount assertion remain production launch decisions.
    Retain the functional-pass/performance-fail split until a provider/release
    owner explicitly excludes those services from launch scope or accepts and
    remediates their gates.
13. [x] Diagnose and correct the four Ubuntu Rust FUSE lifecycle failures from
    the prior run; the fix is published as `d2db74dd`, and current-tip Ubuntu
    Rust job `106450522426` passed the full workspace gate. The follow-up
    provider and operational gates remain open.
14. [x] Diagnose and harden the Windows structural N-API peer-fault test. The
    pre-fix diagnostic run `35650347479`, Windows job `106500944953`, failed
    exactly at `S3 peer-fault callback (+19754ms)`; `093565d9` now uses
    `resetAndDestroy()` after an incomplete request, matching the deterministic
    Rust transport test. The rebuilt full pinned-oracle N-API suite passed
    locally; fixed-tip hosted job `106503290630` completed successfully. The
    enclosing run later failed on separate Unix Node S3 parity, Ubuntu
    Rust/native-FUSE, and provider gates; this sub-gate is not promoted to
    full-run or production acceptance.

15. [x] Diagnose the terminal fixed-tip qualification boundary. Run
    `35651055621` completed with the Windows W04 peer-fault/package sub-gate
    green, but all four Unix Node jobs failed at the same
    `s3/multipart-complete-signed-trailer` ETag mismatch after the W01/S3
    finalization-marker change; Ubuntu Rust/native-FUSE and provider jobs also
    failed. The evidence is retained as a mixed-scope blocker and does not
    change W04.2 closure or the production NO-GO decision.

16. [x] Requalify the current main tip through isolated workflow dispatch.
    Manual run `35654281185` at head `595c5c85` reproduced the four-platform
    W01/S3 ETag mismatch before recovery, the Ubuntu Rust forced-unmount
    deadline regression, native-FUSE timeout, sub-100 IOPS provider gates, and
    failed-closed W26 evidence. Windows/package and selected native/provider
    jobs passed, but the mixed result remains production NO-GO evidence.

## Provisional remaining effort and blockers

| Category | Estimate | Classification |
| --- | ---: | --- |
| Hosted macOS log inspection after runners start | Complete, ~0.5h actual | Engineering/verification |
| Tracker/dashboard closure, formatting, commit, and push | Complete, ~0.5h actual | Engineering/documentation |
| Potential remediation if hosted macOS exposes a regression | 2–6h | Engineering contingency |
| Current Intel socket EPIPE requalification | Complete; current run `35635114595` exact early-rejection and PGlite/restart steps passed in job `106450522845` | Hosted/native gate; 0h remaining |
| GitHub Actions queue delay | Resolved for W04.2 by isolated qualification branch; future shared-main churn remains external | External blocker; not engineering time |
| R2/TiDB local credential/service skips | Unknown | External provider prerequisites; not W04.2's macOS/Linux closure criterion |
| Fixed-tip full qualification diagnosis and ledger publication | Complete, ~0.4h actual | Engineering/evidence; W01 S3 and provider/native remediation remain external or separately owned |
| Isolated current-main production qualification and evidence capture | Complete, ~1.0h actual | Hosted evidence; W01 S3, FUSE, provider-performance, and deployment gates remain external or separately owned |
| Native artifact aggregation and clean consumer smoke | Complete in current run `35635114595` (`aggregate-native` job `106456609397`); 0.1h release-record retention remains | Hosted/package gate |
| Windows Node `EBUSY` cleanup remediation and hosted rerun | Complete; 0h remaining | Engineering plus hosted/native gate |
| Cross-platform `NoSuchUpload` HTTP-parity lane | Complete in isolated candidate; 0.25h release-record retention remains | Hosted/provider boundary; all four Unix Node HTTP parity lanes passed in `35610813385`; provider-scope failures are separately recorded |
| Privileged Linux FUSE `rootmode` correction and hosted confirmation | Complete; current run `35635114595` passed configured FUSE mount/reopen and restart markers | Engineering fix plus hosted native/provider evidence; `a55bd64` masks `rootmode` to `S_IFMT`, `aa3dae3` separates privileged helper metadata, and `9ef982d` aligns the assertion |
| Ozone composition IOPS threshold | Open; current run `35635114595` measured 47.48 and 57.85 in `ozone-compositions`, 15.37 in Ozone/TiDB, and 32.86 in Ozone/FoundationDB versus target 1,000; 0.5–2h to benchmark/remediate or obtain scope decision | External provider/performance gate; functional lifecycle and cleanup passed |
| FoundationDB/RustFS configuration-driven FUSE requalification | Complete for current hosted path; 0h engineering remaining, production provider scope still open | Provider/native gate; current job `106450522539` passed relative-key, configured mount/reopen, N-API, service restart, VFS restart, and cleanup evidence |
| Explicit split-store lease TTL configuration | Published in `113a1fbc`; focused local qualification and current-tip hosted policy complete | Engineering/configuration gate; deployment owner must still select the value and verify provider-latency and stale-writer recovery objectives |
| Credential-free PGlite-only launch-config policy | Complete; current-tip run `35641832862` passed; 0.1h evidence retention remaining | Hosted policy shape is green, but no provider connection, production data directory, backup, or owner approval was used |
| Local backup/restore/rollback rehearsal | Complete; 0.5–1h harness implementation complete | Production execution remains external; local copy is not a production backup or RPO/RTO result |
| Full post-tip repository CI confirmation | Complete for the current Ubuntu Rust/Node/package candidate in run `35635114595`; the prior four FUSE lifecycle failures were fixed in `d2db74dd`, and Ubuntu Rust job `106450522426` passed; provider/performance rows remain separate | Hosted/full-repository release gate; 0h engineering for the repaired Rust gate |
| Fixed-tip Windows N-API peer-fault/package confirmation | Pending in run `35651055621`, job `106503290630`; implementation and local oracle suite are green | 0.1–0.25h engineering evidence retention; hosted Windows/package/aggregate wait external |
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
| 2026-09-22 02:40–02:45 AEST | Retrieved the terminal log for the retained main CI job after it reached the new W04 policy step. | The positive policy and both negative fixtures passed in job `106423435077`; the enclosing run `35626892113` was cancelled by unrelated mainline churn during Ozone work, so no full CI or production claim was added. | Hosted evidence boundary, ~0.1h; full CI and deployment gates external |
| 2026-09-22 02:45–02:50 AEST | Added a dedicated `.github/workflows/w04-production-policy.yml` gate with `cancel-in-progress: false` so static W04 launch-config evidence is not coupled to the provider matrix. | Local workflow command shape and policy fixtures remain green; the dedicated hosted run is pending, while the production decision remains NO-GO. | Engineering/release controls, ~0.2h; hosted run and deployment gates external |
| 2026-09-22 02:50–02:55 AEST | Inspected the dedicated W04 policy workflow and its complete job log. | Run `35627650073`, job `106425838566`, passed the positive policy and both expected rejection fixtures; production remains NO-GO because this is configuration-shape evidence only. | Hosted evidence boundary, ~0.1h; deployment and release gates external |
| 2026-09-22 02:54–02:55 AEST | Waited for and inspected the current-tip dedicated W04 policy workflow after concurrent mainline pushes. | Run `35628414227`, job `106428430754`, passed the positive policy and both expected rejection fixtures on `9f1d3dc`; production remains NO-GO because the run validates configuration shape only. | Hosted evidence boundary, ~0.1h; deployment and release gates external |
| 2026-09-22 02:55–03:00 AEST | Waited for and inspected the current-tip dedicated W04 policy workflow after the ledger-only publication. | Run `35627894548`, job `106426702455`, passed the positive policy and both expected rejection fixtures; current-tip config evidence is terminal, while production remains NO-GO. | Hosted evidence boundary, ~0.1h; deployment and release gates external |
| 2026-09-22 03:03–03:05 AEST | Waited for and inspected the current-tip dedicated W04 policy workflow after the latest mainline synchronization. | Run `35629439034`, job `106431854955`, passed the positive policy and both expected rejection fixtures on `e9a6b25`; production remains NO-GO because the run validates configuration shape only. | Hosted evidence boundary, ~0.1h; deployment and release gates external |
| 2026-09-22 03:05–03:40 AEST | Enabled manual workflow dispatch and isolated concurrency, ran the full current qualification without cancellation, waited for both macOS Node jobs, and inspected every terminal W04/provider/package log. | Run `35630660185` at `42f83bd` passed ARM, Ubuntu, macOS-latest, and macOS-15-intel exact PGlite/restart steps, Windows package distribution, aggregate-native, clean-consumer smoke, and FoundationDB/RustFS durability markers. Ozone measured 35.91/59.25, 14.69, and 7.50 IOPS against 1,000; Ubuntu Rust failed four FUSE lifecycle tests. Ledger update remains NO-GO and is the next publication chunk. | Hosted evidence and production boundary, ~0.5h engineering; runner/provider time external |
| 2026-09-22 03:45–04:17 AEST | Reproduced the four hosted Ubuntu Rust FUSE failures in a Rust 1.95 Linux container, fixed stream-backed test framing and the stale privileged expectation, ran the focused 14-test suite, published `d2db74dd`, dispatched current-tip qualification, and inspected terminal Node/package/provider logs. | Run `35635114595` passed Ubuntu Rust, all four Node exact PGlite/restart steps, package aggregation/clean-consumer smoke, and FoundationDB/RustFS markers. Ozone IOPS measured 47.48/57.85, 15.37, and 32.86 against 1,000; TiDB/RustFS failed its native Node mount assertion; W26 lacked `OZONE_IOPS_PASS`; production remains NO-GO. | Engineering/hosted evidence, ~1.0h engineering; runner/provider time external |
| 2026-09-22 04:17–04:55 AEST | Added explicit `lease_ttl_ms` support to the shared Rust/Node split-store configuration, mapped it through the public SDK/N-API drivers, bounded the W04 production policy to 24 hours, added positive/invalid fixtures and focused tests, then rebased and pushed the implementation chunk. Updated the production ledger, rollout runbook, and tracker follow-up. | Published implementation `113a1fbc`; mount-rs CLI 46/46, SDK 3/3, Node CLI reopen, provider-matrix CLI 10 pass / 3 explicit skips / 0 failures, policy positive plus three fail-closed fixtures, shared Cargo formatting, and `git diff --check` passed. Current-tip hosted policy and deployment-specific TTL selection remain open; production stays NO-GO. | Engineering/documentation, ~0.65h; hosted/deployment gates external |
| 2026-09-22 04:55–04:59 AEST | Monitored the dedicated W04 policy workflow triggered by the documentation publication and inspected its terminal job log. | Run `35641832862`, job `106472745045`, passed the positive bounded-TTL policy and rejected non-durable, inline-secret, and zero-TTL fixtures with the expected markers. Hosted configuration-shape evidence is now current-tip green; deployment-specific persistence, recovery, provider, operations, ownership, and release gates remain open. | Hosted verification/documentation, ~0.1h; deployment/provider gates external |
| 2026-09-22 04:59–05:03 AEST | Tightened the W04 production policy so the general CLI may retain its backward-compatible default, but an approved production config must explicitly set `lease_ttl_ms`; added a durable missing-TTL negative fixture and wired it into both policy workflows. | Local policy checks reject missing, zero, non-durable, and inline-secret cases; the next hosted policy run is required for the new enforcement. Production remains NO-GO. | Engineering/verification, ~0.15h; hosted/deployment gates external |
| 2026-09-22 05:03–05:09 AEST | Recovered the hosted policy evidence after the push-triggered run was cancelled before job creation, manually dispatched the non-cancelling policy workflow on current main, and inspected its complete log. | Run `35642363293` is recorded as non-evidence because it ended with `jobs=[]`; manual run `35642541696`, job `106475151000`, passed `lease_ttl=explicit-bounded` and emitted the four expected rejection markers, including `config.driver.storage.lease_ttl_ms-is-required-for-production`. Production remains NO-GO because this is configuration-shape evidence only. | Hosted verification/documentation, ~0.1h; cancellation and deployment/provider gates external |
| 2026-09-22 05:10–06:29 AEST | Investigated the reproducible Windows N-API S3 timeout, synchronized to current main, rebuilt with the shared Cargo target, added phase attribution (`3705ad0`), and identified the race in the response-reset peer-fault test. Replaced it with a deterministic incomplete-request plus `resetAndDestroy()` case (`093565d9`), rebased/pushed both chunks, and dispatched fixed-tip qualification `35651055621`. | Pre-fix Windows job `106500944953` failed exactly at `S3 peer-fault callback (+19754ms)`; local full pinned-oracle `pnpm --dir integrations/mount-rs-napi test` then passed all runnable tests, including structural server, S3 restart, package/distribution and clean-consumer rows. Fixed-tip Windows job `106503290630` was still building at the ledger snapshot; no hosted acceptance is claimed yet. | Engineering/verification, ~1.25h; hosted Windows/package wait external |
| 2026-09-22 06:29–06:42 AEST | Waited for and inspected the terminal fixed-tip qualification `35651055621`, including the four Node logs, Ubuntu Rust/native-FUSE, provider, aggregate, and W26 evidence jobs. | Windows `106503290630` passed the repaired W04 sub-gate. ARM/Linux/macOS Node all failed the same `s3/multipart-complete-signed-trailer` ETag mismatch; Ubuntu Rust/native-FUSE and Ozone/TiDB/RustFS provider gates failed; aggregate was skipped and W26 failed closed on missing `OZONE_IOPS_PASS`. Production remains NO-GO; the S3 mismatch is recorded as W01-owned follow-up rather than changed from this W04 worktree. | Hosted evidence/documentation, ~0.4h; W01/provider/native remediation external or separately owned |
| 2026-09-22 06:49–07:20 AEST | Diagnosed repeated ordinary-CI cancellation under mainline churn, dispatched isolated manual qualification `35654281185` at current main head, waited for all Node platforms and the terminal provider/native matrix, and inspected the exact completed logs. | All four Node jobs failed before recovery on the same ETag mismatch and skipped the exact recovery step; Ubuntu Rust failed at `404.004757ms`, native FUSE timed out at its 15-minute limit, Ozone IOPS were 92.53/84.02, 5.70, and 34.07 against 1,000, W26 failed closed on missing `OZONE_IOPS_PASS`, while Windows/package, macOS Rust/NFS/WebDAV, TiDB/RustFS, and observability passed. Production remains NO-GO and the S3 regression remains W01-owned. | Hosted evidence/documentation, ~1.0h; W01/FUSE/provider/deployment remediation external or separately owned |
| 2026-09-22 07:20–08:46 AEST | Inspected the stale Ozone lockfile failure in run `35661284009`, diagnosed the current Node 24 macOS `write EPIPE` in run `35661798263`, replaced the `http.ClientRequest` fragmented writer with an explicit TCP/HTTP writer and response parser, and added a narrowly scoped post-response shutdown guard. | Local focused regression passed 30 fail-fast repetitions; the exact pinned-oracle HTTP differential passed 40/40 S3/WebDAV cases; `node --check`, `git diff --check`, and shared Cargo formatting passed. The implementation was committed as `d955042b`, rebased onto concurrent mainline work, and pushed as `27c96424` to `origin/main`. | Engineering/verification, ~1.25h; hosted confirmation and provider/deployment gates external |
| 2026-09-22 08:45–08:46 AEST | Dispatched the fresh full qualification from published head `27c96424` and expanded this ledger with the exact run/job boundary, current production status, evidence split, estimates, and remaining actions. | Run `35664315098` is active; ARM, FoundationDB/RustFS, Ozone/FoundationDB, Windows Rust, and Windows Node had started, while both macOS Node jobs were still queued at the snapshot. No queued/in-progress step is promoted to acceptance; production remains NO-GO. | Release engineering/documentation, ~0.2h; hosted runner time external |
| 2026-09-22 09:05–09:17 AEST | Implemented and published the WebDAV parity metadata stabilization and Ozone cleanup de-duplication as separate chunks, then rebased the workstream onto concurrent `origin/main` changes. | WebDAV focused parity passed 30 repetitions; Ozone R2 passed 18/18 and locked standalone compilation passed; the published mainline includes `9b2dabd7` and `23c0ba7e`. | Engineering/verification, ~0.45h; fresh hosted requalification external |
| 2026-09-22 09:18–09:24 AEST | Diagnosed the stalled qualification recovery, confirmed the stale run's exact pre-fix WebDAV/Ozone/FUSE failures, canceled run `35664315098`, and kept run `35666527609` as the active recovery monitor. | The stale run is excluded from acceptance; the recovery run has Windows/ARM in progress and Linux/macOS/provider jobs queued at the snapshot. Current production decision remains NO-GO. | Release engineering/hosted diagnosis, ~0.2h; hosted runner/provider wait external |
| 2026-09-22 09:24–09:35 AEST | Inspected the completed ARM, Windows, Ozone/TiDB, and Ozone/FoundationDB logs from recovery run `35666527609`. | ARM Node exact PGlite/restart and early-rejection steps passed; Windows package/consumer checks passed; Ozone/TiDB measured 30.34 IOPS and Ozone/FoundationDB 56.72 IOPS against the 1,000 target after functional contract/durability/cleanup markers passed. Intel macOS remains in progress and macOS-latest/Linux Node remain queued; production remains NO-GO. | Hosted evidence/production boundary, ~0.2h; runner/provider wait external |

| 2026-09-22 10:20–10:48 AEST | Diagnosed the hosted Ubuntu/native-FUSE timeout as an unobserved failed-unmount result being immediately retried; fixed the result-publication race, restored the bounded session-stop grace before forced teardown, ran host tests, Linux-target compilation, formatting and diff checks, rebased, pushed `d3c9bb9d` to `origin/main`, and dispatched exact-tip qualification run `35673297621`. | The new run is executing at the published SHA. ARM Node and macOS-15-intel Node are in progress; macOS-latest/Linux Node, Ubuntu Rust, and native FUSE are queued. No queued or partial result is promoted; production remains NO-GO. | Engineering/verification, ~0.5h; hosted runner/provider time external |
| 2026-09-22 10:48–11:05 AEST | Inspected run `35673297621` after native FUSE completed. The result-publication fix removed the hang, but the blocked-read test reached a concrete `fusermount3 ... Device or resource busy` error; added a narrow EBUSY forced-detach fallback, reran host/Linux-target checks, pushed `1bdd6adf`, and dispatched run `35673738166`. | The old run is diagnostic only; its native-FUSE job failed at the EBUSY assertion while Node jobs were still non-terminal. The replacement run is queued at the exact new SHA; no current-tip acceptance is claimed and production remains NO-GO. | Engineering/hosted diagnosis, ~0.35h; hosted runner/provider time external |
| 2026-09-22 11:05–11:16 AEST | Ran the local production-facing controls on the synchronized tree: positive and three fail-closed PGlite policy fixtures, the disk-backed restart plus isolated backup/restore/rollback rehearsal, N-API artifact aggregation, generated typecheck, focused FUSE tests, formatting, and diff checks. | Policy fixtures passed/fail-closed; lifecycle passed 2/2 with `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`; artifact aggregation passed; focused FUSE passed 14/14. The local checkout still lacks four hosted native artifacts, so clean-consumer acceptance remains external. | Engineering/verification, ~0.35h; deployment and hosted artifact gates external |
| 2026-09-22 11:16–11:22 AEST | Inspected current run `35674630831` and the completed ARM Node log, traced `TS2740` to the pre-`d0a67b22` clean-build declaration path, verified the current `pnpm --dir integrations/mount-rs-napi build` preserves `Map<string, number>` and passes typecheck, then canceled the obsolete run. | ARM failure is classified as pre-fix generated-declaration evidence; no PGlite/restart acceptance is claimed. The replacement run will be dispatched after this ledger publication from the current synchronized main tip. Production remains NO-GO. | Engineering/hosted diagnosis, ~0.25h; replacement runner/provider time external |
| 2026-09-22 11:22–11:24 AEST | Committed and pushed the production checkpoint as rebased `0c31c167`, then dispatched replacement qualification run `35675591961` from that exact published head and recorded the Node, Ubuntu Rust, native-FUSE, and Windows job IDs. | The replacement run is queued/in progress with ARM and provider jobs started; no exact PGlite/restart, native, package, or provider PASS is promoted. Production remains NO-GO. | Release engineering/documentation, ~0.1h; hosted runner/provider time external |
| 2026-09-22 11:24–12:01 AEST | Inspected the terminal replacement run `35675591961`, retrieved its failed-job log, and traced the shared Unix/Windows Node failure to the `0c31c167` generated P9 declaration path (`test/types.test.ts(655,9)`: `Record<string, number>` versus `Map<string, number>`). Confirmed native FUSE exceeded its configured timeout and canceled the run after the other jobs were terminal. Then verified the WebDAV provider-network cleanup retry with five fresh concurrency-64 repetitions and published the focused code chunk. | Run `35675591961` is excluded from W04 acceptance. The cleanup fix passed all five fresh local runs, the prior 20-run stress packet, and the full pinned-oracle N-API suite; rebased implementation `819c663e` is now on `origin/main`. Production remains NO-GO; a fresh exact-tip hosted qualification is required. | Hosted diagnosis, engineering, and release integration, ~0.75h; fresh runner/provider time external |
| 2026-09-22 12:01–12:05 AEST | Published the updated ledger/tracker as `0b6e8c4f` after rebase, dispatched full qualification run `35678123095` from that exact SHA, and recorded its Node, Windows, Ubuntu Rust, and native-FUSE job IDs. | The new run is queued; no job or step is treated as evidence yet. Production remains NO-GO pending terminal Node recovery, native/package/provider, deployment, operations, ownership, and release gates. | Release engineering/documentation, ~0.1h; hosted runner/provider wait external |
| 2026-09-22 12:05–12:35 AEST | Inspected all four Node jobs and the terminal native/provider results from run `35678123095`; diagnosed the hosted Linux FUSE metadata-publish/WAL cleanup failure as a pending-atime flush attempting to publish after the filesystem had fail-closed. Added the shutdown lease-release regression, ran the focused test, the full `mount-rs-chunked` library suite (21/21), package Clippy, formatting, and diff checks, then rebased and pushed `3de49e33` to `origin/main`. Inspected the new exact-tip Fault injection logs and the independent W04 policy result. | The four Node exact recovery steps passed on run `35678123095`, but that run remains mixed because native FUSE and provider/composition gates failed and aggregate-native is queued. The shutdown fix is locally verified; Fault injection run `35679778367` passed all three operating systems and W04 policy run `35679778395` passed. New current-head CI run `35679778373` is pending with no jobs materialized, so no current-head or production acceptance is claimed. | Engineering/hosted diagnosis and release integration, ~0.5h; current full CI/native/provider/deployment gates external |

## Publication note

This ledger revision records the W04.2 closure and accepted qualification run
`35635114595` at published revision `d2db74dd`, plus the later terminal
fixed-tip qualification run `35651055621` and the
published production-config control at `68c087d1` on top of `113a1fbc`. All four Node
platforms passed the exact PGlite/restart step; both macOS logs retained
PGlite/chunked-PGlite trace PASS, `providersFailed: 0`, backup/restore/rollback
PASS, and storage artifacts. Ubuntu Rust, Windows package distribution,
five-package aggregation, and clean-consumer smoke also passed. It retains the
exact artifact digests, corrected FoundationDB/RustFS/rootmode qualification,
and the current provider failures: Ozone measured 47.48/57.85, 15.37, and
32.86 IOPS against 1,000, while TiDB/RustFS failed its native Node mount
assertion and W26 lacked `OZONE_IOPS_PASS`. The credential-free PGlite-only
launch-config policy passed its dedicated non-cancelling workflow. Push-
triggered run `35642363293` was cancelled before any job materialized and is
excluded; manual run `35642541696`, job `106475151000`, on current main head
`6c80eccb` emitted `lease_ttl=explicit-bounded` and the four expected
rejection markers, including the required-missing-TTL case. Local CLI/SDK
qualification passes on `113a1fbc`, with explicit policy enforcement published
in `68c087d1`. This is configuration-shape evidence only; it does not prove
production persistence, backup/rollback, or owner approval.
The current W04 production test packet also records the pre-fix Windows
diagnostic failure in run `35650347479` / job `106500944953`, the phase-specific
`S3 peer-fault callback (+19754ms)` timeout, the deterministic `resetAndDestroy`
hardening published as `093565d9`, and the full local pinned-oracle N-API suite
PASS after rebuilding the shared native target. Fixed-tip hosted run
`35651055621` / Windows job `106503290630` passes its exact server, restart,
structural, package, clean-consumer, and artifact checks. The overall run
ended in terminal failure: all four Unix Node jobs hit the same
`s3/multipart-complete-signed-trailer` ETag mismatch, Ubuntu Rust/native-FUSE
and provider gates failed, aggregate-native was skipped, and W26 failed closed
on the missing `OZONE_IOPS_PASS` marker. No full-run or production acceptance
is inferred from the mixed result.
The isolated current-main run `35654281185` independently reproduced the same
four-platform ETag mismatch before recovery, failed the Ubuntu Rust forced-
unmount deadline and native-FUSE timeout gates, measured 92.53/84.02, 5.70,
and 34.07 IOPS against 1,000, and failed closed on the missing W26 marker.
This confirms the production blockers are current-tip evidence, not an
ordinary-push cancellation artifact.
The local
disk-backed `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS` rehearsal remains supporting
evidence only and is not production backup or rollback approval. It is reviewed
with `git diff --check` and the relevant formatting check, then committed and
pushed to `origin/main`. It does **not** approve production: provider
launch-scope/performance, deployment-specific persistence/backup/rollback,
observability, runbook execution, ownership, and release approval remain open.
This publication also records the current-tip macOS Node 24 remediation:
`d955042b` passed 30 fail-fast local repetitions and the 40-case HTTP
differential. Its first full run `35664315098` remained queued because the
macOS runner never materialized and was canceled after its pre-fix WebDAV,
Ozone-cleanup, and Ubuntu-FUSE failures were diagnosed. The recovery run
`35666527609` was dispatched from `9b2dabd7` before `23c0ba7e` was published;
it is retained for the WebDAV/Node confirmation but does not qualify the
current Ozone tip. No queued, skipped, partial, or pre-fix result is promoted
to W04 or production acceptance.

The latest pre-fix qualification `35674630831` is also explicitly excluded:
its ARM Node job failed at the generated `P9SessionStats.messages` type check
on head `ba20d29d`, before `d0a67b22` repaired clean-build declaration
normalization. The current tree's release build, generated typecheck, PGlite
policy fixtures, local disk-backed backup/rollback rehearsal, artifact
aggregation, focused FUSE tests, formatting, and diff checks are green. A
replacement hosted run from the next published tip is required for current
Node, native, package, and provider evidence. Production remains **NO-GO**.

The 2026-09-22 11:24–12:01 AEST recovery inspection is now recorded as a
terminal non-acceptance boundary. Run `35675591961` was canceled after its
native-FUSE job exceeded the configured timeout; `gh run view --log-failed`
showed the four Unix Node jobs and Windows Node failing before W04 recovery at
`test/types.test.ts(655,9)` because the `0c31c167` tree still materialized
`Record<string, number>` instead of `Map<string, number>`. The focused WebDAV
provider cleanup retry was then verified with five fresh concurrency-64
NodeFs/SQLite repetitions and published, after rebase, as `819c663e` on
`origin/main`. A new exact-tip qualification from that published revision is
required; the production decision remains **NO-GO**.

The next exact-tip qualification is now run `35678123095` from published
`0b6e8c4f01ebdd9254c7d6c61595628e0ce4a824`; it was queued at the 12:05 AEST
snapshot. Its Node jobs, Windows package job, Ubuntu Rust job, and native-FUSE
job are recorded in the live table above. Queue state is not acceptance, and
the production decision remains **NO-GO** until the required terminal evidence
and deployment-owner gates are complete.

The hosted failure in run `35678123095` / native-FUSE job `106588864049` was
not treated as a harmless test-only cleanup issue. A metadata-publish fault
correctly fail-closes `ChunkedFs`, but a preceding FUSE read can leave a
pending atime update; the old shutdown path then attempted that blocked
publication before releasing the lease. Commit `3de49e33` now preserves the
fail-closed state while releasing the lease, with a focused regression, the
full `mount-rs-chunked` library suite (21/21), package Clippy, formatting, and
diff checks passing locally. Exact-tip Fault injection run `35679778367`
passed on macOS-latest, Ubuntu-latest, and Windows-latest, and policy run
`35679778395` passed. Push CI run `35679778373` targets the published fix but
is pending with no jobs materialized; the full hosted Linux FUSE, Node,
artifact/package, provider, observability, runbook, ownership, and release
gates remain open, so production is still **NO-GO**.
