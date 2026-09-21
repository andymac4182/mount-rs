# W08 TiDB workstream progress ledger

Status snapshot: **2026-09-21 10:50 UTC / 20:50 AEST**
Repository: `andymac4182/mount-rs`  
Functional evidence tip (before this documentation chunk): `origin/main` at
`bb27fe0`
Authoritative W08 hosted evidence: CI run `35585066458`, source `9c098e5`
(W08-relevant jobs all terminal success)

This ledger is the detailed working record for W08 in `WORK_TRACKER.md`. It
separates implementation completion from real provider, durable-service,
native-mount, and hosted-platform acceptance. A checked tracker item is not
treated as proof for a broader unchecked gate. Percentages and time estimates
are provisional planning values, not release-readiness measurements.

## Summary

W08 is **100% complete for the defined workstream acceptance scope**. The
implementation, real durable TiDB/PD/TiKV restart sequence, provider
time/fencing and ambiguous-commit semantics, live Linux TiDB/RustFS consumer
composition, and current Linux/macOS native-platform rows are all evidenced
by terminal hosted jobs. The aggregate workflow was later cancelled by
`main`-branch concurrency and had unrelated FoundationDB/Windows/macOS Rust
failures; those are separate workstreams and are not converted into W08
failures. Local Docker capacity and local native-device/credential gaps remain
environment boundaries, not open W08 acceptance actions.

The headline percentage uses this deliberately simple weighting of the five
top-level tracker items; it is not a line-count metric:

| Item | Provisional completion | Weight | Weighted contribution |
| --- | ---: | ---: | ---: |
| W08.1 implementation | 100% | 15% | 15% |
| W08.2 durable TiDB/PD/TiKV harness and restart | 100% | 25% | 25% |
| W08.3 fencing, concurrency, ambiguous commit and durability assumptions | 100% | 20% | 20% |
| W08.4 Node/CLI/native platform acceptance | 100% | 20% | 20% |
| W08.5 bounded TiDB metadata + RustFS composition | 100% | 20% | 20% |
| **Workstream view** |  | **100%** | **100%** |

The W08.4 number includes W08.4a and W08.4b; those nested rows are shown
separately below and are not additional weight in the summary calculation.

## Production rollout readiness — new scope

The demo is not a production gate. W08 is **100% complete for its defined
functional acceptance scope**, but the production rollout decision is currently
**NO-GO**. The hosted jobs prove a bounded, real TiDB/RustFS topology and
consumer path; they do not prove that a production deployment has an approved
topology, secret-management process, backup/restore capability, upgrade or
rollback path, SLOs, capacity headroom, security sign-off, on-call readiness or
release provenance.

The provisional production-readiness baseline is **15%**. This is a planning
indicator showing that the functional foundation exists; it is deliberately not
a release-readiness measurement and must not be used to approve a rollout. Each
production item below remains open until its required evidence is produced in a
production-like environment. “Implementation” rows are repository work; the
hosted/provider/native rows require external systems or platform evidence.
The executable deployment contract and rollout sequence are also maintained in
[`docs/W08-production-rollout.md`](W08-production-rollout.md).

