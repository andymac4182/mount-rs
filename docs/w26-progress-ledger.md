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
| Snapshot base | `35ffbfabaea2d822dfabd4023179851e2c9d57b0` (`origin/main` when this ledger was prepared) |
| Checklist completion | 3 of 4 W26 tracker rows checked: 75% |
| Provisional execution completion | Approximately 80%; the remaining acceptance gate is material, so W26 is not complete |
| Current acceptance state | Local Ozone, SQLite/PGlite, single-node TiDB/Ozone, durable three-node FoundationDB/Ozone, Node SDK, and CLI evidence passed; durable multi-node TiDB/Ozone hosted evidence is still running |
| Active hosted workflow | GitHub Actions run `35575442663`; `ozone-tidb` job `106256211434` was in progress at snapshot time |
| Local Docker boundary | Docker Desktop capacity was about 5 CPUs and 8.2 GiB; this is sufficient for the durable FoundationDB proof but below the TiDB harness's 10 GiB durable-topology minimum |
| Release/acceptance decision | No release claim yet. W26 remains open until the hosted acceptance evidence is terminal and reviewed on the resulting revision |

## Work-item ledger

| Work item | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / gates |
| --- | --- | ---: | --- | --- | --- | --- |
| W26.1 — Ozone 2.2.1 gateway harness, digest pinning, health/bucket lifecycle | Implementation and local gate complete | 100% | `scripts/test-ozone.sh` ran against the official pinned Ozone 2.2.1 image on arm64. Health, bucket creation, immutable and duplicate-object behavior, stale ETag, CAS, concurrent writers, range/full/missing reads, binary fixtures, fault window, bounded stopped-gateway behavior, restart/reopen, integration, and cleanup passed. | Review the terminal hosted Linux result on the revision that is ultimately retained; keep the nonsecure loopback limitation visible. | 0–1 h review | Hosted Linux runner and image architecture are external. Local evidence is loopback, nonsecure, and not production replicated durability. |
| W26.2 — immutable-object and fault/restart behavior | Implementation and local gate complete | 100% | The arm64 Ozone run passed immutable/duplicate-object, stale-ETag, CAS, fault-window, stopped-gateway, restart/reopen, and cleanup checks. The failure timeout was widened to 15 seconds after a measured 5.6-second loaded Docker failure window (`62bc099`). | Confirm the hosted job is green on a terminal revision; do not convert a canceled or queued run into acceptance. | 0–1 h review | Hosted CI is the provider/platform gate. No claim is made for production TLS, authentication, power loss, or a production replicated Ozone deployment. |
| W26.3a — SQLite and disk-backed PGlite composition over real Ozone | Local provider composition complete | 100% | `MOUNT_RS_OZONE_COMPOSITIONS=1 ./scripts/test-ozone-compositions.sh` passed both SQLite and disk-backed PGlite against live Ozone, including partial/truncate/reopen semantics, with `revision=17`. The run ended with `OZONE_INTEGRATION_PASS`, `OZONE_CLEANUP_PASS`, and composition cleanup. | Review hosted `ozone-compositions` output on the retained revision. | 0–1 h review | Hosted Linux is required for portable CI evidence. PGlite is not a claim about every native Node packaging/runtime. |
| W26.3b — independent single-node TiDB composition over Ozone | Local smoke and contract complete; not replicated acceptance | 100% of this sub-item | Single-node TiDB/Ozone run passed TiDB identity/provider/fencing, ChunkedFs partial/truncate/CAS/stale-fencing/reopen, ambiguous commit, and cleanup. Evidence was explicitly labeled `single-node-smoke-not-replicated-acceptance`. | Preserve this as a lower-level contract signal only; it does not replace the durable hosted topology gate. | 0 h for current scope | Single-node TiDB is intentionally not a durability or failover claim. Local machine capacity is below the durable TiDB harness minimum. |
| W26.3c — independent durable three-node FoundationDB composition over Ozone | Local durable provider composition complete | 100% of this sub-item | The arm64 durable run used three fixed-address FoundationDB 7.4.7 containers, three coordinators, persistent per-server volumes, `double` redundancy and SSD storage. Transaction probes passed before and after node-2 restart; first-client and fresh-client Ozone compositions passed; `FOUNDATIONDB_TEST_PASS topology=durable`; owned resources were removed and verified absent. | Review the hosted FoundationDB lane only as an additional CI/provider signal; it is RustFS-backed, not a substitute for the local Ozone-specific composition evidence. | 0–1 h review | Local proof is loopback/nonsecure and does not cover production auth/TLS, power-loss behavior, or native mounting. Hosted FoundationDB is a separate provider topology. |
| W26.3d — durable multi-node TiDB composition over Ozone | Harness implementation and recovery fix complete; hosted acceptance in progress | 80% | The hosted job previously reached all direct TiDB/Ozone contract and ambiguous-commit checks, then failed during durable restart recovery after PD/TiDB readiness timed out (`35571758453`, job `106244678984`). The restart sequence was bounded and reordered to recover TiDB before TiKV and PD (`67ca490`). A fresh hosted run is active: workflow `35575442663`, job `106256211434`. | Wait for the active job to become terminal; inspect the full durable restart and post-restart composition output; if green, record the run and close W26.3. If it fails, fix only the evidenced failure, rerun, and retain the failure boundary. | 1–4 h implementation/retry; 5–15 min result review | GitHub-hosted runner scheduling, concurrent pushes that cancel in-progress workflows, and the hosted Docker/TiDB/PD/TiKV environment. This gate cannot be accepted from local underprovisioned Docker. |
| W26.4a — Node provider matrix and Rust/Node CLI coverage | Implementation and local gate complete | 100% | The live arm64 composition run passed Node SDK coverage including PGlite-to-S3 partial/truncate/reopen (`pass=7 skip=1 fail=0`), Node CLI coverage, and the matching ignored Rust CLI live Ozone split-provider/reopen test. | Confirm the hosted `ozone-compositions` job on a terminal revision; keep the Node matrix's one intentional skip visible. | 0–1 h review | Hosted NAPI/PGlite build and Linux Node runtime are provider/native gates. Local arm64 evidence is not Linux-amd64 evidence. |
| W26.4b — CI wiring, pinned images, cleanup, and documentation | Implementation complete; revision-specific hosted result pending | 100% implementation | CI installs NAPI/PGlite with locked Rust dependencies, runs the Ozone composition matrix, Node/CLI coverage, and durable TiDB/Ozone job. FoundationDB harness uses platform-specific pinned official 7.4.7 image manifests and transaction readiness probes. `tests/ozone/README.md` and `WORK_TRACKER.md` record the acceptance boundaries. | Publish the ledger and any final tracker update, then retain a single terminal hosted result for the acceptance record. | 0.5–1 h | GitHub workflow concurrency and other workstream pushes can cancel runs; hosted results are revision-specific. |

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
  The active `ozone-tidb` job at ledger preparation time is therefore reported
  as in progress, not as evidence of completion.
