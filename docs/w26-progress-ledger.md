# W26 progress ledger — Apache Ozone S3 backend

This ledger is the working record for the W26 Apache Ozone S3 backend
workstream. It distinguishes repository implementation, local evidence, and
hosted/native/provider acceptance. Estimates are provisional and are intended
for engineering planning, not a commitment.

## Snapshot

| Field | Current value |
| --- | --- |
| Workstream | W26 — Apache Ozone S3 backend |
| Ledger snapshot | 2026-09-21, Australia/Brisbane |
| Repository | `mount-rs` |
| Snapshot base | `210c9cd508b88112b645c8945cd64b1dc59b54cb` (`origin/main` after the W05 documentation push that retained the W26 isolation fix) |
| Checklist completion | 3 of 4 W26 tracker rows checked: 75% |
| Provisional execution completion | Approximately 80%; the remaining acceptance gate is material, so W26 is not complete |
| Current acceptance state | Local Ozone, SQLite/PGlite, single-node TiDB/Ozone, durable three-node FoundationDB/Ozone, hosted Ozone, hosted SQLite/PGlite, Node SDK, and CLI evidence passed; durable multi-node TiDB/Ozone hosted restart acceptance remains open after the bounded isolation fix was published and its first rerun was superseded by a concurrent main push |
| Latest hosted workflow | Replacement GitHub Actions run `35583109781` on `210c9cd` is in progress: `ozone` job `106280390339`, `ozone-compositions` job `106280390759`, `tidb` job `106280390711`, and `tidb-rustfs` job `106280390773`; superseded isolation run `35582936271` on `ecc1106` was canceled when `210c9cd` landed |
| Local Docker boundary | Docker Desktop capacity was about 5 CPUs and 8.2 GiB; this is sufficient for the durable FoundationDB proof but below the TiDB harness's 10 GiB durable-topology minimum |
| Release/acceptance decision | No release claim yet. W26 remains open until the hosted acceptance evidence is terminal and reviewed on the resulting revision |

## Work-item ledger

