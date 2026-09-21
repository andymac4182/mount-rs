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
| Snapshot base | `9c098e58327c5851100aa0536e4939d5deb96a72` (`origin/main` after the final combined W25/W26 tree was verified) |
| Checklist completion | 4 of 4 W26 tracker rows checked: 100% |
| Provisional execution completion | W26 qualification: 100%; production-rollout readiness: 5% (tracking baseline only). Native, production-authentication, power-loss and other separately bounded gates remain open |
| Current acceptance state | Local Ozone, SQLite/PGlite, single-node TiDB/Ozone, durable three-node FoundationDB/Ozone, hosted Ozone, hosted SQLite/PGlite, Ozone-backed durable TiDB, generic durable TiDB, Node SDK, and CLI evidence passed; W26 is complete within its documented provider/platform boundaries |
| Latest hosted workflow | GitHub Actions run `35585066458` on `9c098e5`; W26 jobs `ozone` `106286459564`, `ozone-compositions` `106286459622`, and `ozone-tidb` `106286459540` completed successfully, with generic `tidb` job `106286459436` also green. Unrelated provider/native jobs are tracked separately and are not required to close W26. |
| Local Docker boundary | Docker Desktop capacity was about 5 CPUs and 8.2 GiB; this is sufficient for the durable FoundationDB proof but below the TiDB harness's 10 GiB durable-topology minimum |
| Production rollout track | Open, currently **NO-GO**; 0 of 15 production gates are terminally accepted. The 5% figure reflects the published tracking baseline, not deployable readiness |
| Release/acceptance decision | W26 qualification accepted within the documented local/hosted provider scope on terminal run `35585066458`; production rollout remains **NO-GO** and no broader native release claim is made |

## Work-item ledger

