# W08 TiDB workstream progress ledger

Status snapshot: **2026-09-21 08:30 UTC / 18:30 AEST**
Repository: `andymac4182/mount-rs`  
Evidence tip: `origin/main` at `6d59d20` (contains the W08 restart-readiness
fix `769ea08`; the concurrent shutdown-lease fix is `c71c8ee`)
Hosted verification queued: CI run `35577929196` for the current main tip

This ledger is the detailed working record for W08 in `WORK_TRACKER.md`. It
separates implementation completion from real provider, durable-service,
native-mount, and hosted-platform acceptance. A checked tracker item is not
treated as proof for a broader unchecked gate. Percentages and time estimates
are provisional planning values, not release-readiness measurements.

## Summary

W08 is approximately **83% complete** by acceptance scope and remains
**Verifying**, not complete. The implementation and bounded single-node
TiDB/RustFS composition are substantially landed. Hosted direct provider,
provider-clock/concurrency, ambiguous-commit, and corrected Linux native
mount rows are now evidenced, but the durable frontend restart still fails
before the post-restart provider phase. The remaining material gates are the
restart result and current macOS/native evidence.

The headline percentage uses this deliberately simple weighting of the five
top-level tracker items; it is not a line-count metric:

| Item | Provisional completion | Weight | Weighted contribution |
| --- | ---: | ---: | ---: |
| W08.1 implementation | 100% | 15% | 15% |
| W08.2 durable TiDB/PD/TiKV harness and restart | 60% | 25% | 15% |
| W08.3 fencing, concurrency, ambiguous commit and durability assumptions | 80% | 20% | 16% |
| W08.4 Node/CLI/native platform acceptance | 85% | 20% | 17% |
| W08.5 bounded TiDB metadata + RustFS composition | 100% | 20% | 20% |
| **Rounded workstream view** |  | **100%** | **83%** |

The W08.4 number includes W08.4a and W08.4b; those nested rows are shown
separately below and are not additional weight in the summary calculation.

## Work-item ledger

