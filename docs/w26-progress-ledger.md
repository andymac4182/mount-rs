# W26 progress ledger — Apache Ozone S3 backend

This ledger is the working record for the W26 Apache Ozone S3 backend
workstream. It distinguishes repository implementation, local evidence, and
hosted/native/provider acceptance. Estimates are provisional and are intended
for engineering planning, not a commitment.

## Snapshot

| Field | Current value |
| --- | --- |
| Workstream | W26 — Apache Ozone S3 backend |
| Ledger snapshot | 2026-09-22, Australia/Brisbane |
| Repository | `mount-rs` |
| Snapshot base | `d619f93` (bounded unstorage parity tests are published over concurrent mainline changes; current CI and fault-injection runs are still non-terminal and are not evidence yet) |
| Checklist completion | 4 of 4 W26 tracker rows checked: 100% |
| Provisional execution completion | W26 qualification: 100%; production-rollout readiness: 26% (scope, benchmark matrix, lifecycle safety, bounded remote enumeration, provider-bounded KV/N-API contract and CI acceptance baseline captured; no terminal production gates yet). Customer deployment, native, provider-durability and release-stream gates remain separately bounded |
| Current acceptance state | The retained W26 packet on `9c098e5` remains accepted within its documented provider/platform boundaries. The newer provider-bounded KV/N-API implementation and bounded unstorage parity fixtures are locally green, but current-revision hosted CI and `MOUNTX_SOURCE` provider-parity evidence are not terminal |
| Latest hosted workflow | GitHub Actions run `35585066458` on `9c098e5`; W26 jobs `ozone` `106286459564`, `ozone-compositions` `106286459622`, and `ozone-tidb` `106286459540` completed successfully, with generic `tidb` job `106286459436` also green. Unrelated provider/native jobs are tracked separately and are not required to close W26. |
| Current-head CI attempt | Published tip `d619f93` has GitHub Actions CI run `35609794020` pending and Fault injection run `35609793694` in progress; Live Cloudflare R2 run `35609794005` failed but is unrelated to W26. No queued, in-progress, canceled or failed run is promoted to evidence; the retained W26 packet above remains the last accepted hosted result. |
| Local Docker boundary | Docker Desktop capacity was about 5 CPUs and 8.2 GiB; this is sufficient for the durable FoundationDB proof but below the TiDB harness's 10 GiB durable-topology minimum |
| Production rollout track | Open, currently **NO-GO**; 0 of 15 production gates are terminally accepted. The 26% figure reflects scope decisions, local hardening and an acceptance baseline, not deployable readiness |
| W26 production target | Customer-deployed Ozone integration; W26 owns provider/client correctness and CI qualification, not customer deployment, backup/DR or release promotion |
| Required service envelope | Target 1,000 IOPS per drive; Tier 1 99.99% reliability; 5-minute RPO and 5-minute RTO. RPO/RTO and availability remain dependent on the customer's Ozone topology and operations |
| Available qualification environment | CI only; no staging environment is available. Production-like evidence must therefore be achieved through controlled hosted CI/provider fixtures and clearly labeled customer-owned prerequisites |
| Release/acceptance decision | W26 qualification accepted within the documented local/hosted provider scope on terminal run `35585066458`; production rollout remains **NO-GO** and no broader native release claim is made |

## Scope decisions recorded from product direction

These decisions were supplied on 2026-09-21 and supersede the earlier
assumption that W26 might own a deployable staging or customer production
environment. They define what W26 must make ready for other streams and
customers to deploy.

| Decision | Recorded answer | W26 consequence and evidence boundary |
| --- | --- | --- |
| Deployment ownership | Customers deploy Ozone; W26 is not the deployment operator | W26 must provide a production-grade Ozone-compatible integration and CI qualification packet. Customer topology, capacity placement, backup/DR and on-call execution are external gates. |
| Metadata providers | Support all available metadata providers where feasible | Qualify SQLite, PGlite, TiDB and FoundationDB against Ozone where the provider can run in CI; publish provider-specific limitations rather than treating one provider's result as universal. |
| Performance target | Each drive must sustain 1,000 IOPS | Add a repeatable CI workload and report operations, latency percentiles, concurrency, errors, resource envelope and provider/topology. CI performance is qualification evidence, not a customer capacity guarantee. |
| Reliability target | Tier 1 service, 99.99% reliability | W26 must test client retry, fencing, idempotency, restart/failover and error observability; 99.99% service availability is ultimately a customer Ozone deployment/SLO responsibility. |
| Recovery objectives | 5-minute RPO and 5-minute RTO | W26 must preserve acknowledged-commit and reopen/recovery semantics; Ozone backup/replication/restore mechanisms and measured RPO/RTO are customer/provider-owned. |
| Security | Proper production security requirements are required | Add secure endpoint/authentication/TLS/secret-reference, least-privilege, redaction, negative-path and audit-boundary checks that can run in CI; do not place credentials in the repository or ledger. |
| End-to-end surface | Everything must work end to end | Qualify Rust, Node, CLI, HTTP and advertised native/mount surfaces through the Ozone-backed path; native platform implementation and runner availability remain cross-workstream gates. |
| Backup/DR | Ozone owns backup and DR | Do not implement a competing W26 backup system. Record Ozone/customer backup, restore and failure-domain requirements as an external acceptance dependency and test W26 recovery behavior around them. |
| Release process | Another stream owns releases | W26 supplies reproducible CI evidence, compatibility notes and release inputs; promotion, signing, canary and rollback execution remain external. |
| Test environment | CI only; no staging | Do not claim staging or production acceptance. Build the strongest bounded hosted CI matrix possible and label customer-environment evidence as pending until supplied by the deployment stream. |

## Work-item ledger