| Work item | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / gates |
| --- | --- | ---: | --- | --- | --- | --- |
| W26.1 — Ozone 2.2.1 gateway harness, digest pinning, health/bucket lifecycle | Implementation and local/hosted gateway gates complete | 100% | `scripts/test-ozone.sh` ran against the official pinned Ozone 2.2.1 image on arm64. Health, bucket creation, immutable and duplicate-object behavior, stale ETag, CAS, concurrent writers, range/full/missing reads, binary fixtures, fault window, bounded stopped-gateway behavior, restart/reopen, integration, and cleanup passed. Final hosted run `35585066458`, job `106286459564`, emitted `OZONE_HEALTHY`, `OZONE_READY`, `OZONE_INTEGRATION_PASS`, and `OZONE_CLEANUP_PASS` on Linux-amd64. | Keep the nonsecure loopback limitation visible; no further W26 gateway action remains. | 0 h acceptance; 0–1 h review | Hosted Linux runner and image architecture are external. Local and hosted harnesses are loopback/nonsecure and not production replicated durability. |
| W26.2 — immutable-object and fault/restart behavior | Implementation and local/hosted gateway gates complete | 100% | The arm64 Ozone run passed immutable/duplicate-object, stale-ETag, CAS, fault-window, stopped-gateway, restart/reopen, and cleanup checks. The failure timeout was widened to 15 seconds after a measured 5.6-second loaded Docker failure window (`62bc099`). Final hosted Ozone job `106286459564` passed on `35585066458`. | Keep the provider/platform and production-durability boundaries explicit; no further W26 action remains. | 0 h acceptance; 0–1 h review | Hosted CI is the provider/platform gate. No claim is made for production TLS, authentication, power loss, or a production replicated Ozone deployment. |
| W26.3a — SQLite and disk-backed PGlite composition over real Ozone | Local and hosted provider composition complete | 100% | `MOUNT_RS_OZONE_COMPOSITIONS=1 ./scripts/test-ozone-compositions.sh` passed both SQLite and disk-backed PGlite against live Ozone, including partial/truncate/reopen semantics, with `revision=17`. Final hosted `ozone-compositions` job `106286459622` passed on `35585066458`, including `SUMMARY node-sdk pass=7 skip=1 fail=0`, Ozone integration, and cleanup markers. | Keep the intentional Node-matrix skip and the native/provider boundaries visible. | 0 h acceptance; 0–1 h review | Hosted Linux is required for portable CI evidence. PGlite is not a claim about every native Node packaging/runtime. |
| W26.3b — independent single-node TiDB composition over Ozone | Local smoke and contract complete; not replicated acceptance | 100% of this sub-item | Single-node TiDB/Ozone run passed TiDB identity/provider/fencing, ChunkedFs partial/truncate/CAS/stale-fencing/reopen, ambiguous commit, and cleanup. Evidence was explicitly labeled `single-node-smoke-not-replicated-acceptance`. | Preserve this as a lower-level contract signal only; it does not replace the durable hosted topology gate. | 0 h for current scope | Single-node TiDB is intentionally not a durability or failover claim. Local machine capacity is below the durable TiDB harness minimum. |
| W26.3c — independent durable three-node FoundationDB composition over Ozone | Local durable provider composition complete | 100% of this sub-item | The arm64 durable run used three fixed-address FoundationDB 7.4.7 containers, three coordinators, persistent per-server volumes, `double` redundancy and SSD storage. Transaction probes passed before and after node-2 restart; first-client and fresh-client Ozone compositions passed; `FOUNDATIONDB_TEST_PASS topology=durable`; owned resources were removed and verified absent. | Review the hosted FoundationDB lane only as an additional CI/provider signal; it is RustFS-backed, not a substitute for the local Ozone-specific composition evidence. | 0–1 h review | Local proof is loopback/nonsecure and does not cover production auth/TLS, power-loss behavior, or native mounting. Hosted FoundationDB is a separate provider topology. |
| W26.3d — durable multi-node TiDB composition over Ozone | Harness and hosted acceptance complete | 100% | Final hosted run `35585066458`, Ozone-specific job `106286459540`, used `MOUNT_RS_OZONE_TIDB_COMPOSITION=1`, durable TiDB v8.5.7, and the real Ozone gateway. It emitted `TIDB_CHUNKED_RUSTFS_SEED_PASS`, `TIDB_CHUNKED_RUSTFS_REOPEN_PASS`, `TIDB_ACCEPTANCE evidence=durable-multinode-restart topology=durable version=v8.5.7 platform=linux/amd64 cpus=4 mem_bytes=16765378560 ambiguous_commit=pass`, `OZONE_INTEGRATION_PASS`, and `OZONE_CLEANUP_PASS`. The generic durable TiDB job `106286459436` also emitted the same acceptance marker. | None for the requested W26 scope; retain the provider and production-boundary notes. | 0 h acceptance; 0–0.5 h evidence review | GitHub-hosted Docker/TiDB/PD/TiKV and Ozone are hosted provider gates. This does not claim production TLS/authentication, power-loss durability, or native mounting. |
| W26.4a — Node provider matrix and Rust/Node CLI coverage | Implementation and local/hosted composition gate complete | 100% | The live arm64 composition run passed Node SDK coverage including PGlite-to-S3 partial/truncate/reopen (`pass=7 skip=1 fail=0`), Node CLI coverage, and the matching ignored Rust CLI live Ozone split-provider/reopen test. Final hosted `ozone-compositions` job `106286459622` passed on `35585066458` with `SUMMARY node-sdk pass=7 skip=1 fail=0`. | Keep the Node matrix's one intentional skip and native/provider boundaries visible. | 0 h acceptance; 0–1 h review | Hosted NAPI/PGlite build and Linux Node runtime are provider/native gates. Local arm64 evidence is not Linux-amd64 evidence. |
| W26.4b — CI wiring, pinned images, cleanup, and documentation | Implementation and hosted provider gates complete | 100% | CI installs NAPI/PGlite with locked Rust dependencies, runs the Ozone composition matrix, Node/CLI coverage, and durable Ozone/TiDB job. FoundationDB harness uses platform-specific pinned official 7.4.7 image manifests and transaction readiness probes. `tests/ozone/README.md`, `WORK_TRACKER.md`, and this ledger record the acceptance boundaries. Final W26 jobs `ozone` `106286459564`, `ozone-compositions` `106286459622`, and `ozone-tidb` `106286459540` passed on `35585066458`. | None for the requested W26 scope. | 0 h acceptance; 0–0.5 h evidence review | GitHub workflow concurrency and other workstream pushes can cancel runs; W26 acceptance is based only on the terminal job results on the retained revision. |

## Production rollout readiness — post-demo track

The successful demo and W26 hosted packet prove the requested qualification
scope; they do not authorize a production rollout. This section is a separate
workstream with its own completion percentages, evidence, owners/gates and
exit criteria. A production gate can only move to complete when the evidence is
from the named production-like environment, on a retained revision, with the
provider/native boundary stated explicitly. Local Docker, demo behavior,
non-secure loopback services, and a green hosted fixture are useful
qualification evidence but are not production approval.