| ID | Production work item | Gate class | Status and completion | Evidence currently available | Remaining actions / exit evidence | Provisional engineering time | External blockers / dependency |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **P01** | Production deployment contract, topology and configuration | Implementation + hosted/provider | **Open — 20%** | W08 proves a pinned TiDB `v8.5.7` durable 3PD/3TiKV test topology and a pinned RustFS service in hosted Linux CI. Public SDK, CLI and N-API consumers now expose an opt-in TLS client build. These are qualification foundations, not an approved production architecture. | Select managed or self-hosted TiDB/PD/TiKV and object storage; document regions, quorum/HA, network policy, TLS endpoints, resource limits, tenant isolation, supported versions, config ownership and IaC; deploy a staging topology and pass a production-like smoke/restart gate. | **1–2 engineer-days** | Platform/provider owners, target regions, DNS/network/TLS and production-sized capacity are not yet supplied. |
| **P02** | Secrets, IAM, credential rotation and audit | Implementation + provider | **Open — 15%** | W08 keeps live provider credentials environment-injected and does not commit them; hosted rows used real credential-gated services. | Bind production credentials through the approved secret manager; define least-privilege TiDB/RustFS policies, rotation/revocation, bootstrap and break-glass procedures, audit events and redaction checks; prove rotation without data loss. | **0.5–1 engineer-day** | Secret manager, IAM roles/policies and credential-rotation owner are external gates. |
| **P03** | Backup, restore, disaster recovery and data-retention policy | Hosted/provider | **Open — 10%** | Restart/reopen and RustFS fault-recovery markers passed, but no backup, restore, corruption, region-loss or measured RPO/RTO evidence exists. | Define RPO/RTO and retention; configure TiDB and object-store backups/versioning; run an isolated restore drill, point-in-time or snapshot recovery, metadata/block consistency checks, corruption/partial-object handling and documented recovery sign-off. | **2–4 engineer-days** | Backup/restore facilities, retention policy, second environment/region and data-owner approval are required. |
| **P04** | Upgrade, compatibility and rollback rehearsal | Hosted/provider + implementation | **Open — 10%** | The acceptance matrix is pinned to TiDB `v8.5.7` and RustFS `1.0.0`; no production-version upgrade or rollback rehearsal has been run. | Test the supported TiDB/RustFS/client version matrix in staging; rehearse schema/config migration, rolling provider upgrades, client/package rollback, interrupted upgrade recovery and downgrade/forward-fix policy; retain logs and compatibility sign-off. | **1–2 engineer-days** | Target upgrade versions, staging snapshots and provider maintenance windows are external inputs. |
| **P05** | Observability, SLOs, health checks and alerting | Implementation + hosted/provider | **Open — 15%** | Existing code and test markers provide component diagnostics; no production collector, dashboard, alert route, SLO or redaction evidence is recorded here. | Define availability, mount/unmount, metadata latency, block latency, error-budget and recovery SLOs; expose actionable metrics/logs/traces and readiness/health checks; configure dashboards, alerts, paging, retention and secret/PII redaction; exercise an alert end-to-end. | **1–2 engineer-days** | Production collector, paging destination, ownership and alert thresholds are not configured in this workstream. |
| **P06** | Capacity, performance, load and soak qualification | Hosted/provider + implementation | **Open — 10%** | RustFS block benchmark and bounded provider/consumer tests passed, but they are not a representative production workload or capacity claim. | Establish workload mix and dataset/concurrency targets; run baseline, peak, saturation, failover and multi-hour soak tests against production-like capacity; record latency/throughput/error budgets, headroom and scaling limits. | **2–4 engineer-days** | Representative workload, load generators, production-sized service capacity and performance acceptance thresholds are required. |
| **P07** | Security, transport and operational hardening | Implementation + provider | **Open — 15%** | Provider configuration is credential-gated and bounded tests reject unsafe fixture scope; the TiDB provider, SDK, CLI and N-API now compile with an opt-in `rustls` feature, and `scripts/test-tidb-tls.sh` fails closed unless TLS plus CA/hostname verification are explicitly enabled. No production TLS handshake, IAM/network/security review or image/dependency sign-off is recorded. | Enforce TLS and certificate rotation, network segmentation, authn/authz and tenant isolation; run dependency/image/SBOM scanning, threat-model review, audit verification, secret-redaction checks and security sign-off for the selected provider topology; execute the guarded script against the chosen credentialed endpoint and retain its terminal pass. | **2–4 engineer-days** | Security review, approved certificates, firewall/network policy, provider endpoint and hardening controls are external gates. |
| **P08** | Failure injection, runbooks, on-call and incident readiness | Hosted/provider + operations | **Open — 15%** | W08 exercised service restarts, provider fencing, ambiguous commit and RustFS fault recovery; no production incident drill, operator runbook or on-call acknowledgement is recorded. | Exercise client/process loss, metadata outage, object-store outage, stale lease, network partition, partial object write, rolling restart and restore; document detection, diagnosis, mitigation, rollback and data-integrity checks; run an on-call tabletop and timed drill. | **1–2 engineer-days** | Named operators, staging access, incident tooling and an agreed escalation policy are needed. |
| **P09** | Release provenance, canary, go/no-go and rollback | Implementation + hosted | **Open — 10%** | Source commits and terminal hosted job IDs are recorded; no production artifact promotion, signed provenance/SBOM, canary, approval record or rollback result exists. | Produce immutable versioned artifacts and SBOM/signatures; verify package/native artifacts in the target platforms; publish release notes and migration limits; execute staged canary with live SLO/alert observation, rollback rehearsal and an explicit approval record. | **1–2 engineer-days** | Registry/signing, deployment controller, release approvers and a production-like canary target are external dependencies. |