| Work item | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / gates |
| --- | --- | ---: | --- | --- | --- | --- |
| W26.1 — Ozone 2.2.1 gateway harness, digest pinning, health/bucket lifecycle | Implementation and local/hosted gateway gates complete | 100% | `scripts/test-ozone.sh` ran against the official pinned Ozone 2.2.1 image on arm64. Health, bucket creation, immutable and duplicate-object behavior, stale ETag, CAS, concurrent writers, range/full/missing reads, binary fixtures, fault window, bounded stopped-gateway behavior, restart/reopen, integration, and cleanup passed. Final hosted run `35585066458`, job `106286459564`, emitted `OZONE_HEALTHY`, `OZONE_READY`, `OZONE_INTEGRATION_PASS`, and `OZONE_CLEANUP_PASS` on Linux-amd64. | Keep the nonsecure loopback limitation visible; no further W26 gateway action remains. | 0 h acceptance; 0–1 h review | Hosted Linux runner and image architecture are external. Local and hosted harnesses are loopback/nonsecure and not production replicated durability. |
| W26.2 — immutable-object and fault/restart behavior | Implementation and local/hosted gateway gates complete | 100% | The arm64 Ozone run passed immutable/duplicate-object, stale-ETag, CAS, fault-window, stopped-gateway, restart/reopen, and cleanup checks. The failure timeout was widened to 15 seconds after a measured 5.6-second loaded Docker failure window (`62bc099`). Final hosted Ozone job `106286459564` passed on `35585066458`. | Keep the provider/platform and production-durability boundaries explicit; no further W26 action remains. | 0 h acceptance; 0–1 h review | Hosted CI is the provider/platform gate. No claim is made for production TLS, authentication, power loss, or a production replicated Ozone deployment. |
| W26.3a — SQLite and disk-backed PGlite composition over real Ozone | Local and hosted provider composition complete | 100% | `MOUNT_RS_OZONE_COMPOSITIONS=1 ./scripts/test-ozone-compositions.sh` passed both SQLite and disk-backed PGlite against live Ozone, including partial/truncate/reopen semantics, with `revision=17`. Final hosted `ozone-compositions` job `106286459622` passed on `35585066458`, including `SUMMARY node-sdk pass=7 skip=1 fail=0`, Ozone integration, and cleanup markers. | Keep the intentional Node-matrix skip and the native/provider boundaries visible. | 0 h acceptance; 0–1 h review | Hosted Linux is required for portable CI evidence. PGlite is not a claim about every native Node packaging/runtime. |
| W26.3b — independent single-node TiDB composition over Ozone | Local smoke and contract complete; not replicated acceptance | 100% of this sub-item | Single-node TiDB/Ozone run passed TiDB identity/provider/fencing, ChunkedFs partial/truncate/CAS/stale-fencing/reopen, ambiguous commit, and cleanup. Evidence was explicitly labeled `single-node-smoke-not-replicated-acceptance`. | Preserve this as a lower-level contract signal only; it does not replace the durable hosted topology gate. | 0 h for current scope | Single-node TiDB is intentionally not a durability or failover claim. Local machine capacity is below the durable TiDB harness minimum. |
| W26.3c — independent durable three-node FoundationDB composition over Ozone | Implementation and local proof complete; hosted Ozone lane pending | 90% | The arm64 durable run used three fixed-address FoundationDB 7.4.7 containers, three coordinators, persistent per-server volumes, `double` redundancy and SSD storage. Transaction probes passed before and after node-2 restart; first-client and fresh-client Ozone compositions passed; `FOUNDATIONDB_TEST_PASS topology=durable`; owned resources were removed and verified absent. CI now has a dedicated `ozone-foundationdb` job that runs the same durable mode against the live Ozone gateway. | Review a terminal `ozone-foundationdb` job on a retained revision and record the exact FoundationDB/Ozone versions, restart marker, cleanup and client image evidence. | 0.5–1 h hosted review | Hosted CI capacity, nested FoundationDB client image startup, and provider/native runtime are external. Local and hosted proofs remain loopback/nonsecure and do not cover production auth/TLS, power-loss behavior, or customer replication. |
| W26.3d — durable multi-node TiDB composition over Ozone | Harness and hosted acceptance complete | 100% | Final hosted run `35585066458`, Ozone-specific job `106286459540`, used `MOUNT_RS_OZONE_TIDB_COMPOSITION=1`, durable TiDB v8.5.7, and the real Ozone gateway. It emitted `TIDB_CHUNKED_RUSTFS_SEED_PASS`, `TIDB_CHUNKED_RUSTFS_REOPEN_PASS`, `TIDB_ACCEPTANCE evidence=durable-multinode-restart topology=durable version=v8.5.7 platform=linux/amd64 cpus=4 mem_bytes=16765378560 ambiguous_commit=pass`, `OZONE_INTEGRATION_PASS`, and `OZONE_CLEANUP_PASS`. The generic durable TiDB job `106286459436` also emitted the same acceptance marker. | None for the requested W26 scope; retain the provider and production-boundary notes. | 0 h acceptance; 0–0.5 h evidence review | GitHub-hosted Docker/TiDB/PD/TiKV and Ozone are hosted provider gates. This does not claim production TLS/authentication, power-loss durability, or native mounting. |
| W26.4a — Node provider matrix and Rust/Node CLI coverage | Implementation and local/hosted composition gate complete | 100% | The live arm64 composition run passed Node SDK coverage including PGlite-to-S3 partial/truncate/reopen (`pass=7 skip=1 fail=0`), Node CLI coverage, and the matching ignored Rust CLI live Ozone split-provider/reopen test. Final hosted `ozone-compositions` job `106286459622` passed on `35585066458` with `SUMMARY node-sdk pass=7 skip=1 fail=0`. | Keep the Node matrix's one intentional skip and native/provider boundaries visible. | 0 h acceptance; 0–1 h review | Hosted NAPI/PGlite build and Linux Node runtime are provider/native gates. Local arm64 evidence is not Linux-amd64 evidence. |
| W26.4b — CI wiring, pinned images, cleanup, and documentation | HTTP Ozone gate implemented; hosted result pending | 100% implementation; 90% hosted packet | CI installs NAPI/PGlite with locked Rust dependencies, runs the Ozone composition matrix, Node/CLI coverage, the shipped HTTP server/client reopen path, and durable Ozone/TiDB job. The HTTP runner restricts its prefix below the unique Ozone run scope and verifies deletion before returning. FoundationDB harness uses platform-specific pinned official 7.4.7 image manifests and transaction readiness probes; the dedicated `ozone-foundationdb` job is wired for the Ozone-backed durable mode. | Run and review `ozone-compositions` and `ozone-foundationdb` on a retained revision; record the HTTP pass and cleanup markers; keep W26 production readiness NO-GO until all open gates are terminal. | 0.75–1.25 h hosted review | GitHub workflow concurrency, nested provider startup, NAPI/PGlite builds and other workstream pushes can cancel runs; canceled/queued jobs are not evidence. |
| W26.5 — explicit immutable-block reconciliation and open-unlink safety | Implementation and local contract tests complete; provider/platform acceptance open | 100% implementation; 50% production qualification | `BlockStore::reconcile` now fails closed by default; `ChunkedFs::reconcile_blocks` rejects zero grace before taking the lease, renews the writer lease, roots the committed namespace and open-unlinked handles, and delegates scoped cleanup. R2/Ozone blocks stream only their validated prefix, retain live/recent objects, delete only aged unreferenced objects and return bounded counts without materializing the entire listing. Rust SDK, observability, fault-injection and N-API wrappers forward the capability; locked R2/chunked/wrapper tests, strict Clippy and the rebuilt N-API chunked test passed. | Add provider-native enumeration/reconciliation where supported or retain explicit `ENOTSUP`; exercise ambiguous publication, object loss, quotas/space pressure, metrics/alerts and the customer/Ozone maintenance owner in hosted CI; review a fresh security scan on the final published revision. | ~2.5–4.5 h implementation and local verification; ~0.5–2 d hosted/provider/security review | Ozone/customer retention policy, provider listing/deletion semantics, hosted credentials/topologies, alert collector and security-review execution are external gates. |
| W26.6 — bounded remote directory enumeration and response materialization | Built-in and KV/N-API provider-boundary implementation complete; provider/native qualification open | 100% implementation; 65% production qualification | `FsDriver::readdir_bounded` fails closed with `ENOTSUP` by default. HTTP `/entries` and directory-file routes request the bound before serialization and map overflow to the existing 413/connection-close contract. Memory, host, chunked, versioned, persisted, observability, CLI and native N-API wrappers implement or forward the boundary. `KeyValueStore::get_keys_bounded` is an optional provider-side contract; the unstorage bridge exposes `getKeysBounded(prefix, maxKeys)` and `Filesystem.readdirBounded(path, maxEntries)`, while providers without the callback remain fail-closed. Terminal local evidence: KV 13 integration tests, N-API Rust 16 tests, HTTP 8 unit + 12 integration, core 11, host 5, chunked 14, CLI 44, observability 4 and persistence 4; release N-API packaging, unstorage bridge, typecheck, chunked smoke, strict affected-package Clippy, formatting and diff checks passed. The follow-up parity fixtures also pass the capable-provider overflow/success path and the absent/legacy-callback `ENOTSUP` path. `MOUNTX_SOURCE` parity suites were explicitly skipped because the provider source is unset. | Exercise the new contract in hosted Ozone/provider jobs, add provider-native pagination where a backend can safely enforce it, retain `ENOTSUP` for legacy providers, run `MOUNTX_SOURCE` parity, and review the fresh security scan on the final revision. | ~2.75–4.5 h implementation/local verification; ~1–3 d provider/security/hosted review | Provider-side key enumeration, JavaScript callback implementation, hosted provider topologies, secure Ozone fixture, mountx source and security-review execution are external or cross-workstream gates. |

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
| W26 integration readiness | **NO-GO** | W26 qualification is green on hosted run `35585066458`, but all-feasible-provider CI, secure integration checks, 1,000-IOPS results and complete end-to-end/security coverage are not yet terminal | All W26-owned P0–P5, P7–P8, P10–P11 and P14 evidence is terminal on one retained revision; customer Ozone deployment, DR and release remain explicit external dependencies |
| Customer production target | Customer-deployed Ozone; topology not supplied | Product direction fixes the service envelope at 1,000 IOPS per drive, 99.99% reliability and five-minute RPO/RTO, but W26 does not operate the customer topology | W26 documents the Ozone/provider/client contract; customers and deployment streams provide secure topology, backup/DR, monitoring and measured availability/recovery evidence |
| Qualification baseline | Complete for W26 only | Revision `9c098e5` and hosted run `35585066458` are the retained W26 acceptance packet | Any production implementation change gets a fresh qualification and production evidence packet on the tested revision |