| Work item | Status and completion | Implementation evidence | Provider/hosted/native evidence | Remaining actions | Provisional engineering time | External blockers |
| --- | --- | --- | --- | --- | --- | --- |
| **W08.1** Separate TiDB metadata/block implementation and dependencies | **Landed — 100%** | `integrations/mount-rs-tidb/**` is a separate provider crate. The tracker records the implementation landing as `ca57758`; format checks, four unit tests and acceptance binaries passed. The provider has explicit pessimistic transactions, writer fencing, provider-clock reads, CAS publication and ambiguous-commit mapping. | Local focused TiDB library tests passed (`6 passed`). The real v8.5.7 single-node lane has passed schema, UTF-8/trailing-space, identity and provider checks. | No remaining implementation action in this item. Broader replicated-service evidence belongs to W08.2/W08.3. | **0–1 h** for maintenance/documentation only. | None for the landed scope. |
| **W08.2** Durable real TiDB/PD/TiKV Docker harness and restart results | **Verifying — 60%** | The harness is pinned to TiDB `v8.5.7`, has single-node and durable topology paths, explicit readiness/identity markers, restart sequencing, and a fast capacity check. The TiKV descriptor-limit hardening is in the published ancestry (`a452220`); restart ordering is `67ca490`; top-level provider DDL serialization is `17c8bda`; the published restart-readiness change is `769ea08`. | Hosted run `35576142240`, `tidb` job `106258416149`, reached the durable 3PD/3TiKV topology and passed direct provider identity/schema, provider-clock/concurrency, and ambiguous-commit checks, but TiDB status readiness timed out after the frontend restart at 300 seconds. TiDB v8.5.7 logs showed `force-init-stats=true`; `769ea08` disables that optional startup gate for the next attempt. The documented local Docker host remains below the declared memory requirement (`8,232,747,008` versus `10,737,418,240`). | Obtain a terminal hosted pass with the published `force-init-stats=false` harness; retain PD/TiKV readiness, identity, frontend/store/PD restart, fresh-client and post-restart provider results; update `WORK_TRACKER.md` only after the complete result is available. | **4–8 h active**, plus **30–90 min hosted wall time** per attempt. | Local Docker capacity is below the declared topology requirement. Hosted runner scheduling and concurrent pushes can cancel a run before evidence is retained. |
| **W08.3** Provider time/fencing, ambiguous commits, concurrency and deployment durability | **Verifying — 80%** | The published TiDB hardening packet (`6327857`, published through `bbe8ebb`) adds provider-clock fencing races, concurrent publication, ambiguous-commit failure injection and durable-harness markers. The code maps known transaction conflicts to `EAGAIN`, stale leases to `ESTALE`, and uncertain commit outcomes to a distinct backend error. | Hosted run `35576142240`, `tidb` job `106258416149`, passed all three direct provider tests and `actual_tidb_commit_outcome_is_ambiguous_and_not_replayed` before the restart timeout. This is accepted evidence for provider time/fencing/concurrency and ambiguity semantics, but not for post-restart durability. | Retain a complete durable deployment/restart result; keep the unknown-commit outcome distinct from retryable statement conflicts and keep liveness probes separate from fsync/replication claims. | **2–5 h active**, plus **30–90 min hosted wall time**. | The remaining service lane depends on hosted TiDB/PD/TiKV restart behavior and queue stability. |
| **W08.4** Node, CLI, native-mount and macOS/Linux acceptance coverage | **Verifying — 85%** | Public Rust SDK/N-API and Rust/Node CLI configuration paths are wired for TiDB metadata with RustFS/S3-compatible `r2` chunks. Native integration scripts distinguish credentials, transport, mount, unmount and fresh-provider verification. | Hosted run `35574581481`, `tidb-rustfs` job `106253499553`, passed the corrected Linux FUSE native gate, including independent Rust/Node mounted I/O, clean unmount, fresh provider readback and retained client bytes. Node ARM and the standalone native-FUSE job also passed in that run. macOS NFS remains unclosed. | Close the umbrella only when W08.4a is retained and W08.4b has current Linux and macOS/native evidence, with no credentials, mount or post-unmount cleanup step silently skipped. | **1–3 h active**, mostly evidence reconciliation and any final fix. | This macOS host has no usable local FUSE device; hosted native jobs are required. CI concurrency can invalidate an otherwise useful run. |
| **W08.4a** Bounded Node/CLI consumer slice | **Landed bounded scope — 100%** | The provider matrix covers configuration, explicit `durable`, partial write, truncate, shutdown, reopen and owned RustFS-prefix cleanup. Key published chunks include `f55f2eb` (native Node consumer wiring), `5556872` (CI temp-target scoping), and `6038772` (supported PD bootstrap flags). Local tracker evidence records Rust SDK, CLI, N-API, Node matrix and CLI matrix passes with explicit credential-gated skips. | In hosted run `35571758453`, the live TiDB/RustFS Node SDK matrix passed all `8/8` cases and the CLI matrix passed all `16/16` cases before the native gate. This does not, by itself, close native acceptance or replicated durability. | No new implementation is currently required. Keep the live rows and bounded skips explicit; carry native and durable-service closure to W08.4b/W08.2/W08.3. | **0.5–1 h** for evidence maintenance. | Local credentials and a local TiDB/RustFS service are absent; the hosted lane supplies them. |
| **W08.4b** Native-mount and live TiDB/RustFS consumer acceptance | **Verifying — 85%** | The native gate verifies independent Rust and Node clients through the mounted Node SDK CLI path and performs a fresh provider read after unmount. Commit `71a7a4c` changed default native mount ownership to the current process identity, fixing the prior Linux FUSE `EACCES`; formatting cleanup followed in `6b75cea`. Commit `2221144` makes the CLI call `Filesystem.shutdown()` after a successful transport unmount, releasing the TiDB writer before fresh verification. | In `35571758453`/job `106244679000`, native FUSE mounted I/O passed but the fresh post-unmount TiDB open hit `EAGAIN`. In `35574581481`/job `106253499553`, the corrected native gate passed: mounted Rust/Node I/O, clean unmount, fresh provider readback and retained client bytes. That job later failed in the separate durable TiDB restart phase; macOS NFS is not yet retained for this live provider row. | Obtain current macOS NFS/native evidence and retain a non-cancelled run containing the corrected live TiDB/RustFS row. Do not infer macOS closure from Linux FUSE or from the later durable-service failure. | **1–3 h active** for follow-up, plus **10–30 min hosted wall time** per native attempt. | No local FUSE device or local live TiDB/RustFS services. Hosted macOS rows remain external gates. |
| **W08.5** TiDB metadata + RustFS S3 chunks | **Landed bounded scope — 100%** | The real single-node v8.5.7 service and pinned loopback RustFS endpoint passed mixed-provider seed, partial write, truncate, reopen, CAS/fencing and exact owned cleanup. Persisted fixtures require explicit volume/prefix/manifest scope and reject transient or symlink paths. | `TIDB_CHUNKED_RUSTFS_SEED_PASS` and reopen/cleanup evidence were retained in the real composition lane. This is accepted for the single-node bounded scope only. | No remaining action inside the bounded W08.5 scope. Replicated TiDB capacity and provider restart acceptance remain open under W08.2/W08.3. | **0–1 h** for documentation only. | None for the bounded single-node composition; broader durability is intentionally not inferred. |