### Production go/no-go rule

The rollout remains **NO-GO** while any P01–P09 item is open, skipped,
credential-blocked, provider-unverified, or supported only by local planning
documents. A later aggregate CI cancellation must not be converted into a
production pass, and W08's Linux live-provider evidence must not be promoted to
macOS/provider or production evidence. The first production milestone is a
staging deployment with real provider credentials and retained evidence for
P01–P05; P06–P09 then gate canary and production approval.

## Work-item ledger

| Work item | Status and completion | Implementation evidence | Provider/hosted/native evidence | Remaining actions | Provisional engineering time | External blockers |
| --- | --- | --- | --- | --- | --- | --- |
| **W08.1** Separate TiDB metadata/block implementation and dependencies | **Landed — 100%** | `integrations/mount-rs-tidb/**` is a separate provider crate. The tracker records the implementation landing as `ca57758`; format checks, four unit tests and acceptance binaries passed. The provider has explicit pessimistic transactions, writer fencing, provider-clock reads, CAS publication and ambiguous-commit mapping. | Local focused TiDB library tests passed (`6 passed`). The real v8.5.7 single-node lane has passed schema, UTF-8/trailing-space, identity and provider checks. | No remaining implementation action in this item. Broader replicated-service evidence belongs to W08.2/W08.3. | **0–1 h** for maintenance/documentation only. | None for the landed scope. |
| **W08.2** Durable real TiDB/PD/TiKV Docker harness and restart results | **Complete — 100%** | The harness is pinned to TiDB `v8.5.7`, owns a durable 3PD/3TiKV topology, validates capacity/readiness/identity, replaces the frontend for a fresh listener lifecycle, restarts TiKV and PD, and reruns the persisted provider contract. Published implementation chunks include `a452220`, `17c8bda`, `c0aa081`, `63dbdbd` and `ece6977`. | CI `35585066458` / `tidb` job `106286459436` passed network, PD, TiKV and TiDB readiness, the frontend replacement, TiKV restart, PD restart and the post-restart provider rerun. Marker: `TIDB_ACCEPTANCE evidence=durable-multinode-restart topology=durable version=v8.5.7 platform=linux/amd64 cpus=4 mem_bytes=16766414848 ambiguous_commit=pass`. | No required W08.2 action remains. Keep the local capacity limitation documented and do not reinterpret this hosted test-cluster result as a host power-loss/fsync guarantee. | **0–1 h** maintenance/documentation only; hosted gate consumed approximately **20 min wall time** in the retained run. | Local Docker remains below the durable memory threshold (`8,232,747,008` available versus `10,737,418,240` required); this is an environment boundary, not an open hosted acceptance blocker. |
| **W08.3** Provider time/fencing, ambiguous commits, concurrency and deployment durability | **Complete — 100%** | The provider maps statement conflicts to `EAGAIN`, stale leases to `ESTALE`, and unknown commit outcomes to a distinct backend error. The published ordering refinement `ecc1106` runs ambiguous-commit injection after durable restart/reopen, and `2e66f94` supplies the strict-Clippy cleanup. | In CI `35585066458`, direct `tidb` job `106286459436` and composite job `106286459715` passed provider identity/schema, provider-clock fencing, concurrent publication, ambiguous-commit no-replay, frontend/TiKV/PD restart and persisted fresh-client checks. | No required W08.3 action remains. Preserve the boundary that liveness/readiness and container restart are service durability evidence, not a universal fsync or power-loss claim. | **0–1 h** maintenance/documentation only; approximately **20 min hosted wall time** retained. | None for the defined W08.3 acceptance. The aggregate workflow had unrelated failures, recorded separately below. |
| **W08.4** Node, CLI, native-mount and macOS/Linux acceptance coverage | **Complete — 100%** | Public Rust SDK/N-API and Rust/Node CLI configuration paths compose TiDB metadata with RustFS/S3-compatible `r2` chunks. Native scripts distinguish credentials, transport, mount, unmount, fresh-provider readback and cleanup. | CI `35585066458` passed `tidb-rustfs` job `106286459715`, ARM Node job `106286459639`, Linux FUSE job `106286459998`, Ubuntu NFS job `106286459483`, and macOS NFS job `106286459246`. | No required W08.4 action remains. Platform rows retain their provider boundaries: live TiDB/RustFS is Linux FUSE; macOS/Ubuntu NFS are native-platform lifecycle gates. | **0–1 h** reconciliation only; approximately **25 min hosted wall time** across retained rows. | Local macOS lacks the required native-device/service prerequisites; hosted platform jobs provide the evidence. |
| **W08.4a** Bounded Node/CLI consumer slice | **Complete bounded scope — 100%** | The provider matrix covers configuration, explicit `durable`, partial write, truncate, shutdown, reopen and owned RustFS-prefix cleanup. Published chunks include `f55f2eb`, `5556872` and `6038772`; hosted `tidb-rustfs` retained all live Node SDK/CLI rows. | CI `35585066458` / job `106286459715` passed the Node SDK and CLI matrices, with live TiDB/RustFS cases executed rather than credential-skipped. Local credential-gated skips remain explicit and are not used as hosted evidence. | No required W08.4a action remains. | **0–1 h** maintenance only. | Local credentials/services are absent, but the hosted gate is complete. |
| **W08.4b** Native-mount and live TiDB/RustFS consumer acceptance | **Complete — 100%** | `71a7a4c` uses current-process mount identity; `6b75cea` is formatting cleanup; `2221144` releases the provider after transport unmount. `ece6977` replaces the TiDB frontend during durable restart, and `ecc1106` isolates ambiguous-commit injection from restart readiness. | CI `35585066458` / `tidb-rustfs` job `106286459715` passed live Linux TiDB/RustFS Node and CLI matrices, independent Rust/Node mounted I/O, clean unmount, fresh provider readback, retained client bytes, TiDB/TiKV/PD restart and RustFS reopen. Standalone Linux FUSE `106286459998`, ARM Node `106286459639`, Ubuntu NFS `106286459483` and macOS NFS `106286459246` also passed. | No required W08.4b action remains. The macOS NFS row is native lifecycle evidence only; it does not claim live TiDB/RustFS ran on macOS. | **0–1 h** reconciliation only; approximately **15 min** for the retained composite/native rows. | Local FUSE/Docker/credential limitations remain non-hosted environment boundaries. |
| **W08.5** TiDB metadata + RustFS S3 chunks | **Complete bounded scope — 100%** | The real v8.5.7 TiDB service and pinned RustFS endpoint passed mixed-provider seed, partial write, truncate, reopen, CAS/fencing and exact owned cleanup; persisted fixtures require explicit volume/prefix/manifest scope and reject transient or symlink paths. | CI `35585066458` / job `106286459715` emitted `TIDB_CHUNKED_RUSTFS_SEED_PASS`, `TIDB_CHUNKED_RUSTFS_REOPEN_PASS`, `RUSTFS_COMBO_PASS`, `RUSTFS_RESTART_REOPEN_PASS` and `RUSTFS_INTEGRATION_PASS`; RustFS restart/fault recovery also passed. | No required W08.5 action remains. | **0–1 h** documentation only. | None for the bounded scope; broad provider claims remain limited to the explicitly exercised durable topology. |