### Current production decision

| Decision | Status | Evidence now available | Exit condition |
| --- | --- | --- | --- |
| Production rollout | **NO-GO** | W26 qualification is green on hosted run `35585066458`, but the Ozone deployment is non-secure/loopback and the provider tests do not prove production authentication, power-loss durability, DR, native mounting, capacity, or operational sign-off | All P0–P14 rows below have terminal evidence or an explicitly approved, documented non-goal; the release owner records GO/NO-GO against the same revision |
| Production target | Not defined | No named production topology, SLOs, RPO/RTO, supported metadata provider, traffic envelope, or on-call owner is recorded in the W26 packet | Product and operations name the target environment, support matrix, SLOs, RPO/RTO, owners and rollback authority |
| Qualification baseline | Complete for W26 only | Revision `9c098e5` and hosted run `35585066458` are the retained W26 acceptance packet | Any production implementation change gets a fresh qualification and production evidence packet on the tested revision |

### Production gate ledger

| Gate / work item | Work type | Status | Completion | Evidence now available | Remaining actions / exit evidence | Provisional engineering time | External blockers / hosted or native gates |
| --- | --- | --- | ---: | --- | --- | ---: | --- |
| P0 — production scope, support matrix, SLO/RPO/RTO and ownership | Implementation / operations | Open — tracking baseline published | 10% | W26 provider boundaries and the explicit non-production decision are documented; no production target or service objectives are approved | Name the supported Ozone version/topology, metadata providers, traffic/capacity envelope, SLOs, RPO/RTO, on-call owner, rollback authority and approved non-goals; publish the signed baseline | 0.5–1.5 d | Product and operations decisions; provider support commitments; no code can close this gate alone |
| P1 — production Ozone topology and deployment rehearsal | Hosted/provider | Not started | 0% | Current Ozone evidence is a pinned all-in-one, non-secure, loopback test deployment with anonymous volumes and no production replication claim | Provision a production-like multi-node Ozone/SCM/OM topology, pinned images, persistent storage, network policy, capacity limits and repeatable deploy/destroy/rehearsal scripts; capture clean-client health and failover evidence | 3–7 d | Ozone/cluster infrastructure, persistent storage, image architecture, network access and environment credentials |
| P2 — production metadata-provider support matrix | Hosted/provider | Open — qualification only | 10% | SQLite, disk-backed PGlite, single-node TiDB and durable FoundationDB/TiDB compositions have separate local/hosted qualification evidence | Select the production-supported provider set; define version/upgrade policy, HA topology, failure semantics and unsupported combinations; run each selected provider through the secure staging matrix | 1–3 d | Provider versions, managed-service access, capacity and operator support; local composition is not a production approval |
| P3 — authentication, TLS, secret lifecycle and redaction | Implementation / hosted/provider | Not started | 0% | W26 gateway tests intentionally use non-secure loopback access; no production credential or TLS evidence is claimed | Implement and test certificate validation/rotation, service and client authentication, secret injection, least privilege, log redaction, expiry/revocation and clean-client negative cases | 2–5 d | Security review, certificate/secret manager, identity provider, Ozone Kerberos/TLS configuration and production network policy |
| P4 — replicated block durability and storage failure protection | Hosted/provider | Not started | 0% | Restart/reopen checks passed in bounded test topologies; no power-loss, disk-loss, storage-corruption or production replication guarantee exists | Define and test replication, fsync/barrier assumptions, disk/node loss, corrupt/torn block handling, object integrity, capacity exhaustion and recovery on the actual storage class; retain before/after hashes and namespace evidence | 3–7 d | Production storage, failure-injection controls, host access and provider durability semantics; SIGKILL/container restart is not automatically power-loss evidence |
| P5 — fencing, ambiguous commit and stale-writer recovery under failover | Implementation / hosted/provider | Partial qualification | 25% | W26 exercises CAS, stale fencing, ambiguous commit and durable restart in bounded provider compositions; hosted TiDB marker reports `ambiguous_commit=pass` | Repeat the matrix over secure multi-node production topology, node loss, network delay/partition and client retry; prove no stale publication, duplicate block, lost acknowledged commit or split-brain writer after recovery | 2–5 d | Distributed test controls, provider failover behavior, multiple clients and network fault tooling |
| P6 — backup, restore, disaster recovery and retention | Operations / hosted/provider | Not started | 0% | Existing versioning design distinguishes application-consistent backup concerns, but W26 has no production backup/restore or DR packet | Define backup contents and consistency fence, encrypt/store copies, restore into clean infrastructure, verify hashes/revisions/metadata, measure RPO/RTO, test retention/deletion and document regional/provider loss procedure | 3–6 d | Backup destination, KMS, second failure domain/region, restore capacity and operator access |
| P7 — observability, alerts, dashboards and runbooks | Implementation / operations | Not started | 0% | W26 emits bounded harness markers and cleanup evidence; these are test evidence, not production SLO telemetry or alerting | Add redacted structured logs, metrics/traces for gateway/provider latency/errors, queue/retry/fencing/recovery states, health/readiness, dashboards, alert thresholds, escalation and operator runbooks; test alert delivery | 2–5 d | Collector/monitoring provider, alert routing, SLO ownership and external-collector acceptance; W30 evidence is separate until connected to this topology |
| P8 — load, capacity, soak and cost envelope | Hosted/provider | Not started | 0% | No sustained production-shaped load, throughput/latency budget, concurrency limit, cost envelope or soak result is in the W26 packet | Define representative object/chunk/metadata workload, run load and multi-day soak, measure p50/p95/p99, memory/CPU/storage growth, throttling and recovery; record safe capacity and scaling trigger | 3–7 d | Dedicated environment, traffic generator, monitoring, service quotas, cost budget and stable provider capacity |
| P9 — upgrade, rollback and compatibility | Implementation / operations | Not started | 0% | W26 pins Ozone 2.2.1 and provider fixture versions for qualification only; no production migration rehearsal is recorded | Test forward/backward compatibility, schema/object compatibility, rolling upgrade, failed upgrade rollback, image/digest provenance, lockfile/release reproducibility and downgrade boundaries on restored data | 2–5 d | Maintained provider versions, release artifact signing, change window and operator approval |
| P10 — security, privacy, tenancy and audit review | Implementation / hosted/provider | Not started | 0% | No production threat model, tenant isolation, audit-log, vulnerability/dependency or security sign-off is attached to W26 | Review trust boundaries, authorization/isolation, data classification, encryption, audit retention, abuse/rate limits, dependency/image provenance and incident response; close findings or record approved exceptions | 2–5 d | Security reviewer, identity/tenant model, compliance requirements and scanning infrastructure |
| P11 — native client, mount and platform qualification | Native/provider | Not started | 0% | W26 covers Rust/Node CLI and hosted Linux composition; it does not claim FUSE/NFS/FSKit/Windows mount or native runtime acceptance | Decide supported client/platform matrix, then run clean native installs, mount/unmount, concurrent access, locks, restart/recovery, package signing and negative capability tests on every advertised platform | 3–8 d per included platform | macOS/Linux/Windows hosts, privileged mount facilities, native CI runners, signing/notarization and provider connectivity |
| P12 — release packaging, CI promotion, canary and rollback automation | Implementation / hosted | Open — qualification CI exists | 10% | W26 has pinned-image hosted jobs and terminal markers on `35585066458`; that pipeline is not a production promotion or canary control | Produce signed versioned artifacts, SBOM/provenance, environment promotion checks, migration gates, canary/rollback automation, protected approvals and a retained release evidence packet | 2–5 d | CI/CD permissions, artifact registry, signing keys, deployment platform and change-management policy |
| P13 — incident, failover and recovery rehearsal | Operations / hosted/provider | Not started | 0% | W26 restart and cleanup tests are bounded qualification checks, not an operator incident rehearsal | Run timed exercises for gateway loss, metadata-provider loss, stale client, storage exhaustion, bad deploy, credential expiry and restore; verify paging, runbooks, RTO, data integrity and post-incident evidence | 2–5 d | On-call participants, production-like failure controls, paging/incident tooling and maintenance window |
| P14 — final launch review and go/no-go | Operations / release | Not started | 0% | Current decision is explicitly NO-GO with the open rows above | Audit every gate on one retained revision, attach provider/native/hosted evidence, record known limitations and approvals, execute canary exit criteria, and obtain release-owner GO or documented NO-GO | 0.5–1.5 d | Release owner, product/operations/security sign-off and all upstream gates |