### Production gate ledger

| Gate / work item | Work type | Status | Completion | Evidence now available | Remaining actions / exit evidence | Provisional engineering time | External blockers / hosted or native gates |
| --- | --- | --- | ---: | --- | --- | ---: | --- |
| P0 — production scope, support matrix, SLO/RPO/RTO and ownership | Implementation / operations | Scope captured; CI acceptance baseline open | 60% | Product direction now records customer-deployed Ozone, all feasible metadata providers, 1,000 IOPS per drive, 99.99% reliability and 5-minute RPO/RTO; no customer topology is supplied | Turn these targets into provider-specific CI assertions, define the advertised client/platform matrix, document customer/Ozone-owned prerequisites and obtain owner sign-off on the support matrix | 0.5–1.5 d | Product/support decisions are mostly supplied; provider support limits and customer deployment owners remain external |
| P1 — customer Ozone topology and deployment rehearsal | External dependency — not a W26 deployment task | Customer-owned / not measured by W26 | 0% W26 deployment evidence | Current Ozone evidence is a pinned all-in-one, non-secure, loopback CI fixture with anonymous volumes and no production replication claim | Customer/deployment stream must provision and operate secure multi-node Ozone; W26 consumes CI-accessible endpoints or fixtures and documents the required topology contract | 0–1 d W26 contract review | Customer infrastructure, persistent storage, network policy, image architecture, certificates and environment access |
| P2 — production metadata-provider support matrix | Hosted/provider CI | Ozone provider matrix expanded; KV/N-API contract added; terminal all-provider evidence pending | 45% | The benchmark has explicit SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 rows. SQLite is eligible from the Ozone composition job; PGlite, TiDB and FoundationDB are configuration/feature-gated and appear as explicit skips when their CI topology is absent. The new key-value contract can enforce a bound through `get_keys_bounded`, and unstorage can opt in through `getKeysBounded`; local parity fixtures now cover both capable and legacy/absent callback behavior, but no hosted provider-parity result has yet exercised it. | Retain terminal Ozone results for each configured provider on one revision, record provider versions/HA/failure semantics and unsupported combinations, add capable KV/provider rows, and do not promote an explicitly skipped provider | 3–8 d | CI capacity, provider images/versions, provider-side listing APIs, managed-service access if required and provider-specific operator limits |
| P3 — authentication, TLS, secret lifecycle and redaction | Implementation / hosted/provider CI | Local transport boundary hardened; secure integration gate open | 50% | R2/Ozone config parsing and runtime validation reject non-HTTP(S), embedded credentials, query/fragment, missing-authority and remote plaintext-HTTP endpoints before client construction; HTTP config and runtime now reject non-loopback binds, require loopback behind a TLS reverse proxy, keep credentials as environment references and redact diagnostics | Add authenticated HTTPS endpoint CI where available, certificate identity/rotation checks, secret injection/rotation references, least privilege and clean-client negative tests; retain no secret values | 3–7 d | Secure Ozone CI endpoint or customer-supplied fixture, certificates/identity, secret manager integration and security review |
| P4 — replicated block durability and storage failure protection | Provider/customer deployment dependency plus CI contract | Lifecycle protection implemented; durability qualification open | 40% | Restart/reopen and durable provider checks pass in bounded CI topologies. The explicit reconciliation path rejects zero grace, protects committed and open-unlinked roots, retains a configurable grace window, streams the configured R2/Ozone prefix and scopes deletion to validated block IDs; no customer storage, power-loss or Ozone replication guarantee exists | Test client behavior for object loss, unavailable gateway, retries, integrity mismatch and recovery in CI; document Ozone replication/fsync/storage requirements, retention ownership, deletion authorization and space-pressure alerts that customers must satisfy | 1–3.5 d W26 CI work; deployment work external | Ozone storage and replication semantics, failure controls, retention policy and customer topology; CI restart is not power-loss evidence |
| P5 — fencing, ambiguous commit and stale-writer recovery under failover | Implementation / hosted/provider CI | Lease-protected reconciliation implemented; failover matrix open | 35% | W26 exercises CAS, stale fencing, ambiguous commit and durable restart in bounded provider compositions; hosted TiDB marker reports `ambiguous_commit=pass`; reconciliation renews the writer lease before deriving roots and never runs implicitly on shutdown | Extend all feasible Ozone/provider CI lanes with concurrent clients, retry, gateway/provider loss, delayed responses and post-ambiguity reconciliation; prove no stale publication, duplicate block or lost acknowledged commit | 3–7 d | Distributed CI fault controls, provider failover behavior and multiple-client scheduling |
| P6 — backup, restore, disaster recovery and retention | External dependency — Ozone/customer owned | Not a W26 implementation task | 0% W26 DR evidence | Product direction assigns backup and DR to Ozone/customer deployment; W26 has no competing backup system | Document the Ozone/customer requirements needed to meet 5-minute RPO/RTO and test W26 reopen/error behavior around supplied recovery scenarios when CI fixtures expose them | 0.5–1.5 d W26 contract documentation | Ozone backup/replication/restore design, failure domains, KMS and customer operations |
| P7 — observability, alerts, dashboards and runbooks | Implementation / CI contract / cross-workstream | Local HTTP/OTLP and provider-boundary evidence passed; deployment integration open | 45% | `mount-rs-http` passed 8 unit and 12 integration tests; the OTLP-enabled HTTP suite passed 11 integration tests including bounded error telemetry; full-feature observability passed 5 unit tests plus local collector and exporter-failure tests; CLI observability passed 44 unit, 9 CLI, 2 HTTP subprocess and 1 native-artifact test. `reconcileBlocks` returns scanned/protected/recent/deleted counts and fails closed when unsupported, while `readdir_bounded` is forwarded through observability/CLI wrappers. The N-API unstorage bridge also preserves the provider-boundary `EOVERFLOW` contract. These are local collector/fixture results, not deployed alerting. | Add/retain machine-readable Ozone/provider error categories, reconciliation and bounded-listing metrics, and health evidence in the hosted packet; coordinate dashboards, alerts and runbooks with W30/customer operations | 2–5 d W26 contract/tests | Collector reachability, alerting and paging are external; W30 and customer operations own deployed dashboards/paging |
| P8 — load, capacity, soak and cost envelope | Hosted/provider CI | Per-provider CI gate implemented; hosted result pending | 25% | The benchmark records successful write+read+delete lifecycle IOPS, supports a hard `--min-iops` threshold and redacted JSON. `scripts/test-ozone.sh` now requests SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 rows; each configured row must meet the 1,000 target, while absent provider prerequisites remain visible as skips. `.github/workflows/ci.yml` requests 4 KiB payloads, 400 iterations and concurrency 64. Benchmark unit tests, no-credential skip output, shell syntax, and rebuilt N-API chunked lifecycle tests pass; no live local IOPS result is claimed. | Run the terminal hosted `ozone-compositions` job, retain `artifacts/ozone-iops.json`, review latency/errors/resources and repeat with TiDB/FoundationDB topologies when their provider-specific CI environments are present; add soak/capacity variants before treating the target as qualified | 3–8 d after harness implementation | Stable hosted CI runners, Ozone fixture startup, provider quotas/topologies and enough runtime for meaningful soak; CI does not prove customer capacity or 99.99%/RPO/RTO |
| P9 — upgrade, rollback and compatibility | External release/deployment dependency | Not a W26 release task | 0% W26 migration evidence | W26 pins Ozone 2.2.1 and provider fixture versions for qualification only; release execution belongs to another stream | Supply compatibility notes, config/schema/object invariants and requalification commands for the release stream; do not own promotion or rollback automation here | 1–3 d W26 compatibility notes | Release stream, maintained provider versions, change window and customer deployment approval |
| P10 — security, privacy, tenancy and audit review | Implementation / hosted CI / security review | Threat-model checkpoint and local controls recorded; lifecycle, pre-materialization listing and provider-boundary hardening added; final review open | 55% | The delegated architecture worker mapped provider composition, credentials, object prefixes, cleanup, Rust/Node/CLI/N-API/HTTP/native paths and customer/Ozone ownership. Local R2 endpoint/TLS policy, HTTP loopback-only binding, bearer isolation/constant-time comparison, redaction, connection/request bounds, pre-materialization directory entry limits, serialized-response limits, telemetry redaction and scoped cleanup checks pass; strict affected-workspace check is green; scoped reconciliation fails closed for unsupported providers, renews the writer lease, protects live/open-unlinked roots, validates block IDs and deletes only aged objects under the configured prefix. `FsDriver::readdir_bounded` now fails closed for unsupported providers and is implemented/forwarded for built-in Rust paths; KV can opt into `get_keys_bounded`, and N-API maps the bounded unstorage path back to Node-style `EOVERFLOW`, while absent/legacy provider callbacks remain fail-closed. The capable/legacy provider parity fixtures pass locally, but they do not substitute for a security review of arbitrary callbacks. Standard scan `943a7c01-e25f-48ee-952f-040b7421e80d` is still running against older revision `f54dff8`; current-revision scan `82b47cc3-d6c8-45ed-ac21-35c304c2e4b8` is still at its threat-model checkpoint against `3d15855`; current published CI run `35609794020` is pending. No final no-findings or vulnerability result is claimed. | Complete discovery/validation on the current revision or a refreshed diff, review dependency/image provenance, authorization/isolation, encryption expectations, audit fields and abuse limits; exercise provider-native pagination in hosted CI or retain documented fail-closed contracts with owner sign-off; close findings or record exceptions; retain secure Ozone auth/rotation evidence where available | 4–10 d | Security reviewer, scan worker completion, customer identity/tenancy model, secure Ozone endpoint/certificates, compliance requirements and scanning infrastructure |
| P11 — end-to-end client, mount and platform qualification | Native/provider CI / cross-workstream | HTTP path and built-in/KV bounded-listing contracts added to Ozone CI; full matrix required | 35% | W26 covers Rust/Node/CLI and the shipped HTTP server/client path through the Ozone composition gate. The HTTP row verifies binary/full/range I/O, cross-drive auth rejection, graceful shutdown/reopen and scoped object cleanup; built-in Rust providers enforce the directory bound before response materialization; the unstorage/N-API path now has a provider callback, public `readdirBounded` API, Node error-shape test and TypeScript declaration; the follow-up unstorage fixtures cover capable overflow/success and legacy fail-closed behavior. Native mount, providers without the callback, `MOUNTX_SOURCE` parity and every platform are not yet accepted | Review terminal `ozone-compositions` evidence for the HTTP marker and cleanup, add/retain bounded provider rows for key-value/JavaScript-owned metadata, run mountx parity and then exercise every advertised native/mount surface; retain platform/provider matrices, restart/recovery and negative capability evidence | 5–15 d depending on advertised platforms | macOS/Linux/Windows runners, privileged mount facilities, native workstreams, provider callback contracts, mountx source, signing and provider connectivity |
| P12 — release packaging, CI promotion, canary and rollback automation | External release stream | Not a W26 release task | 0% W26 release evidence | Product direction assigns releases to another stream; W26 hosted jobs provide qualification inputs only | Publish reproducible CI commands, version/image pins, evidence markers and compatibility notes for the release stream; no W26 canary claim | 0.5–2 d W26 handoff | CI/CD, artifact registry, signing keys, deployment platform and release owner |
| P13 — incident, failover and recovery rehearsal | External customer/Ozone operations plus CI fault contract | Not a W26 operator task | 0% W26 rehearsal evidence | W26 restart and cleanup tests are bounded qualification checks, not customer incident exercises | Add CI fault/recovery cases where controllable and document the operator scenarios customers must rehearse to meet 99.99% and 5-minute RTO | 1–3 d W26 fault contract | Customer on-call, Ozone operations, paging/incident tooling and maintenance windows |
| P14 — final W26 integration-readiness review | W26 implementation / hosted CI / handoff | Not started | 0% | Current decision remains NO-GO because the all-provider, performance, security and end-to-end CI packet is incomplete | Audit the CI matrix on one retained revision, attach provider/platform/security/performance evidence, list customer/Ozone dependencies and hand off an explicit integration-ready or NO-GO decision to the release/deployment streams | 1–2 d | All W26-owned CI gates plus external customer Ozone and release-stream confirmations |