## Evidence ledger

### Local implementation and compile evidence

These checks establish implementation/build state only. They do not substitute
for a real TiDB, RustFS, PD/TiKV restart, or native kernel mount:

| Check | Result | Boundary |
| --- | --- | --- |
| `./scripts/cargo-shared fmt --all -- --check` | PASS | Formatting; the normal shared target was overridden to an allowed `/private/tmp` target because of the local sandbox. |
| `CARGO_TARGET_DIR=/private/tmp/mount-rs-w08-cargo-target ./scripts/cargo-shared check --locked -p mount-rs-napi` | PASS | N-API compilation. |
| N-API library tests | PASS, 15 tests | Binding/lifecycle unit coverage. |
| TiDB library tests | PASS, 6 tests | Provider unit coverage. |
| Ignored TiDB acceptance binaries `tidb`, `ambiguous_commit`, `chunked_rustfs` | PASS compile-only | Does not claim that live services ran. |
| Shell and Node syntax checks | PASS | Script syntax only. |
| `git diff --check` | PASS | Patch hygiene. |
| Local `node examples/node-cli/index.mjs ...` consumer run | BLOCKED | This checkout currently has no built N-API binding (`Cannot find native binding`); hosted Node jobs are the functional evidence. |

### Hosted and native evidence

| Run/job | Result counted here | Evidence boundary |
| --- | --- | --- |
| CI `35571758453`, `tidb-rustfs` job `106244679000` | Partial pass, then FAIL | Real TiDB/RustFS topology, provider contract, seed, Node SDK matrix and CLI matrix passed; native FUSE I/O passed; fresh post-unmount TiDB writer acquisition failed with `EAGAIN`. The run is diagnostic, not a W08.4b pass. |
| CI `35571758453`, Node ARM job `106244679020` | PASS for its listed rows | Native Linux Node/FUSE and consumer coverage passed at the pre-`2221144` source; it does not prove the corrected fresh-provider teardown. |
| CI `35574581481`, `tidb-rustfs` job `106253499553` | Partial pass, then FAIL | The corrected native Linux FUSE gate passed, including fresh post-unmount provider readback and cleanup. The same job later failed at durable TiDB restart readiness, so the job is not a complete W08 acceptance pass. |
| CI `35576142240`, `tidb` job `106258416149` | FAIL after direct provider checks | Direct provider rows and ambiguous commit passed; TiDB frontend restart status readiness timed out after 300 seconds. The serialized DDL attempt did not resolve the restart gate. |
| CI `35577929196`, current main CI | QUEUED | Includes published `769ea08` (`--force-init-stats=false`) and the current main tip; no result is counted until the W08 jobs are terminal. |

The run status is intentionally recorded as a snapshot. A later ledger update
must replace `QUEUED` with the exact conclusion, job URL/ID, commit SHA,
and retained pass/fail markers rather than inferring success from queue state.

## Remaining action plan

1. Follow CI run `35577929196` (or its replacement after this ledger push)
   until the `tidb-rustfs` and `tidb` jobs have terminal results.
2. For W08.4b, retain the corrected Linux native result and obtain current
   macOS NFS/native evidence for the live TiDB/RustFS row.
3. For W08.2, accept only the complete durable PD/TiKV restart sequence with
   service identity, readiness, fresh-client and provider checks. Preserve a
   capacity-gated result as a blocker, not a pass. The next attempt includes
   the TiDB v8.5.7 stats-readiness fix from `769ea08`.
4. For W08.3, retain the hosted ambiguous-commit failure-injection outcome and
   keep its unknown-commit semantics distinct from retryable statement conflicts.
5. Update `WORK_TRACKER.md` and this ledger only after the evidence is terminal;
   do not close W08 while any required provider or platform gate is skipped,
   cancelled, failed, or only compile-tested.

## External blockers and boundaries