### Production rollout phases and provisional effort

The following plan is provisional engineering time, not a delivery promise. It
excludes provider provisioning, CI queues, approvals, maintenance windows and
other elapsed wall-clock gates.

| Phase | Gates | Provisional engineering effort | Exit |
| --- | --- | ---: | --- |
| Define | P0 | 0.5–1.5 d | Approved target, support matrix, SLO/RPO/RTO, owners and non-goals |
| Secure staging | P1–P4 | 9–22 d | Repeatable secure Ozone/provider topology with auth, TLS, secrets and durability evidence |
| Resilience and operations | P5–P7, P13 | 9–21 d | Failure, backup/restore, telemetry and incident-recovery evidence meets the approved objectives |
| Capacity and release | P8–P12 | 12–30 d | Load/soak, upgrade/rollback, security, native and canary/release gates pass for the advertised matrix |
| Launch decision | P14 | 0.5–1.5 d | Evidence-backed GO or an explicit NO-GO with blockers and owners |
| **Total provisional engineering range** | **P0–P14** | **31–76 d, plus external waits** | **Not currently scheduled or committed; refine after P0 decisions** |

### Production evidence rules

- The W26 completion percentage and the production percentage are separate.
  W26 remains 100% complete within its requested qualification scope; the
  production track remains open until the gates above close.