### Production rollout phases and provisional effort

The following plan is provisional engineering time, not a delivery promise. It
excludes provider provisioning, CI queues, approvals, maintenance windows and
other elapsed wall-clock gates.

| Phase | Gates | Provisional engineering effort | Exit |
| --- | --- | ---: | --- |
| Scope and support matrix | P0 | 0.5–1.5 d | Provider-specific CI assertions, client/platform matrix, 1,000-IOPS definition, 99.99% boundary and five-minute recovery-objective contract |
| Provider, security and failure CI | P2–P5, P7, P10 | 17–42 d | All feasible Ozone/provider lanes, secure endpoint checks, redaction, failure semantics, telemetry contract and security evidence |
| Performance and end-to-end CI | P8, P11, P14 | 10–27 d | Per-drive 1,000-IOPS workload plus Rust/Node/CLI/HTTP/native advertised surfaces and one-revision evidence audit |
| Customer/Ozone and release handoffs | P1, P6, P9, P12, P13 | 3–10 d W26 contract work | W26 supplies requirements and CI evidence; customer deployment, Ozone DR/backup, incident operations and release execution remain external |
| **Total provisional W26 engineering/contract range** | **P0–P14** | **31–82 d, plus external waits** | **Planning range only; customer deployment, Ozone DR and release execution are not W26 estimates** |

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
- Because no staging environment exists, W26 may claim only controlled CI and
  provider-fixture evidence. CI can qualify integration behavior and measured
  performance; it cannot by itself prove a customer's 99.99% availability,
  five-minute RPO/RTO or Ozone deployment topology.
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
- The 1,000-IOPS target is a W26 CI qualification target, not a universal
  customer capacity guarantee. The 99.99% availability and five-minute RPO/RTO
  objectives depend on the customer's Ozone replication, backup, storage,
  monitoring and recovery design.