| Work item | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / gates |
| --- | --- | ---: | --- | --- | --- | --- |
| W26.1 — Ozone 2.2.1 gateway harness, digest pinning, health/bucket lifecycle | Implementation and local/hosted gateway gates complete | 100% | `scripts/test-ozone.sh` ran against the official pinned Ozone 2.2.1 image on arm64. Health, bucket creation, immutable and duplicate-object behavior, stale ETag, CAS, concurrent writers, range/full/missing reads, binary fixtures, fault window, bounded stopped-gateway behavior, restart/reopen, integration, and cleanup passed. Hosted run `35581168122`, job `106274147767`, passed the same Ozone gateway job on Linux-amd64. | Keep the nonsecure loopback limitation visible; retain the terminal hosted job as revision-specific evidence. | 0–1 h review | Hosted Linux runner and image architecture are external. Local and hosted harnesses are loopback/nonsecure and not production replicated durability. |
| W26.2 — immutable-object and fault/restart behavior | Implementation and local/hosted gateway gates complete | 100% | The arm64 Ozone run passed immutable/duplicate-object, stale-ETag, CAS, fault-window, stopped-gateway, restart/reopen, and cleanup checks. The failure timeout was widened to 15 seconds after a measured 5.6-second loaded Docker failure window (`62bc099`). Hosted Ozone job `106274147767` passed on `35581168122`. | Keep the provider/platform and production-durability boundaries explicit; do not convert unrelated canceled or queued jobs into acceptance. | 0–1 h review | Hosted CI is the provider/platform gate. No claim is made for production TLS, authentication, power loss, or a production replicated Ozone deployment. |
| W26.3a — SQLite and disk-backed PGlite composition over real Ozone | Local and hosted provider composition complete | 100% | `MOUNT_RS_OZONE_COMPOSITIONS=1 ./scripts/test-ozone-compositions.sh` passed both SQLite and disk-backed PGlite against live Ozone, including partial/truncate/reopen semantics, with `revision=17`. Hosted `ozone-compositions` job `106274147763` passed on `35581168122`; the run included the locked NAPI/PGlite and Node/CLI composition checks. | Keep the intentional Node-matrix skip and the native/provider boundaries visible. | 0–1 h review | Hosted Linux is required for portable CI evidence. PGlite is not a claim about every native Node packaging/runtime. |
| W26.3b — independent single-node TiDB composition over Ozone | Local smoke and contract complete; not replicated acceptance | 100% of this sub-item | Single-node TiDB/Ozone run passed TiDB identity/provider/fencing, ChunkedFs partial/truncate/CAS/stale-fencing/reopen, ambiguous commit, and cleanup. Evidence was explicitly labeled `single-node-smoke-not-replicated-acceptance`. | Preserve this as a lower-level contract signal only; it does not replace the durable hosted topology gate. | 0 h for current scope | Single-node TiDB is intentionally not a durability or failover claim. Local machine capacity is below the durable TiDB harness minimum. |
| W26.3c — independent durable three-node FoundationDB composition over Ozone | Local durable provider composition complete | 100% of this sub-item | The arm64 durable run used three fixed-address FoundationDB 7.4.7 containers, three coordinators, persistent per-server volumes, `double` redundancy and SSD storage. Transaction probes passed before and after node-2 restart; first-client and fresh-client Ozone compositions passed; `FOUNDATIONDB_TEST_PASS topology=durable`; owned resources were removed and verified absent. | Review the hosted FoundationDB lane only as an additional CI/provider signal; it is RustFS-backed, not a substitute for the local Ozone-specific composition evidence. | 0–1 h review | Local proof is loopback/nonsecure and does not cover production auth/TLS, power-loss behavior, or native mounting. Hosted FoundationDB is a separate provider topology. |
| W26.3d — durable multi-node TiDB composition over Ozone | Harness implementation complete; hosted rerun in progress | 80% | Hosted run `35581168122` on `63dbdbd`: direct TiDB identity/provider tests and standalone ambiguous-commit passed; `tidb-rustfs` also passed direct provider, `TIDB_CHUNKED_RUSTFS_SEED_PASS`, and its ambiguous-commit phase. Both jobs then timed out after 300 seconds at `TiDB phase=TiDB restart readiness`; the TiDB container stayed running but never exposed SQL/status. The bounded isolation fix was committed as `ecc1106`, moving dropped-COMMIT injection after durable restart/reopen. Its first rerun `35582936271` was canceled by the concurrent `210c9cd` push, so it is not evidence; replacement run `35583109781` is active with both TiDB jobs in progress. | Review replacement run `35583109781` on a terminal retained revision. Only paired terminal `tidb` and `tidb-rustfs` jobs with `TIDB_ACCEPTANCE evidence=durable-multinode-restart` close this item. | 1–4 h implementation/retry; 5–15 min result review | GitHub-hosted runner scheduling, concurrent pushes that cancel in-progress workflows, and the hosted Docker/TiDB/PD/TiKV environment. This gate cannot be accepted from local underprovisioned Docker. |
| W26.4a — Node provider matrix and Rust/Node CLI coverage | Implementation and local/hosted composition gate complete | 100% | The live arm64 composition run passed Node SDK coverage including PGlite-to-S3 partial/truncate/reopen (`pass=7 skip=1 fail=0`), Node CLI coverage, and the matching ignored Rust CLI live Ozone split-provider/reopen test. Hosted `ozone-compositions` job `106274147763` passed on `35581168122`. | Keep the Node matrix's one intentional skip and native/provider boundaries visible. | 0–1 h review | Hosted NAPI/PGlite build and Linux Node runtime are provider/native gates. Local arm64 evidence is not Linux-amd64 evidence. |
| W26.4b — CI wiring, pinned images, cleanup, and documentation | Implementation complete; W26-specific hosted rerun in progress | 100% implementation | CI installs NAPI/PGlite with locked Rust dependencies, runs the Ozone composition matrix, Node/CLI coverage, and durable TiDB/Ozone job. FoundationDB harness uses platform-specific pinned official 7.4.7 image manifests and transaction readiness probes. `tests/ozone/README.md`, `WORK_TRACKER.md`, and this ledger record the acceptance boundaries. Hosted Ozone and mixed composition jobs passed on `35581168122`; the TiDB jobs failed only at the first frontend restart gate. The isolation patch is present on `210c9cd`; replacement W26 jobs are active in `35583109781`. | Review the replacement W26 jobs together on a terminal revision and update the tracker with either the paired acceptance markers or the next exact failure boundary. | 0.5–1 h | GitHub workflow concurrency and other workstream pushes can cancel runs; hosted results are revision-specific. |

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
  On `35581168122`, the Ozone and mixed composition jobs are terminal green,
  while both TiDB jobs are terminal failures at the same first frontend
  restart gate. The isolation rerun `35582936271` was canceled by a newer
  main push; replacement run `35583109781` is active and is not yet evidence.
  W26 therefore remains open.