## Evidence ledger

### Local implementation and compile evidence

These checks establish implementation/build state only. They do not substitute
for a real TiDB, RustFS, PD/TiKV restart, or native kernel mount:

| Check | Result | Boundary |
| --- | --- | --- |
| `./scripts/cargo-shared fmt --all -- --check` | PASS | Formatting; the normal shared target was overridden to an allowed `/private/tmp` target because of the local sandbox. |
| `CARGO_TARGET_DIR=/private/tmp/mount-rs-w08-cargo-target ./scripts/cargo-shared check --locked -p mount-rs-napi` | PASS | N-API compilation. |
| `./scripts/cargo-shared check --locked -p mount-rs-tidb --features rustls` | PASS | TLS-capable provider graph compilation only; no endpoint handshake. |
| `./scripts/cargo-shared check --locked -p mount-rs-sdk --features rustls`, `-p mount-rs-cli --features rustls`, `-p mount-rs-napi --features rustls` | PASS | Public TLS feature propagation; no credentials, CA policy or live provider evidence. |
| `sh -n scripts/test-tidb-tls.sh` plus credential-free positive/negative URL-policy checks | PASS | Guardrail logic only; `TIDB_TLS_CONFIG_ONLY_PASS` does not connect or prove a TLS handshake. |
| `./scripts/cargo-shared test --locked -p mount-rs-tidb --features rustls` | PASS, 6 provider unit tests | TLS-enabled provider unit/error-redaction coverage; ignored service tests remained explicitly ignored because no endpoint was available. |
| `./scripts/cargo-shared clippy --locked -p mount-rs-tidb --all-targets --features rustls -- -D warnings` | PASS | Strict-Clippy TLS feature build; not live provider or production evidence. |
| N-API library tests | PASS, 15 tests | Binding/lifecycle unit coverage. |
| TiDB library tests | PASS, 6 tests | Provider unit coverage. |
| Ignored TiDB acceptance binaries `tidb`, `ambiguous_commit`, `chunked_rustfs` | PASS compile-only | Does not claim that live services ran. |
| Shell and Node syntax checks | PASS | Script syntax only. |
| `git diff --check` | PASS | Patch hygiene. |
| Local `node examples/node-cli/index.mjs ...` consumer run | BLOCKED | This checkout currently has no built N-API binding (`Cannot find native binding`); hosted Node jobs are the functional evidence. |