- The Ozone IOPS artifact now has one row per configured metadata provider over
  Ozone blocks. SQLite/R2 is the baseline row; PGlite/R2, TiDB/R2 and
  FoundationDB/R2 require their own endpoint/feature/topology prerequisites.
  A skipped row is an unavailable gate, never a provider pass or a substitute
  for another metadata backend.
- The current IOPS measurement is explicitly a three-operation filesystem
  lifecycle (`write + full read/verify + delete`) over the public Node
  split-provider path. It is a repeatable integration baseline, not a claim
  that one lifecycle equals every customer's physical-drive I/O profile.
- Block reconciliation is explicit and provider-scoped. R2/Ozone is the only
  current implementation that can enumerate and delete its immutable objects;
  SQLite, PGlite, TiDB and FoundationDB block providers retain the default
  `ENOTSUP` contract until they provide an equivalent safe enumerator. The
  grace period, writer lease, committed namespace roots and open-unlinked
  handles are safety inputs; shutdown success never implies cleanup.
- Bounded key enumeration is an optional provider promise, not a post-hoc slice.
  `KeyValueStore::get_keys_bounded` and unstorage's `getKeysBounded` must bound
  the backend operation before materializing its result; the legacy `getKeys`
  callback is intentionally not reused for remote bounded listings. Providers
  without the callback return `ENOTSUP`, and the N-API postlude preserves
  provider overflow as Node-style `EOVERFLOW`. The in-memory callback used by
  the local bridge test is a test oracle, not production provider evidence.

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
- [x] Implement the lifecycle-safety slice: explicit scoped reconciliation,
  positive grace validation at every coordinator/API boundary, writer-lease
  fencing, committed/open-unlinked root protection, streamed R2
  prefix/block-ID validation, wrapper forwarding and
  N-API `ENOTSUP`/range-error behavior. Local locked tests and strict Clippy
  pass; hosted provider/retention/alert evidence remains open.