- The local Docker host cannot satisfy the durable 3PD/3TiKV memory requirement
  (`8,232,747,008` available versus `10,737,418,240` required). This is an
  environment capacity blocker, not permission to downgrade the topology.
- The local macOS host has no usable `/dev/fuse`/`fusermount3`, so Linux FUSE
  evidence must come from the hosted native job; macOS NFS evidence must remain
  a separate macOS row.
- Local TiDB/RustFS credentials and services are not present. The live provider
  rows are intentionally hosted and credential-gated rather than simulated.
- Concurrent pushes to `origin/main` have repeatedly cancelled queued CI runs.
  Runs `35577657951` and `35577758341` were superseded before their W08 jobs
  could produce evidence; a cancelled or superseded run is not acceptance
  evidence. Each ledger update must identify the exact terminal run and commit.
- Unrelated hosted failures (for example global Clippy, FoundationDB, or
  Windows parity jobs) are tracked as separate workstreams. They may make the
  aggregate workflow red, but they do not become W08 failures without a
  W08-relevant failing step.

## Session time log

Times below are approximate engineering/wall-clock accounting for this goal;
hosted CI wait is listed separately from active implementation time. They are
provisional and should be revised when the next terminal CI result is known.

| Time (UTC) | Activity | Active engineering estimate | Wait/external time | Result |
| --- | --- | ---: | ---: | --- |
| 2026-09-21 07:23–07:30 | Read the hosted native failure, traced `EAGAIN` to the CLI not releasing its owned `Filesystem` after transport unmount, and checked the ownership contract. | ~20 min | ~0 min | Root cause identified; no TTL/fencing weakening proposed. |
| 2026-09-21 07:30–07:35 | Patched `examples/node-cli/index.mjs`, ran syntax and diff checks, fetched concurrent `origin/main`, committed `2221144`, and pushed to `origin/main`. | ~15 min | ~2 min | Lifecycle fix published. |
| 2026-09-21 07:35–07:41 | Located hosted CI run `35573697620`, observed the replacement jobs, and separated W08-relevant rows from unrelated failures. | ~5 min | ~6 min | Corrected run in progress; no new pass claimed. |
| 2026-09-21 07:41–07:50 | Wrote and published the initial detailed W08 ledger as `fac9c7e`, then followed the hosted replacement run. | ~15–25 min | ~10 min CI wait | Ledger saved under `docs/`; snapshot later superseded by terminal job results. |
| 2026-09-21 07:50–08:06 | Read the hosted restart diagnostics, confirmed direct provider and ambiguous-commit passes, isolated the repeatable TiDB restart timeout, and serialised top-level provider tests in `17c8bda`. | ~20 min | ~10 min hosted wait | DDL serialization was published in merge tip `a6e870c`; the next durable run still reproduced the timeout. |
| 2026-09-21 08:07–08:24 | Inspected the TiDB v8.5.7 startup path and confirmed `force-init-stats` withholds service until statistics initialization completes; added `--force-init-stats=false` in `769ea08`, merged concurrent main updates, and pushed. | ~20 min | ~10 min CI/queue wait | Restart-readiness fix is published; hosted verification is still queued. |
| 2026-09-21 08:24–08:30 | Reconciled run `35574581481` native pass, run `35576142240` restart failure, and the superseded queue; refreshed this ledger against `origin/main` `6d59d20`. | ~10 min | ~5 min queue observation | W08 remains open pending the next terminal durable and macOS/native results. |
| Prior goal phase before this ledger request | TiDB/RustFS harness hardening, native process-identity fix, TiDB/TiKV descriptor and bootstrap fixes, hosted-log analysis and repeated CI queue monitoring. | **Substantial; exact active split not instrumented** | Goal telemetry previously reported roughly 2 h 41 min elapsed, including tool/CI waits | Implementation chunks were committed and pushed; W08 remains open pending hosted gates. |

## Update protocol

For each subsequent W08 chunk, append or revise the relevant row with:

- exact commit and `origin/main` SHA;
- exact command or hosted job ID, including exit/conclusion;
- separate implementation, provider, native and hosted status;
- remaining actions and provisional active/wall-time estimates; and
- any new blocker without converting a skip or cancellation into a pass.

Commit and push each validated ledger or implementation chunk to `origin/main`.
The workstream is complete only when the required W08.2, W08.3 and W08.4b
integration/provider/platform evidence is terminal and recorded in both this
ledger and `WORK_TRACKER.md`.