- A local demo, a green hosted fixture, or a provider restart test cannot be
  promoted to production evidence without the target topology, security
  posture, capacity envelope and operator/recovery context being recorded.
- Provider acceptance, native-platform acceptance, security review and release
  approval are independent gates. A passing implementation test does not close
  any of those external gates.
- Every production result must identify the exact revision, image/provider
  versions, topology, test start/end, terminal status, redacted markers and
  cleanup/rollback outcome. Queued, skipped, canceled or failed jobs remain
  non-evidence.
- Missing credentials, infrastructure, certificates, provider access, native
  runners or approvers are recorded as blockers; they are not worked around by
  synthesizing local evidence or weakening the gate.

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
  The historical failed run `35581168122` and canceled reruns remain recorded
  as diagnosis only. Final run `35585066458` has terminal green W26 jobs on
  `9c098e5`: `ozone`, `ozone-compositions`, and `ozone-tidb`, with generic
  durable `tidb` also green. The separate RustFS/native lane is not required
  for W26's Ozone acceptance.
- Local Docker capacity is below the durable TiDB harness minimum. The local
  environment must not be used to manufacture a durable-TiDB result by
  overriding the capacity guard.
- The production rollout remains NO-GO. The non-secure loopback Ozone service,
  hosted fixture jobs, and demo behavior are not evidence of production TLS,
  authentication, replicated/power-loss durability, backup/restore, native
  mounting, capacity, security or operational readiness.

## Remaining-action checklist

- [x] Review workflow `35581168122` and record the terminal Ozone and mixed
  composition results plus the durable TiDB restart failure as historical
  diagnosis.
- [x] Move the dropped-COMMIT failure injection after durable restart/reopen,
  synchronize with the latest `origin/main`, and publish the bounded hosted
  gate; the first rerun was canceled by a concurrent push and is not treated
  as acceptance.
- [x] Review final run `35585066458`: terminal `ozone`,
  `ozone-compositions`, `ozone-tidb`, and generic `tidb` jobs all passed on
  the retained revision; keep the separate RustFS/native lane distinct.
- [x] Update the W26 row in `WORK_TRACKER.md` with the exact terminal hosted
  run and the evidence-backed acceptance markers.
- [x] Commit and push each completed chunk to `origin/main`; after every push,
  verify the remote revision and the resulting workflow state.
- [x] Close W26 when the tracker, hosted evidence, and provider-boundary
  notes agree. W26 is complete within the documented scope; no broader
  production or native-platform readiness claim is made.

### Production rollout checklist (open)

- [ ] P0: approve the production target, supported provider/platform matrix,
  SLOs, RPO/RTO, owners and non-goals.
- [ ] P1–P2: provision and rehearse the secure production-like Ozone topology
  and select the metadata providers that are actually supported in production.
- [ ] P3–P4: close authentication/TLS/secrets and replicated storage durability
  evidence, including storage/node failure and integrity recovery.
- [ ] P5–P7: close failover/fencing, backup/restore/DR, observability, alerts
  and operator runbooks against the approved objectives.
- [ ] P8–P10: close load/soak/capacity, upgrade/rollback and security/privacy
  review for the advertised workload and tenancy model.
- [ ] P11: run the native client/mount matrix for every platform that will be
  advertised; keep unsupported platforms explicitly out of the release.