### Hosted and native evidence

| Run/job | Result counted here | Evidence boundary |
| --- | --- | --- |
| CI `35571758453`, `tidb-rustfs` job `106244679000` | Diagnostic partial pass | Real TiDB/RustFS topology, provider contract, seed, Node SDK matrix and CLI matrix passed; native FUSE I/O passed; fresh post-unmount TiDB writer acquisition failed with `EAGAIN`. Retained as the pre-`2221144` failure baseline only. |
| CI `35574581481`, `tidb-rustfs` job `106253499553` | Diagnostic partial pass | Corrected Linux FUSE I/O, clean unmount and fresh provider readback passed; the same job later failed at the then-unfixed durable TiDB restart gate. Retained as an intermediate regression checkpoint. |
| CI `35576142240`, `tidb` job `106258416149` | Diagnostic failure | Direct provider rows and ambiguous commit passed; TiDB frontend restart readiness timed out after 300 seconds. This motivated the supported TOML setting, fresh frontend replacement and later ordering fix. |
| CI `35585066458`, source `9c098e5`, `tidb` job `106286459436` | PASS — W08.2/W08.3 | Terminal success across durable 3PD/3TiKV startup, TiDB frontend replacement, TiKV restart, PD restart, provider-clock/fencing/concurrency, ambiguous-commit no-replay and post-restart persisted provider checks. |
| CI `35585066458`, source `9c098e5`, `tidb-rustfs` job `106286459715` | PASS — W08.4b/W08.5 | Terminal success for real RustFS, TiDB/RustFS seed and reopen, Node SDK/CLI matrices, Linux FUSE mounted Rust/Node I/O, clean unmount, fresh provider readback, retained bytes and RustFS restart/fault recovery. |
| CI `35585066458`, source `9c098e5`, `native-fuse` job `106286459998` | PASS — native Linux | Terminal success for the standalone privileged FUSE/native suite. |
| CI `35585066458`, source `9c098e5`, `node (ubuntu-24.04-arm)` job `106286459639` | PASS — ARM consumer | Terminal success for the ARM Node distribution/consumer gate. |
| CI `35585066458`, source `9c098e5`, `native-nfs (ubuntu-latest)` job `106286459483` | PASS — native Ubuntu | Terminal success for native NFS lifecycle and SQLite split-store coverage. |
| CI `35585066458`, source `9c098e5`, `native-nfs (macos-latest)` job `106286459246` | PASS — native macOS | Terminal success for macOS ARM native NFS lifecycle and SQLite split-store coverage; this does not claim live TiDB/RustFS on macOS. |
| CI `35585066458` aggregate | Cancelled after W08 jobs completed | `main` concurrency superseded the workflow; unrelated FoundationDB, Windows Node and macOS/Windows Rust jobs were also reported separately. The W08 job conclusions above are terminal successes and are the evidence counted here. |
| CI `35588858142`, source `e515036`, `tidb-tls-compile` job `106298487587` | PASS — P01/P07 implementation capability | Hosted compile gate passed for `mount-rs-tidb`, Rust SDK, CLI and N-API with `rustls`. This proves feature propagation, not certificates, secret injection, a live TLS handshake, provider IAM, or production deployment. |
| CI `35589825356`, source `ea062c0`, `tidb-tls-compile` job `106301529923` | PASS — P07 implementation guard | Hosted compile and credential-free positive/negative URL-policy checks passed. The guard requires `require_ssl=true` and rejects disabled CA, hostname, or built-in-root verification; no live endpoint or production identity was used. |
| CI `35590503133`, source `c54c1a4`, `tidb-tls-compile` job `106303655177` | PASS — P07 implementation guard | A later current-main run again passed the TLS-enabled provider/SDK/CLI/N-API compile and credential-free positive/negative URL-policy checks. This remains implementation/guard evidence, not a live provider or production pass. |