- [x] Add a fail-closed bounded directory-enumeration contract before HTTP
  materialization. Memory, host, chunked, versioned and persisted Rust paths,
  observability, CLI and native N-API wrappers implement or forward the bound;
  HTTP `/entries` and directory-file routes preserve the existing 413/close
  contract. Providers without a provider-side bound remain explicit
  `ENOTSUP`/pagination gates.
- [x] Add the provider-bounded key enumeration follow-up for key-value and
  JavaScript-owned consumers. `KeyValueStore::get_keys_bounded`, unstorage's
  `getKeysBounded`, N-API `Filesystem.readdirBounded`, generated TypeScript
  declarations and Node error-shape wrapping are implemented and locally
  tested; absent callbacks still fail closed. Hosted provider parity and
  `MOUNTX_SOURCE` suites remain open gates.

### Production rollout checklist (open)

- [x] Record customer deployment ownership, all-feasible-provider intent,
  1,000 IOPS per drive, 99.99% reliability, five-minute RPO/RTO, end-to-end
  scope, external DR/release ownership and CI-only qualification.
- [ ] P0: convert those decisions into the supported provider/platform matrix,
  provider-specific assertions, owners and approved non-goals.
- [ ] P1: document the secure Ozone topology and customer deployment contract;
  do not claim W26 staging or deployment ownership.
- [ ] P2–P5: close all feasible Ozone/provider CI lanes, secure endpoint tests,
  durability/error contracts and concurrent failover/fencing evidence.
- [ ] P6: document the Ozone/customer backup and DR prerequisites for five-minute
  RPO/RTO; do not build a competing W26 backup system.
- [ ] P7/P10: close integration telemetry, redaction, threat-model, security
  CI and audit-boundary evidence.
- [ ] P7/P10 follow-up: implement provider-native bounded/paginated listing for
  key-value and JavaScript-owned providers, or retain their explicit fail-closed
  production contract with owner sign-off.
- [ ] P8: run the per-drive 1,000-IOPS CI workload with latency, errors,
  resource and provider-specific results. The workload now names every
  supported Ozone-backed metadata provider and records absent provider
  prerequisites as explicit skips; terminal hosted artifacts are still open.
- [ ] P9/P12/P13: hand compatibility, CI evidence, customer incident scenarios
  and release inputs to the owning streams.
- [ ] P11: run every advertised Rust/Node/CLI/HTTP/native surface end to end
  through Ozone, retaining cross-workstream native blockers.
- [ ] P14: audit one retained CI revision and record W26 integration-ready or
  NO-GO before another stream promotes a release.

## Provisional remaining effort and blockers