- [ ] P12–P13: produce signed artifacts, promotion/canary controls and execute
  incident/recovery rehearsals.
- [ ] P14: perform the final evidence audit and record an explicit GO or NO-GO.

## Provisional remaining effort and blockers

| Category | Estimate | Notes |
| --- | --- | --- |
| Hosted result inspection | 0 h acceptance; 15–30 min record maintenance | Final W26-specific jobs are green on `35585066458`; any future review is maintenance only. |
| Durable TiDB/Ozone remediation | 0 h for W26 acceptance | The restart isolation fix is validated by the Ozone-backed durable acceptance marker. Separate production/native/provider expansion would be a new scope. |
| Tracker/ledger publication | 15–30 min for this closeout chunk | Includes fast-forwarding concurrent `origin/main` changes, `git diff --check`, commit, push, and remote verification. |
| External CI waiting | Unbounded wall-clock; not engineering time | GitHub runner queue, workflow concurrency, and concurrent pushes have repeatedly canceled otherwise useful runs. |
| Native/provider acceptance | Separate gate | Linux hosted NAPI/PGlite, TiDB/PD/TiKV, FoundationDB, and Ozone container behavior cannot be fully inferred from the local arm64 run. |
| Production rollout definition (P0) | 0.5–1.5 d engineering | Production target, support matrix, SLO/RPO/RTO, owners and non-goals are not yet approved. |
| Secure provider/staging gates (P1–P4) | 9–22 d engineering | Requires production-like Ozone/storage/provider environments, identity/certificates, secret management and external capacity. |
| Resilience, DR and operations (P5–P7, P13) | 9–21 d engineering | Requires fault controls, backup destination, monitoring/paging and operator participation; elapsed waits are separate. |
| Capacity, security, native and release (P8–P12) | 12–30 d engineering | Scope depends on advertised platforms/providers and requires hosted/native runners, signing, security and deployment access. |
| Production launch decision (P14) | 0.5–1.5 d engineering | Cannot close until upstream evidence and named release-owner approvals exist. |
| **Production track total** | **31–76 d engineering plus external waits** | Provisional planning range only; refine after P0 is approved. |

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
| 2026-09-21 — Hosted rerun reconciliation | Verified the canceled isolation run, fetched concurrent `origin/main` (`210c9cd`), and refreshed this ledger/tracker with the replacement W26 job IDs. | ~0.25 h | ~0.25 h hosted scheduling | W26 remained open until a terminal retained revision was available; no canceled result was promoted to evidence. |
| 2026-09-21 — Final hosted W26 acceptance | Reviewed run `35585066458` on `9c098e5`: hosted Ozone, SQLite/PGlite composition, Ozone-backed durable TiDB, and generic durable TiDB all passed their terminal jobs and redacted success markers. | ~0.25 h | ~1 h hosted queue/retries | W26 acceptance is complete within scope; unrelated native/RustFS jobs remain separate gates. |
| 2026-09-21 — W26 closeout publication | Fast-forwarded the combined tree and updated this ledger plus `WORK_TRACKER.md` with the final run, job IDs, completion percentages, evidence, boundaries, estimates, and session record. | ~0.5 h | ~0.25 h log retrieval | Closeout commit `b7e2758` was pushed to `origin/main`; no implementation action remains for W26. |
| 2026-09-21 — Production rollout tracking expansion | Added the separate P0–P14 production gate matrix, current NO-GO decision, implementation versus hosted/provider/native boundaries, provisional phase estimates, external blockers, open checklist and rollout evidence rules. | ~0.75 h | ~0.25 h remote reconciliation/push | W26 qualification remains accepted; production readiness is explicitly open and must be requalified on the retained implementation revision. |

## Publication record

This document is intentionally updated alongside `WORK_TRACKER.md`. The
ledger's percentages and estimates are snapshots; the tracker checkbox and
terminal hosted evidence remain authoritative for whether W26 is actually
complete.

The authoritative W26 hosted packet is run `35585066458` on tested revision
`9c098e5`; the documentation closeout was subsequently rebased and pushed as
`b7e2758` over unrelated mainline changes. Later unrelated pushes do not turn
the terminal W26 provider jobs into queued or canceled evidence.

The production rollout track is intentionally separate from that packet. Its
current decision is **NO-GO** with 0 of 15 P0–P14 gates terminally accepted;
the open gate ledger above is the source of truth for production work, estimates
and blockers.