The W08 rows above use exact terminal job IDs and markers. The aggregate
workflow conclusion is retained as `Cancelled` because later `main` pushes
superseded it; this does not erase the terminal W08 job results, and it is not
reported as an aggregate CI green result.

## Remaining action plan

1. No required W08 implementation or hosted acceptance action remains for the
   defined functional scope. Preserve the exact terminal job IDs above; do not
   replace them with a later queued/cancelled aggregate status.
2. Resolve P01/P02 first: select the production topology and approved secret
   path, then produce a staging deployment with real provider credentials.
3. Close P03–P05 in that staging environment with restore, upgrade/rollback,
   observability and redaction evidence; retain measured RPO/RTO and SLOs.
4. Close P06–P08 with production-like load/soak, failure drills and an owned
   operator runbook/on-call acknowledgement.
5. Close P09 with immutable release artifacts, signed provenance/SBOM,
   canary telemetry, rollback evidence and an explicit go/no-go approval.
6. If the W08 harness or provider implementation changes, rerun the durable
   and composite hosted rows before reopening W08. Keep native platform rows
   separate from live provider claims.

## External blockers and boundaries

- The local Docker host cannot satisfy the durable 3PD/3TiKV memory requirement
  (`8,232,747,008` available versus `10,737,418,240` required). This is an
  environment capacity blocker, not permission to downgrade the topology.
- The local macOS host has no usable `/dev/fuse`/`fusermount3`, so Linux FUSE
  evidence must come from the hosted native job; macOS NFS evidence must remain
  a separate macOS row.
- Local TiDB/RustFS credentials and services are not present. The live provider
  rows are intentionally hosted and credential-gated rather than simulated.
- Concurrent pushes to `origin/main` repeatedly cancelled earlier CI runs;
  those are retained only as diagnostic history. The retained run
  `35585066458` reached terminal success for every W08-relevant job before its
  aggregate workflow was later superseded.
- Unrelated hosted failures in that aggregate (FoundationDB, Windows Node and
  macOS/Windows Rust) belong to their own workstreams. They may make the
  aggregate workflow cancelled/red, but they do not become W08 failures when
  every W08-relevant job is terminal success.