| Category | Estimate | Notes |
| --- | --- | --- |
| Hosted result inspection | 0 h acceptance; 15–30 min record maintenance | Final W26-specific jobs are green on `35585066458`; any future review is maintenance only. |
| Durable TiDB/Ozone remediation | 0 h for W26 acceptance | The restart isolation fix is validated by the Ozone-backed durable acceptance marker. Separate production/native/provider expansion would be a new scope. |
| Tracker/ledger publication | 15–30 min for this closeout chunk | Includes fast-forwarding concurrent `origin/main` changes, `git diff --check`, commit, push, and remote verification. |
| External CI waiting | Unbounded wall-clock; not engineering time | GitHub runner queue, workflow concurrency, and concurrent pushes have repeatedly canceled otherwise useful runs. |
| Native/provider acceptance | Separate gate | Linux hosted NAPI/PGlite, TiDB/PD/TiKV, FoundationDB, and Ozone container behavior cannot be fully inferred from the local arm64 run. |
| Scope and provider-matrix definition (P0–P2) | 4–11 d engineering | Product targets are recorded; provider-specific support, platform scope and CI assertions remain to be defined and qualified. |
| Security, failure and integration CI (P3–P7, P10) | 14–36 d engineering | Requires secure CI fixtures where available, fault controls, redacted telemetry and security review; customer deployment remains external. |
| Performance and end-to-end matrix (P8, P11, P14) | 10–27 d engineering | Requires stable hosted CI runners, 1,000-IOPS workload design, native runners and one-revision audit. |
| Customer/Ozone and release handoff (P1, P6, P9, P12, P13) | 3–10 d W26 contract work | Ozone backup/DR, customer operations, deployment and release execution are external and not estimated as W26 implementation. |
| External CI waiting | Unbounded wall-clock; not engineering time | CI queues, provider image startup, fixture credentials and runner/platform availability remain elapsed gates. |
| **W26-owned production-readiness total** | **31–82 d engineering/contract work plus external waits** | Provisional planning range; no customer deployment or release commitment is implied. |

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
| 2026-09-21 — Product production-scope decisions | Recorded customer-deployed Ozone ownership, all-feasible-provider intent, 1,000 IOPS per drive, 99.99% reliability, five-minute RPO/RTO, end-to-end/security scope, customer/Ozone DR ownership, separate release stream and CI-only testing. | ~0.5 h | 0 h | Reframed P0–P14 around W26 CI qualification and explicit external deployment/DR/release gates; production remains NO-GO until the CI packet is complete. |
| 2026-09-21 — Ozone IOPS CI gate | Extended the dependency-light storage benchmark with payload-size override, lifecycle IOPS measurement and hard minimum threshold; wired the 1,000-IOPS split-PGlite/R2 workload and artifact retention into `ozone-compositions`. | ~1.25 h | 0 h local; hosted CI pending | Benchmark unit tests and shell syntax checks passed. The live local gate is blocked by missing PGlite/N-API prerequisites; no performance pass is claimed until a terminal hosted job is reviewed. |
| 2026-09-21 — Ozone FoundationDB provider lane | Added a dedicated `ozone-foundationdb` hosted job for the durable three-node FoundationDB metadata composition over the live Ozone gateway; updated the Ozone test README, tracker and this ledger with the terminal-marker and evidence boundary. | ~0.75 h | ~0.25 h remote reconciliation; hosted result pending | Workflow and shell syntax are ready. The provider gate remains pending until a retained revision reaches terminal `FOUNDATIONDB_TEST_PASS ... service_restart=pass`, Ozone integration and cleanup. |
| 2026-09-21 — Ozone endpoint security boundary | Added static R2/Ozone endpoint validation for scheme, authority, credentials, query/fragment and control characters; added redaction-aware parser tests and an explicit CI invocation. | ~0.75 h | 0 h local; hosted CI pending | CLI configuration tests and workflow syntax are the local gate. Secure Ozone TLS/authentication and customer secret lifecycle remain separate hosted/customer gates. |
| 2026-09-21 — Plaintext remote endpoint guard | Restricted R2/Ozone HTTP endpoints to loopback and Docker test authorities in both the public R2 runtime config and the CLI parser; added tests for remote HTTP rejection while preserving local Ozone/RustFS fixtures. | ~0.75 h | 0 h local; hosted CI pending | R2 and CLI package tests are the local gate. This does not claim authenticated TLS or customer certificate/rotation acceptance. |
| 2026-09-21 — Ozone HTTP client/server end-to-end lane | Added the shipped `mount-rs serve-http` remote test to the Ozone composition job with binary/full/range I/O, cross-drive token rejection, graceful shutdown/reopen and an exact-run-prefix list/delete/re-list cleanup check. | ~1 h | 0 h local; hosted CI pending | Locked HTTP integration target compiled, shell/Node checks passed, and the unsafe-prefix cleanup guard failed closed. A terminal hosted marker is still required; loopback CI does not prove customer TLS, availability or native mounts. |
| 2026-09-21 — Local HTTP/Ozone execution boundary | Checked the local prerequisites for the new composition path. PGlite dependencies and the N-API artifact are present, but Docker cannot access `/var/run/docker.sock` in this environment. | ~0.1 h | 0 h test attempt; hosted CI required | Local live Ozone execution is explicitly blocked by the Docker daemon permission boundary, so no local live pass is claimed. The locked package and static harness checks remain green. |
| 2026-09-21 — HTTP security and observability qualification | Ran locked `mount-rs-http`, OTLP-enabled HTTP, full-feature observability, and CLI observability suites. | ~0.75 h | 0 h local; hosted collector/security review pending | Local auth/isolation, request/range bounds, lifecycle cleanup, telemetry redaction, local collector delivery and exporter-failure isolation passed. The evidence does not close deployed dashboards, secure customer Ozone auth/rotation, or the standard scan. |
| 2026-09-21 — HTTP production boundary hardening | Enforced loopback-only HTTP binds in the server and static CLI config, added configurable active-connection and total-request/header time bounds, and added stalled-body and excess-connection integration tests. | ~1.5 h | 0 h local; hosted/provider/security review pending | HTTP unit suite passed 8 tests, integration suite 8 tests, OTLP integration 9 tests, CLI suite 44 unit/9 CLI/2 subprocess/1 native-artifact tests, and workspace strict Clippy passed. Secure customer TLS/Ozone auth, provider matrix, hosted CI and final scan remain open. |
| 2026-09-21 — HTTP directory response bounds | Added configurable directory-entry and serialized-response limits to both `/entries` and directory-file routes, with fail-closed integration coverage for entry-count and byte limits. | ~1 h | 0 h local; hosted/provider/security review pending | HTTP unit suite passed 8 tests, default integration suite 10 tests, OTLP integration 11 tests, CLI suite 44 unit/9 CLI/2 subprocess/1 native-artifact tests, and strict workspace Clippy passed. The current core readdir API remains vector-based, so provider-side pagination/streaming and hosted Ozone evidence remain open. |
| 2026-09-21 — Ozone metadata-provider IOPS matrix | Added explicit SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 benchmark providers, configuration-gated availability, provider-specific durability labels and an Ozone IOPS invocation that applies the 1,000 target to every configured row. | ~1.5 h | ~0.1 h native build; live Ozone credentials/provider topologies unavailable locally | Benchmark unit tests, no-credential skip output, Node syntax checks and shell syntax passed. A rebuilt current N-API artifact passed the chunked lifecycle test. Hosted Ozone IOPS and provider-specific rows remain pending; absent TiDB/FoundationDB topologies are intentionally not treated as passes. |
| 2026-09-21 — Scoped immutable-block reconciliation | Added the explicit `BlockStore` reconciliation contract, lease-protected `ChunkedFs` root collection, R2/Ozone prefix-scoped aged-object deletion, default `ENOTSUP` behavior, Rust wrapper forwarding, N-API `reconcileBlocks` report/range contract and operator documentation. | ~2.5 h | ~0.1 h shared-target build/test wait; hosted/provider/collector/security gates pending | R2 14/14 tests, ChunkedFs 14/14 tests including committed/open-unlinked roots, SDK 2/2, observability 4/4, strict affected-package Clippy, formatting, diff checks and rebuilt N-API chunked integration passed. No provider-wide or production-retention pass is claimed. |
| 2026-09-21 — Reconciliation safety follow-up | Enforced positive grace at the `ChunkedFs` coordinator and changed R2/Ozone cleanup to consume the provider listing as a stream, avoiding whole-list materialization. | ~0.75 h | ~0.1 h shared-target compile/test wait; hosted/provider/collector/security gates pending | Locked R2 14/14 and ChunkedFs 14/14 tests, strict affected-package Clippy, formatting and diff checks passed. A fresh security review must target the final published revision; no provider-wide or production-retention pass is claimed. |
| 2026-09-21 — Bounded directory materialization hardening | Added `FsDriver::readdir_bounded`, made HTTP directory routes request the bound before serialization, implemented bounded enumeration for MemoryFs, HostFs, ChunkedFs and VersionedView, forwarded it through persisted/observability/CLI/native wrappers, and kept JavaScript-owned callbacks fail-closed. | ~2–3 h | ~0.25 h shared-target build/test wait; hosted CI run `35606250115` was superseded and is not evidence | HTTP 8 unit + 12 integration, core 11, host 5, chunked 14, CLI 44, observability 4 and persistence 4 local tests/targets passed; affected workspace check and formatting passed. The provider-specific follow-up is recorded below. |
| 2026-09-21 — Provider-bounded key enumeration and N-API bridge | Added the optional key-value provider contract, unstorage `getKeysBounded` callback bridge, public `Filesystem.readdirBounded` API, generated TypeScript declaration and Node error-shape wrapping. Legacy providers and structural JavaScript drivers fail closed when they cannot enforce the bound before materialization. | ~2.5–3 h | ~0.1 h host-process permission for N-API packaging; current CI run `35608567514` is non-terminal | KV integration tests 13/13, N-API Rust tests 16/16, strict affected-package Clippy, formatting/diff checks, release N-API build, unstorage bridge, typecheck and chunked smoke passed. MOUNTX_SOURCE parity was explicitly skipped because the source is unset; hosted provider/security gates remain open. |
| 2026-09-22 — Bounded unstorage parity fixtures | Added explicit capable-provider overflow/success coverage and a legacy-provider fail-closed `ENOTSUP` check to the unstorage/N-API qualification tests. | ~0.25–0.5 h | 0 h local; `MOUNTX_SOURCE` remains an external/source-gated parity check | `node integrations/mount-rs-napi/test/unstorage.mjs` passed, including capable and legacy fixtures; syntax checks and `git diff --check` passed. The raw provider parity harness now covers bounded overflow/success, but the full oracle comparison remains skipped when `MOUNTX_SOURCE` is unset. |

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
and blockers. Product direction now makes W26 a customer-deployed integration
qualification stream: its next acceptance target is a complete, secure,
all-feasible-provider, end-to-end CI packet, not a customer deployment.