- Local Docker capacity is below the durable TiDB harness minimum. The local
  environment must not be used to manufacture a durable-TiDB result by
  overriding the capacity guard.

## Remaining-action checklist

- [x] Review workflow `35581168122` and record the terminal Ozone and mixed
  composition results plus the durable TiDB restart failure.
- [x] Move the dropped-COMMIT failure injection after durable restart/reopen,
  synchronize with the latest `origin/main`, and publish the bounded hosted
  gate; the first rerun was canceled by a concurrent push and is not treated
  as acceptance.
- [ ] Review the next terminal `ozone`, `ozone-compositions`, `tidb`, and
  `tidb-rustfs` jobs from replacement run `35583109781` together on the
  retained revision.
- [x] Update the W26 row in `WORK_TRACKER.md` with the exact terminal hosted
  run and the evidence-backed failure boundary.
- [x] Commit and push each completed chunk to `origin/main`; after every push,
  verify the remote revision and the resulting workflow state.
- [ ] Close W26 only when the tracker, hosted evidence, and provider-boundary
  notes agree. Until then, make no release/readiness claim.

## Provisional remaining effort and blockers

| Category | Estimate | Notes |
| --- | --- | --- |
| Hosted result inspection | 15–30 min | Review all W26-specific jobs from one retained revision, including logs and cleanup. |
| Durable TiDB/Ozone remediation | 0–4 engineering hours per evidenced failure | The restart-order fix is already pushed; further work should be driven by the next terminal log rather than speculation. |
| Tracker/ledger publication | 15–30 min per completed chunk | Includes fast-forwarding concurrent `origin/main` changes, `git diff --check`, commit, push, and remote verification. |
| External CI waiting | Unbounded wall-clock; not engineering time | GitHub runner queue, workflow concurrency, and concurrent pushes have repeatedly canceled otherwise useful runs. |
| Native/provider acceptance | Separate gate | Linux hosted NAPI/PGlite, TiDB/PD/TiKV, FoundationDB, and Ozone container behavior cannot be fully inferred from the local arm64 run. |

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
| 2026-09-21 — Hosted rerun reconciliation | Verified the canceled isolation run, fetched concurrent `origin/main` (`210c9cd`), and refreshed this ledger/tracker with the replacement W26 job IDs. | ~0.25 h | ~0.25 h hosted scheduling | W26 remains open while `35583109781` runs; no canceled result is promoted to evidence. |

## Publication record

This document is intentionally updated alongside `WORK_TRACKER.md`. The
ledger's percentages and estimates are snapshots; the tracker checkbox and
terminal hosted evidence remain authoritative for whether W26 is actually
complete.