- Production rollout is additionally blocked by the absence of an approved
  deployment target, secret/IAM policy, backup/restore environment, production
  observability and paging, representative load target, security sign-off,
  named on-call ownership and release/canary approval. These are intentionally
  recorded as external/provider/hosted gates rather than fabricated local
  passes.

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
| 2026-09-21 09:56–10:17 | Reconciled terminal W08 run `35585066458`, added the P01–P09 production rollout ledger and tracker, committed the documentation chunk as `d8f8893`, merged concurrent `origin/main` changes, and published the reconciled tip at `8e42efb`. | ~15 min | ~6 min remote fetch/merge/push wait | W08 functional acceptance is complete; production rollout tracking is a separate no-go scope and is now published. |
| 2026-09-21 10:17–10:26 | Added and locally compiled the opt-in TiDB TLS feature through the provider, Rust SDK, CLI and N-API; added a dedicated CI compile gate and documented the boundary between TLS capability and a credentialed production handshake. Implementation commit `3a70238` was reconciled with concurrent main and published at `7b76556`. | ~10 min | ~4 min compile/remote wait | Public consumers can ship the TLS client graph; P01/P07 remain open pending target topology, certificates, secrets and live provider evidence. |
| 2026-09-21 10:26–10:29 | Followed hosted run `35588858142` and retained `tidb-tls-compile` job `106298487587` as terminal success for source `e515036`. | ~2 min | ~1 min hosted wait | Hosted TLS feature compilation passed; live TLS/provider and production rollout gates remain open. |
| 2026-09-21 10:29–10:37 | Added `scripts/test-tidb-tls.sh`, validated its positive and fail-closed URL-policy paths without credentials, and added the guardrail checks to the hosted TLS compile job. | ~8 min | ~0 min | A reproducible live TLS-provider command now exists; no live endpoint or production IAM/certificate evidence is available here. |
| 2026-09-21 10:37–10:41 | Followed hosted run `35589825356` and retained `tidb-tls-compile` job `106301529923` as terminal success for source `ea062c0`. | ~1 min | ~3 min hosted wait | The fail-closed TLS deployment guard is remotely verified; live credentials, certificates and production provider evidence remain open. |
| 2026-09-21 10:41–10:44 | Ran the TLS-enabled TiDB provider unit suite and strict-Clippy gate locally; the six unit tests passed and the ignored service tests remained explicit. | ~3 min | ~0 min | Repository TLS implementation is locally tested and lint-clean; no live endpoint is available for P07 closure. |
| 2026-09-21 10:44–10:50 | Reconciled the current-main hosted TLS pass and added `docs/W08-production-rollout.md` with the deployment contract, executable evidence matrix, rollout sequence and explicit NO-GO/blocker rules; linked it from `WORK_TRACKER.md`. | ~6 min | ~3 min hosted/remote observation | Production tracking now has both a detailed ledger and an operational rollout contract; external deployment gates remain open. |
| Prior goal phase before this ledger request | TiDB/RustFS harness hardening, native process-identity fix, TiDB/TiKV descriptor and bootstrap fixes, hosted-log analysis and repeated CI queue monitoring. | **Substantial; exact active split not instrumented** | Goal telemetry previously reported roughly 2 h 41 min elapsed, including tool/CI waits | Implementation chunks were committed and pushed; W08 functional acceptance is complete and production gates remain open. |

## Update protocol

For each subsequent W08 chunk, append or revise the relevant row with:

- exact commit and `origin/main` SHA;
- exact command or hosted job ID, including exit/conclusion;
- separate implementation, provider, native and hosted status;
- remaining actions and provisional active/wall-time estimates; and
- any new blocker without converting a skip or cancellation into a pass.

Commit and push each validated ledger or implementation chunk to `origin/main`.
W08 functional acceptance is complete only when the required W08.2, W08.3 and
W08.4b integration/provider/platform evidence is terminal and recorded in both
this ledger and `WORK_TRACKER.md`. Production rollout is complete only when
P01–P09 have terminal evidence, named ownership and an explicit GO decision;
demo approval, local tests, hosted job presence or a cancelled aggregate are
not substitutes.