- Local Docker capacity is below the durable TiDB harness minimum. The local
  environment must not be used to manufacture a durable-TiDB result by
  overriding the capacity guard.

## Remaining-action checklist

- [ ] Wait for workflow `35575442663` / job `106256211434` and inspect the
  durable TiDB/Ozone restart and post-restart composition output.
- [ ] If that workflow is canceled by a newer push, synchronize with the
  latest `origin/main` and rerun the bounded hosted gate; do not report the
  canceled run as acceptance.
- [ ] Review the terminal `ozone`, `ozone-compositions`, and `ozone-tidb`
  jobs together on the retained revision.
- [ ] Update the W26 row in `WORK_TRACKER.md` with the exact terminal hosted
  run, or with the next evidence-backed failure and its bounded follow-up.
- [ ] Commit and push each completed chunk to `origin/main`; after every push,
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
| 2026-09-21 — Hosted TiDB/Ozone diagnosis and recovery fix | Inspected the first hosted failure, identified restart ordering around PD/TiDB readiness, changed the bounded recovery sequence, and published `67ca490`. | ~1 h | ~2+ h CI queue/canceled retries | Fix is published; terminal validation remains open. |
| 2026-09-21 — Ledger preparation | Captured the current W26 inventory, evidence boundaries, provisional estimates, blockers, and remaining actions in this document. | ~0.25 h | 0 h | Ledger ready for its publication chunk. |

## Publication record

This document is intentionally updated alongside `WORK_TRACKER.md`. The
ledger's percentages and estimates are snapshots; the tracker checkbox and
terminal hosted evidence remain authoritative for whether W26 is actually
complete.