The HTTP security-boundary chunk was committed as `f74ffce`, merged with
concurrent mainline changes, and pushed as `b997696`. The current-head hosted
workflow is tracked separately above; a queued or in-progress run is not
promoted to production evidence.

The Ozone provider-matrix/IOPS chunk was committed as `333c764`, merged with
concurrent mainline changes, and pushed as `a0b2fae`. Its local evidence is
recorded above; the hosted workflow created from `a0b2fae` must reach terminal
success before the provider or 1,000-IOPS gates move beyond pending.

The scoped block-reconciliation chunk was implemented in `ab7c63e`, with the
zero-grace and streamed-listing safety follow-up in `502ba35`; both were merged
with concurrent mainline changes and pushed at `69dd740`. Their local evidence
is recorded above; a fresh hosted workflow and provider/retention/security
review are required before the lifecycle production gates move beyond pending.

The bounded directory-materialization chunk was implemented in `fddf4e1`, merged
with concurrent mainline changes, and pushed at `9de159a`. Its local evidence
is recorded above. The provider-bounded key-enumeration follow-up was committed
as `ab551be`, merged with concurrent mainline changes, and published at
`c719306`. Its local evidence is recorded above; the current CI run
`35608567514` has Ozone and Ozone-TiDB jobs in progress with Ozone compositions
and FoundationDB queued, so none of those current-revision states is promoted
to a pass. Providers without a backend-enforced listing limit remain
fail-closed, and `MOUNTX_SOURCE` parity remains an explicit environment gate.

The bounded unstorage parity fixtures were committed as `8057fd1`, merged with
concurrent mainline changes, and published at `d619f93`; the published revision
was verified to match `origin/main`. The capable-provider overflow/success and
legacy-provider `ENOTSUP` paths pass locally. Its current CI run `35609794020`
is pending and Fault injection run `35609793694` is in progress, so neither is
evidence yet; `MOUNTX_SOURCE` remains unset and its oracle parity is still open.
