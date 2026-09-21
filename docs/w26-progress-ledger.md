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
| Snapshot base | `1775895` (latest published origin/main head carrying the W26 evidence-packet verifier, retained acceptance logs, fail-closed artifact/profile/metric checks and concurrent mainline changes; no matching current-tip hosted W26 result was visible at snapshot time and no local result is promoted) |
| Checklist completion | 9 of 9 W26 tracker rows checked: 100% implementation scope; hosted/provider production gates remain open |
| Provisional execution completion | W26 qualification: 100%; production-rollout readiness: 44% (scope, benchmark matrix, lifecycle safety, bounded remote enumeration, provider-bounded KV/N-API contract, durable-provider bounded-listing tests, strict provider-specific hard-threshold IOPS wiring, fail-closed artifact/profile/metric verification and retention, all-provider credential-free production-config policy, and one-revision CI evidence-packet aggregation are implemented and locally tested; no terminal production gates yet). Customer deployment, native, provider-durability, capacity/SLO, backup/DR and release-stream gates remain separately bounded |
| Current acceptance state | The retained W26 packet on `9c098e5` remains accepted within its documented provider/platform boundaries. The newer provider-bounded KV/N-API implementation, unstorage parity fixtures, SQLite/PGlite/TiDB/FoundationDB Ozone bounded-listing assertions, FoundationDB and TiDB Ozone Node/N-API bounded assertions, exact-limit regression, SDK debug redaction, dedicated TiDB/FoundationDB Ozone IOPS harnesses, strict provider-configuration qualification, fail-closed IOPS artifact/profile/metric verification and retention policy, expanded credential-free all-provider production-config policy, and complete evidence-packet verifier are locally green, syntax-checked or compile-checked. Latest published head `1775895` includes the metric-integrity chunk; no matching current-tip hosted W26 result was visible at snapshot time. No current-tip W26 result exists and no current-tip Live Cloudflare R2 or Live AWS S3 result is recorded. None is W26 acceptance evidence. Focused IOPS diff scans `5fc5a943-07bb-4979-9f37-efd87a7f505e`, `60269206-bb22-4b78-aaf7-f05d16ffcca0`, `1d97028f-e153-4b49-9fac-c3a8c1fc1117`, `c67ae8e0-99af-4ade-8284-a612d599b5e4` and `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564` found zero reportable findings with hosted/provider coverage explicitly deferred; production-config scan `d74e3e86-2e0a-45cf-9819-e31f428eb5d4`, negative-path scan `7addeeb5-4601-4951-aca9-becffb9bd4b9` and retention scan `18010cad-ed69-4da3-b0a9-57163091e878` also found zero; `MOUNTX_SOURCE` parity and hosted/customer security controls remain bounded |
| Latest hosted workflow | GitHub Actions run `35585066458` on `9c098e5`; W26 jobs `ozone` `106286459564`, `ozone-compositions` `106286459622`, and `ozone-tidb` `106286459540` completed successfully, with generic `tidb` job `106286459436` also green. Unrelated provider/native jobs are tracked separately and are not required to close W26. |
| Current-head CI attempt | Published tip `1775895` contains the W26 metric-integrity extension; no matching current-tip workflow result was visible at snapshot time. No Live Cloudflare R2 or Live AWS S3 result was recorded for this tip. Only terminal W26 jobs with strict provider markers, retained logs/artifacts and a passing aggregate packet verifier can be promoted; the retained W26 packet above remains the last accepted hosted result. |
| Local Docker boundary | Docker Desktop capacity was about 5 CPUs and 8.2 GiB; this is sufficient for the durable FoundationDB proof but below the TiDB harness's 10 GiB durable-topology minimum |
| Production rollout track | Open, currently **NO-GO**; 0 of 15 production gates are terminally accepted. The 44% figure reflects scope decisions, local hardening, provider-boundary implementation, strict provider-configuration and artifact-integrity/metric/retention qualification, expanded credential-free security policy and one-revision evidence-packet aggregation, not deployable readiness |
| W26 production target | Customer-deployed Ozone integration; W26 owns provider/client correctness and CI qualification, not customer deployment, backup/DR or release promotion |
| Required service envelope | Target 1,000 IOPS per drive; Tier 1 99.99% reliability; 5-minute RPO and 5-minute RTO. RPO/RTO and availability remain dependent on the customer's Ozone topology and operations |
| Available qualification environment | CI only; no staging environment is available. Production-like evidence must therefore be achieved through controlled hosted CI/provider fixtures and clearly labeled customer-owned prerequisites |
| Release/acceptance decision | W26 qualification accepted within the documented local/hosted provider scope on terminal run `35585066458`; latest completed standard security scan `5ad61e60-20e3-4223-885a-d4b516d49bb1`, focused production-config diff scan `d74e3e86-2e0a-45cf-9819-e31f428eb5d4`, negative-path scan `7addeeb5-4601-4951-aca9-becffb9bd4b9`, strict IOPS diff scans `1d97028f-e153-4b49-9fac-c3a8c1fc1117` and `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564`, and evidence-packet diff scan `c67ae8e0-99af-4ade-8284-a612d599b5e4` report zero reportable findings, while hosted/provider follow-up remains deferred; production rollout remains **NO-GO** and no broader native release claim is made |

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
| W26.3c — independent durable three-node FoundationDB composition over Ozone | Implementation and local proof complete; hosted Ozone/IOPS lane pending | 90% base; 85% extension | The arm64 durable run used three fixed-address FoundationDB 7.4.7 containers, three coordinators, persistent per-server volumes, `double` redundancy and SSD storage. Transaction probes passed before and after node-2 restart; first-client and fresh-client Ozone compositions passed; `FOUNDATIONDB_TEST_PASS topology=durable`; owned resources were removed and verified absent. The published test extension `d1c9e44` asserts provider-backed bounded listing and `EOVERFLOW` in both composition and reopen paths. `b80c19c`, published at `0842474`, additionally enables the feature-built Node/N-API client in the Ozone FoundationDB job; its test asserts bounded success/overflow in seed/reopen and scopes the Node prefix below the owned Ozone run prefix. Code chunk `de9d267`, published in reconciled tip `414a469`, now enables a hard `--min-iops 1000` TiDB/FoundationDB-compatible benchmark path for the FoundationDB Ozone Node client, validates positive settings before container setup, retains JSON evidence and emits a pass marker only after a successful run. TiDB/FoundationDB compile checks pass locally; FoundationDB binary linking is blocked on this macOS host by missing `libfdb_c`. CI has a dedicated `ozone-foundationdb` job that runs the durable Rust, Node and IOPS paths against the live Ozone gateway. | Review a terminal `ozone-foundationdb` job on a retained revision and record the exact FoundationDB/Ozone versions, `FOUNDATIONDB_RUSTFS_CHUNKED_BOUNDED_READDIR_PASS`, `FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS`, `FOUNDATIONDB_OZONE_IOPS_PASS`, retained IOPS JSON, restart marker, cleanup and client image evidence. | 0.75–1.25 h hosted review | Hosted CI capacity, nested FoundationDB client image startup, provider/native runtime and a terminal retained artifact are external. Local and hosted proofs remain loopback/nonsecure and do not cover production auth/TLS, power-loss behavior or customer replication. |
| W26.3d — durable multi-node TiDB composition over Ozone | Rust harness and hosted acceptance complete; Node/N-API and IOPS Ozone extensions pending | 100% base; 90% extension | Final hosted run `35585066458`, Ozone-specific job `106286459540`, used `MOUNT_RS_OZONE_TIDB_COMPOSITION=1`, durable TiDB v8.5.7, and the real Ozone gateway. It emitted `TIDB_CHUNKED_RUSTFS_SEED_PASS`, `TIDB_CHUNKED_RUSTFS_REOPEN_PASS`, `TIDB_ACCEPTANCE evidence=durable-multinode-restart topology=durable version=v8.5.7 platform=linux/amd64 cpus=4 mem_bytes=16765378560 ambiguous_commit=pass`, `OZONE_INTEGRATION_PASS`, and `OZONE_CLEANUP_PASS`. The generic durable TiDB job `106286459436` also emitted the same acceptance marker. Published `ef6a876` adds a feature-built public Node/N-API TiDB Ozone test with bounded success/overflow in seed/reopen and a prefix under the owned Ozone scope; the current hosted run has not reached a terminal result for that extension. Code chunk `de9d267`, published in reconciled tip `414a469`, now invokes the same public Node split-provider benchmark with hard `--min-iops 1000`, positive-setting validation and a retained JSON artifact in the dedicated durable TiDB Ozone job; it emits `TIDB_OZONE_IOPS_PASS` only after the runner succeeds. | Review a terminal `ozone-tidb` job on a retained revision for `TIDB_NAPI_BOUNDED_READDIR_PASS` in seed/reopen, `TIDB_OZONE_IOPS_PASS`, retained IOPS JSON, the durable restart marker, Ozone integration and cleanup. | 0.75–1.25 h hosted review | GitHub-hosted Docker/TiDB/PD/TiKV, N-API build, Ozone and a terminal retained artifact are hosted provider gates. This does not claim production TLS/authentication, power-loss durability or native mounting. |
| W26.4a — Node provider matrix and Rust/Node CLI coverage | Implementation and local/hosted composition gate complete | 100% | The live arm64 composition run passed Node SDK coverage including PGlite-to-S3 partial/truncate/reopen (`pass=7 skip=1 fail=0`), Node CLI coverage, and the matching ignored Rust CLI live Ozone split-provider/reopen test. Final hosted `ozone-compositions` job `106286459622` passed on `35585066458` with `SUMMARY node-sdk pass=7 skip=1 fail=0`. | Keep the Node matrix's one intentional skip and native/provider boundaries visible. | 0 h acceptance; 0–1 h review | Hosted NAPI/PGlite build and Linux Node runtime are provider/native gates. Local arm64 evidence is not Linux-amd64 evidence. |
| W26.4b — CI wiring, pinned images, cleanup, and documentation | HTTP, durable-provider IOPS, credential-free production-config policy and one-revision evidence-packet gates implemented; hosted result pending | 100% implementation; 92% hosted packet | CI installs NAPI/PGlite with locked Rust dependencies, runs the Ozone composition matrix, Node/CLI coverage, the shipped HTTP server/client reopen path, and durable Ozone/TiDB/FoundationDB jobs. The HTTP runner restricts its prefix below the unique Ozone run scope and verifies deletion before returning. FoundationDB harness uses platform-specific pinned official 7.4.7 image manifests and transaction readiness probes; the dedicated `ozone-foundationdb` job is wired for the Ozone-backed durable mode. The published `de9d267` code chunk adds hard-threshold TiDB/FoundationDB IOPS invocation, fixed artifact paths and always-upload steps for the dedicated jobs. The published `b8d8fb1` policy chunk adds the credential-free all-provider configuration step; `7018b59` adds independent inline-secret, FoundationDB-authority and TiDB-TLS downgrade rejection checks. The current `08f4530` chunk retains the base policy/recovery log and all three provider acceptance logs/JSON artifacts, then runs a fail-closed aggregate verifier against one checkout revision. | Run and review the aggregate `w26-ozone-evidence` job on a retained revision; record the HTTP pass, IOPS JSON/pass markers, all production-config policy PASS/FAIL output, provider acceptance and cleanup markers; keep W26 production readiness NO-GO until all open gates are terminal. | 0.75–1.5 h hosted review | GitHub workflow concurrency, nested provider startup, NAPI/PGlite builds and other workstream pushes can cancel runs; canceled/queued/failed jobs, missing logs/artifacts and aggregate verifier failures are not evidence. |
| W26.7 — credential-free Ozone production configuration policy | Implementation complete; hosted/customer security gate open | 100% implementation; 60% production qualification | `scripts/verify-w26-ozone-production-config.mjs` rejects inline secret-like values, unknown provider fields, non-HTTPS R2 endpoints, unsafe paths/prefixes, non-durable metadata and non-TLS TiDB URLs. Positive SQLite, PGlite, TiDB and FoundationDB fixtures plus independent HTTP, inline-secret, FoundationDB-authority and TiDB-TLS negative fixtures are exercised in the `ozone` CI job without provider connections or real credentials. Local positive/negative execution passed. Commit `b8d8fb1` was published in `4d8b5fd`; expansion `7018b59` was published in `6a1b94b`. Focused scans `d74e3e86-2e0a-45cf-9819-e31f428eb5d4` and `7addeeb5-4601-4951-aca9-becffb9bd4b9` found zero reportable findings with partial external/provider coverage. | Obtain terminal CI output on a retained revision; bind the policy to customer Ozone endpoint/IAM/certificate/secret-rotation evidence and confirm runtime deployment configuration matches the validated references. | 2–3 h implementation; 1–2 h review/handoff | Hosted CI execution, customer Ozone TLS/IAM, secret manager and rotation, host permissions, provider-native security and audit evidence remain external. |
| W26.5 — explicit immutable-block reconciliation and open-unlink safety | Implementation and local contract tests complete; provider/platform acceptance open | 100% implementation; 50% production qualification | `BlockStore::reconcile` now fails closed by default; `ChunkedFs::reconcile_blocks` rejects zero grace before taking the lease, renews the writer lease, roots the committed namespace and open-unlinked handles, and delegates scoped cleanup. R2/Ozone blocks stream only their validated prefix, retain live/recent objects, delete only aged unreferenced objects and return bounded counts without materializing the entire listing. Rust SDK, observability, fault-injection and N-API wrappers forward the capability; locked R2/chunked/wrapper tests, strict Clippy and the rebuilt N-API chunked test passed. | Add provider-native enumeration/reconciliation where supported or retain explicit `ENOTSUP`; exercise ambiguous publication, object loss, quotas/space pressure, metrics/alerts and the customer/Ozone maintenance owner in hosted CI. The current-tip security scan is complete with zero local reportable findings, but its hosted/customer follow-ups remain open. | ~2.5–4.5 h implementation and local verification; ~0.5–2 d hosted/provider/security review | Ozone/customer retention policy, provider listing/deletion semantics, hosted credentials/topologies and alert collector are external gates. |
| W26.6 — bounded remote directory enumeration and response materialization | Built-in and KV/N-API provider-boundary implementation complete; provider/native qualification open | 100% implementation; 81% production qualification | `FsDriver::readdir_bounded` fails closed with `ENOTSUP` by default. HTTP `/entries` and directory-file routes request the bound before serialization and map overflow to the existing 413/connection-close contract. Memory, host, chunked, versioned, persisted, observability, CLI and native N-API wrappers implement or forward the boundary. `KeyValueStore::get_keys_bounded` is an optional provider-side contract; the unstorage bridge exposes `getKeysBounded(prefix, maxKeys)` and `Filesystem.readdirBounded(path, maxEntries)`, while providers without the callback remain fail-closed. Terminal local evidence: KV 13 integration tests, N-API Rust 16 tests, HTTP 8 unit + 12 integration, core 11, host 5, chunked 14, CLI 44, observability 4 and persistence 4; release N-API packaging, unstorage bridge, typecheck, chunked smoke, strict affected-package Clippy, formatting and diff checks passed. The parity fixtures pass capable-provider overflow/success and absent/legacy-callback `ENOTSUP`; SQLite/PGlite Ozone composition asserts bounded success and `EOVERFLOW`; the adapter forwards exactly the caller's limit with a 13-test regression asserting `[2, 3]`. Published `d1c9e44` adds the same provider-backed success/overflow and seed/reopen assertions to the TiDB/RustFS and FoundationDB/RustFS composition lanes. `b80c19c` adds the feature-built FoundationDB Node/N-API Ozone test, and `ef6a876` adds the corresponding TiDB Node/N-API Ozone test; both assert bounded success/overflow in seed/reopen and scope their prefixes under the owned Ozone run. TiDB test compilation and strict Clippy pass; FoundationDB `cargo check --tests` passes, while local test-binary linking is blocked by missing `libfdb_c`; both non-feature Node invocations skip safely on this host. `MOUNTX_SOURCE` parity remains skipped because the provider source is unset. Current standard security scan `5ad61e60-20e3-4223-885a-d4b516d49bb1` is complete at `44b01a7` with zero reportable findings across 16 W26-relevant surfaces; semantic coverage is explicitly partial and defers customer Ozone TLS/IAM/rotation, provider-native allocation, dependency provenance, native/platform and production SLO/recovery controls. | Review terminal Ozone composition results for all provider-backed assertions, exercise the contract in the remaining hosted provider jobs, add provider-native pagination where a backend can safely enforce it, retain `ENOTSUP` for legacy providers, run `MOUNTX_SOURCE` parity, and retain the explicit security follow-ups. | ~3.5–5.5 h implementation/local verification; ~1–3 d provider/security/hosted review | Provider-side key enumeration, JavaScript callback implementation, hosted provider topologies, secure Ozone fixture, native FoundationDB library, mountx source and customer security controls remain external or cross-workstream gates. |

| W26.8 — strict provider-configuration IOPS qualification | Implementation complete; hosted performance evidence open | 100% implementation; 42% production qualification | Added `--require-configured` to the storage benchmark. In strict mode every requested provider skip becomes a failed result with machine-readable missing-variable names; non-strict local matrices retain explicit skips. The generic Ozone composition lane now requests only its configured SQLite/R2 and PGlite/R2 rows, while dedicated TiDB and FoundationDB Ozone IOPS lanes invoke strict mode. Local benchmark unit tests, the missing-TiDB/R2 regression, Node/shell syntax, YAML parsing and diff checks passed. Commit `d46e091` was published in merged tip `74fe4c7`. Focused scan `60269206-bb22-4b78-aaf7-f05d16ffcca0` found zero reportable findings across the changed qualification surfaces; hosted/provider controls remain partial/deferred. | Review terminal `ozone-compositions`, `ozone-tidb` and `ozone-foundationdb` results on one retained revision; require strict provider markers, JSON artifacts, lifecycle success and no configuration failures before moving P2/P8 forward. | 1–1.5 h implementation; 0.75–1.25 h security review; 0.5–1 h hosted review | GitHub runner/provider startup, PGlite/TiDB/FoundationDB topology, Ozone credentials, artifact retention, provider TLS/IAM and customer capacity remain external. |

| W26.9 — IOPS artifact integrity and fixed production profile | Implementation complete; hosted performance evidence open | 100% implementation; 48% production qualification | Added `scripts/verify-w26-ozone-iops-artifact.mjs`, which treats the JSON as untrusted evidence and requires the exact requested provider set, `requireConfigured=true`, 4 KiB/400-iteration/concurrency-64 profile, target >=1,000 IOPS, zero skipped/configuration-failed rows, successful cleanup and per-size lifecycle success before a wrapper emits its provider pass marker. Generic, TiDB and FoundationDB Ozone wrappers reject weakened profile/target settings and invoke the verifier. Local benchmark unit tests, Node/shell syntax and diff checks pass. Implementation commit `66f3670` was published in merged tip `00d2b80`; focused scan `1d97028f-e153-4b49-9fac-c3a8c1fc1117` found zero reportable findings across five changed source surfaces; hosted/provider controls remain partial/deferred. | Review terminal generic, TiDB and FoundationDB artifacts for verifier PASS markers, exact profile, provider set, cleanup and retained JSON on one revision; add soak/capacity variants only after the fixed qualification packet is green. | 1.5–2.5 h implementation/tests; 0.75–1.25 h security review; 0.5–1 h hosted review | Hosted provider startup, artifact retention/access, provider TLS/IAM, customer capacity and Ozone topology remain external. |
| W26.10 — fail-closed retention of IOPS evidence artifacts | Implementation complete; hosted performance evidence open | 100% implementation; 50% production qualification | Changed the three W26 Ozone IOPS artifact uploads in `.github/workflows/ci.yml` from `if-no-files-found: warn` to `error`, so a missing generic, TiDB or FoundationDB JSON artifact fails the evidence job even when the test step is already in an `always()` upload path. CI YAML parsing, shell syntax, benchmark unit tests and diff checks pass locally. Commit `c0f8370` was published in merged tip `12ba117`; focused scan `18010cad-ed69-4da3-b0a9-57163091e878` found zero reportable findings. | Review one terminal retained run for each artifact and confirm the verifier PASS marker, uploaded JSON and provider/job identity remain aligned. | 0.25–0.5 h implementation/local verification; 0.5–1 h security/hosted review | GitHub artifact service, workflow concurrency, hosted provider startup and terminal W26 job completion remain external. |
| W26.11 — aggregate one-revision Ozone evidence packet | Implementation complete; hosted aggregate packet open | 100% implementation; 20% production qualification | Added `scripts/verify-w26-ozone-evidence-packet.mjs` and unit coverage that treat retained JSON/log files as untrusted evidence. The verifier requires the exact SQLite/PGlite, TiDB and FoundationDB provider sets, clean matching source revisions, all eight positive/negative policy markers, provider acceptance, Ozone integration/fault/recovery and cleanup markers. `.github/workflows/ci.yml` now tees policy/base/provider logs into fail-closed artifacts and adds an `always()` aggregate job that downloads every artifact and verifies the complete packet. Local benchmark/evidence unit tests, Node/shell syntax, YAML parse and diff checks passed. Implementation commit `08f4530` was published after remote reconciliation at `097ed00`; security diff scan `c67ae8e0-99af-4ade-8284-a612d599b5e4` found zero reportable findings with hosted/provider/customer coverage explicitly deferred. | Review a terminal `w26-ozone-evidence` job on the same revision as all four producer jobs; record retained artifact names, exact source revision, policy markers, per-provider 1,000-IOPS result, acceptance/restart/fault markers and cleanup outcome. | 1.5–2.5 h implementation/tests; 0.75–1.25 h security review; 0.5–1 h hosted review | GitHub artifact service, runner/provider startup, Ozone credentials/TLS/IAM, customer capacity/SLOs, native clients, Ozone backup/DR and release ownership remain external. |
| W26.12 — strict IOPS metric and payload-map integrity | Implementation complete; hosted performance evidence open | 100% implementation; 24% production qualification | Extended `scripts/verify-w26-ozone-iops-artifact.mjs` to require the configured payload-size map, 100% lifecycle success, finite elapsed time and operation percentiles, zero timeout/cleanup failures, exact write/read/delete/verified-read counts and complete statistic sample counts before a pass marker is accepted. Added synthetic negative cases for payload-map mismatch and timeout evidence. Local benchmark/evidence unit tests, Node/shell syntax, YAML parse and diff checks pass. Commit `e875ba6` was published after concurrent reconciliation at `1775895`; security diff scan `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564` found zero reportable findings with hosted/provider/customer coverage explicitly deferred. | Review terminal provider artifacts for finite p95/p99, resource envelope, exact lifecycle counts and fixed payload mapping; then run meaningful soak/capacity variants on the accepted hosted topology. | 0.75–1.25 h implementation/tests; 0.75–1.25 h security review; 0.5–1 h hosted review | Hosted provider startup, artifact service, customer capacity, latency/SLO interpretation and Ozone topology remain external. |

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
| W26 integration readiness | **NO-GO** | W26 qualification is green on hosted run `35585066458`, and the durable-provider IOPS plus credential-free production-config policy gates are implemented in `4d8b5fd`, but provider-backed bounded-listing assertions, all-feasible-provider CI, secure integration checks, 1,000-IOPS results and complete end-to-end/security coverage are not yet terminal | All W26-owned P0–P5, P7–P8, P10–P11 and P14 evidence is terminal on one retained revision; customer Ozone deployment, DR and release remain explicit external dependencies |
| Customer production target | Customer-deployed Ozone; topology not supplied | Product direction fixes the service envelope at 1,000 IOPS per drive, 99.99% reliability and five-minute RPO/RTO, but W26 does not operate the customer topology | W26 documents the Ozone/provider/client contract; customers and deployment streams provide secure topology, backup/DR, monitoring and measured availability/recovery evidence |
| Qualification baseline | Complete for W26 only | Revision `9c098e5` and hosted run `35585066458` are the retained W26 acceptance packet; implementation tip `f3aae7d` carries the policy/IOPS gates and one-revision evidence-packet verifier but its current CI was canceled before jobs started | Any production implementation change gets a fresh qualification and production evidence packet on the tested revision |

### Production gate ledger

| Gate / work item | Work type | Status | Completion | Evidence now available | Remaining actions / exit evidence | Provisional engineering time | External blockers / hosted or native gates |
| --- | --- | --- | ---: | --- | --- | ---: | --- |
| P0 — production scope, support matrix, SLO/RPO/RTO and ownership | Implementation / operations | Scope captured; CI acceptance baseline open | 60% | Product direction now records customer-deployed Ozone, all feasible metadata providers, 1,000 IOPS per drive, 99.99% reliability and 5-minute RPO/RTO; no customer topology is supplied | Turn these targets into provider-specific CI assertions, define the advertised client/platform matrix, document customer/Ozone-owned prerequisites and obtain owner sign-off on the support matrix | 0.5–1.5 d | Product/support decisions are mostly supplied; provider support limits and customer deployment owners remain external |
| P1 — customer Ozone topology and deployment rehearsal | External dependency — not a W26 deployment task | Customer-owned / not measured by W26 | 0% W26 deployment evidence | Current Ozone evidence is a pinned all-in-one, non-secure, loopback CI fixture with anonymous volumes and no production replication claim | Customer/deployment stream must provision and operate secure multi-node Ozone; W26 consumes CI-accessible endpoints or fixtures and documents the required topology contract | 0–1 d W26 contract review | Customer infrastructure, persistent storage, network policy, image architecture, certificates and environment access |
| P2 — production metadata-provider support matrix | Hosted/provider CI | Ozone provider matrix and durable-provider hard-threshold paths expanded; terminal all-provider evidence pending | 62% | The benchmark has explicit SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 rows. The generic Ozone composition now requests its configured SQLite/R2 and PGlite/R2 rows with `--require-configured`; dedicated TiDB/FoundationDB paths are also strict, so a requested but unavailable provider fails the qualification result instead of being promoted as a pass. The new artifact verifier independently requires the exact requested provider set and rejects skipped/configuration-failed rows before any wrapper pass marker. The key-value contract can enforce a bound through `get_keys_bounded`, and unstorage can opt in through `getKeysBounded`; capable and legacy/absent callback fixtures pass locally. SQLite/PGlite Ozone composition plus the TiDB/RustFS and FoundationDB/RustFS composition tests exercise bounded success and overflow against provider-backed `ChunkedFs`; the feature-built FoundationDB and TiDB Node/N-API Ozone lanes exercise the same contract. Published code chunk `de9d267` at `414a469` adds dedicated TiDB/FoundationDB Ozone IOPS invocations, positive-setting validation and retained JSON/pass-marker paths, while `d46e091` at `74fe4c7` makes requested-provider skips fail closed and `66f3670` at `00d2b80` validates the retained artifact. No terminal hosted provider-parity or performance result exists. | Retain terminal Ozone results for each configured provider on one revision, including `ozone-tidb` and `ozone-foundationdb` artifacts/markers; record provider versions/HA/failure semantics and unsupported combinations, add capable KV/provider rows, and do not promote an explicitly skipped provider | 3–8 d | CI capacity, provider images/versions, provider-side listing APIs, managed-service access if required and provider-specific operator limits |
| P3 — authentication, TLS, secret lifecycle and redaction | Implementation / hosted/provider CI | Local transport and expanded credential-free production-config policy hardened; secure integration gate open | 60% | R2/Ozone config parsing and runtime validation reject non-HTTP(S), embedded credentials, query/fragment, missing-authority and remote plaintext-HTTP endpoints before client construction; HTTP config and runtime now reject non-loopback binds, require loopback behind a TLS reverse proxy, keep credentials as environment references and redact diagnostics. The `b8d8fb1` policy gate validates SQLite/PGlite/TiDB/FoundationDB metadata shapes, HTTPS R2 blocks, exact external secret references and TiDB TLS options without provider connections; `7018b59` adds independent inline-secret, shared-provider-authority and TiDB hostname-verification negative cases. Local positive/negative policy checks passed | Add authenticated HTTPS endpoint CI where available, certificate identity/rotation checks, secret injection/rotation references, least privilege and clean-client negative tests; retain no secret values; review customer runtime configuration against the policy | 3–7 d | Secure Ozone CI endpoint or customer-supplied fixture, certificates/identity, secret manager integration, customer IAM and security review |
| P4 — replicated block durability and storage failure protection | Provider/customer deployment dependency plus CI contract | Lifecycle protection implemented; durability qualification open | 40% | Restart/reopen and durable provider checks pass in bounded CI topologies. The explicit reconciliation path rejects zero grace, protects committed and open-unlinked roots, retains a configurable grace window, streams the configured R2/Ozone prefix and scopes deletion to validated block IDs; no customer storage, power-loss or Ozone replication guarantee exists | Test client behavior for object loss, unavailable gateway, retries, integrity mismatch and recovery in CI; document Ozone replication/fsync/storage requirements, retention ownership, deletion authorization and space-pressure alerts that customers must satisfy | 1–3.5 d W26 CI work; deployment work external | Ozone storage and replication semantics, failure controls, retention policy and customer topology; CI restart is not power-loss evidence |
| P5 — fencing, ambiguous commit and stale-writer recovery under failover | Implementation / hosted/provider CI | Lease-protected reconciliation implemented; failover matrix open | 35% | W26 exercises CAS, stale fencing, ambiguous commit and durable restart in bounded provider compositions; hosted TiDB marker reports `ambiguous_commit=pass`; reconciliation renews the writer lease before deriving roots and never runs implicitly on shutdown | Extend all feasible Ozone/provider CI lanes with concurrent clients, retry, gateway/provider loss, delayed responses and post-ambiguity reconciliation; prove no stale publication, duplicate block or lost acknowledged commit | 3–7 d | Distributed CI fault controls, provider failover behavior and multiple-client scheduling |
| P6 — backup, restore, disaster recovery and retention | External dependency — Ozone/customer owned | Not a W26 implementation task | 0% W26 DR evidence | Product direction assigns backup and DR to Ozone/customer deployment; W26 has no competing backup system | Document the Ozone/customer requirements needed to meet 5-minute RPO/RTO and test W26 reopen/error behavior around supplied recovery scenarios when CI fixtures expose them | 0.5–1.5 d W26 contract documentation | Ozone backup/replication/restore design, failure domains, KMS and customer operations |
| P7 — observability, alerts, dashboards and runbooks | Implementation / CI contract / cross-workstream | Local HTTP/OTLP and provider-boundary evidence passed; deployment integration open | 45% | `mount-rs-http` passed 8 unit and 12 integration tests; the OTLP-enabled HTTP suite passed 11 integration tests including bounded error telemetry; full-feature observability passed 5 unit tests plus local collector and exporter-failure tests; CLI observability passed 44 unit, 9 CLI, 2 HTTP subprocess and 1 native-artifact test. `reconcileBlocks` returns scanned/protected/recent/deleted counts and fails closed when unsupported, while `readdir_bounded` is forwarded through observability/CLI wrappers. The N-API unstorage bridge also preserves the provider-boundary `EOVERFLOW` contract. These are local collector/fixture results, not deployed alerting. | Add/retain machine-readable Ozone/provider error categories, reconciliation and bounded-listing metrics, and health evidence in the hosted packet; coordinate dashboards, alerts and runbooks with W30/customer operations | 2–5 d W26 contract/tests | Collector reachability, alerting and paging are external; W30 and customer operations own deployed dashboards/paging |
| P8 — load, capacity, soak and cost envelope | Hosted/provider CI | Per-provider hard-threshold and one-revision packet gates implemented; terminal hosted results pending | 55% | The benchmark records successful write+read+delete lifecycle IOPS, supports a hard `--min-iops` threshold, strict `--require-configured` provider qualification and redacted JSON. The generic `scripts/test-ozone.sh` lane requests SQLite/R2 and PGlite/R2 only, with strict mode; dedicated TiDB and FoundationDB lanes request their own rows with the same strict mode. Wrapper settings cannot lower the target below 1,000 or weaken the 4 KiB/400-iteration/concurrency-64 production profile. `scripts/verify-w26-ozone-iops-artifact.mjs` requires the exact provider set, zero skipped/configuration-failed rows, successful cleanup, per-size lifecycle success, target attainment and a valid retained JSON artifact before the generic or dedicated pass marker is emitted. The three W26 IOPS uploads now fail the CI job when an expected JSON artifact is absent. The new `w26-ozone-evidence` job also retains policy/base/provider logs and requires the four producer artifacts, matching source revisions, policy negative-path markers, provider acceptance, Ozone fault/integration and cleanup markers before packet success. Published code chunk `de9d267` at `414a469` extends the dedicated durable TiDB and FoundationDB Ozone jobs to retain provider-specific artifacts; `66f3670` at `00d2b80` adds the fixed-profile/artifact gate, `c0f8370` at `12ba117` makes retention fail closed and `08f4530` at `097ed00` adds one-revision aggregation. Benchmark/evidence unit tests, missing-provider regression, verifier negative cases, shell syntax, YAML parsing and rebuilt N-API chunked lifecycle checks pass; no live local IOPS result is claimed. | Run terminal hosted `ozone`, `ozone-compositions`, `ozone-tidb`, `ozone-foundationdb` and `w26-ozone-evidence` jobs on one revision; retain all logs/JSON; verify exact profile, strict no-skip behavior, artifact upload, p95/p99 latency, errors, CPU/memory and topology, then add soak/capacity variants before treating the target as qualified | 3–8 d after harness implementation | Stable hosted CI runners, artifact service, Ozone fixture startup, provider quotas/topologies and enough runtime for meaningful soak; CI does not prove customer capacity or 99.99%/RPO/RTO |
| P9 — upgrade, rollback and compatibility | External release/deployment dependency | Not a W26 release task | 0% W26 migration evidence | W26 pins Ozone 2.2.1 and provider fixture versions for qualification only; release execution belongs to another stream | Supply compatibility notes, config/schema/object invariants and requalification commands for the release stream; do not own promotion or rollback automation here | 1–3 d W26 compatibility notes | Release stream, maintained provider versions, change window and customer deployment approval |
| P10 — security, privacy, tenancy and audit review | Published-tip local security review complete; hosted/customer security remains open | 78% | The standard scan `5ad61e60-20e3-4223-885a-d4b516d49bb1` completed against published revision `44b01a7` with zero reportable findings across 16 W26-relevant surfaces. Focused diff scans `5fc5a943-07bb-4979-9f37-efd87a7f505e`, `60269206-bb22-4b78-aaf7-f05d16ffcca0` and `1d97028f-e153-4b49-9fac-c3a8c1fc1117` of the IOPS harness/workflow surfaces, retention scan `18010cad-ed69-4da3-b0a9-57163091e878`, production-config diff scan `d74e3e86-2e0a-45cf-9819-e31f428eb5d4`, negative-path scan `7addeeb5-4601-4951-aca9-becffb9bd4b9` and evidence-packet scan `c67ae8e0-99af-4ade-8284-a612d599b5e4` all found zero reportable findings; their coverage is intentionally partial because hosted/provider controls remain unmeasured. The credential-free policy review now covers external secret references, HTTPS R2 endpoint shape, scoped prefixes, durable metadata, shared-provider FoundationDB authority and TiDB TLS verification across SQLite/PGlite/TiDB/FoundationDB fixtures, with local positive/negative execution. Existing source/local evidence also covers R2 endpoint/TLS policy, HTTP/S3 loopback-only binding, bearer isolation/constant-time comparison, redaction, connection/request bounds, pre-materialization directory limits, telemetry boundaries, scoped cleanup checks and early positive-integer IOPS validation; strict IOPS qualification now turns missing requested provider configuration into a failure while reporting names only, and the artifact verifier rejects malformed/weak profiles without printing artifact contents; W26 IOPS artifact uploads now fail closed when expected evidence is missing; retained logs are marker-checked without printing their contents; SDK `StoreConfig` debug output redacts provider credentials; bounded provider assertions cover SQLite/PGlite/TiDB/FoundationDB code paths. Customer Ozone TLS/IAM/rotation, provider-native allocation, dependency/native provenance, 99.99%/recovery drills and production operations remain deferred. | Exercise provider-native pagination in hosted CI or retain documented fail-closed contracts with owner sign-off; review dependency/image provenance, authorization/isolation, encryption expectations, audit fields and abuse limits in the owning security/customer streams; retain secure Ozone auth/rotation and hosted artifact-access evidence where available | 3–8 d | Customer identity/tenancy model, secure Ozone endpoint/certificates, provider-native allocation, compliance requirements, hosted CI and scanning infrastructure |
| P11 — end-to-end client, mount and platform qualification | Native/provider CI / cross-workstream | HTTP path and all implemented bounded-listing contracts added to Ozone CI; full matrix required | 46% | W26 covers Rust/Node/CLI and the shipped HTTP server/client path through the Ozone composition gate with scoped cleanup; built-in Rust providers enforce the directory bound before response materialization; the unstorage/N-API path has a provider callback, public `readdirBounded` API, Node error-shape test and TypeScript declaration; parity fixtures cover capable overflow/success and legacy fail-closed behavior; SQLite/PGlite plus the TiDB/RustFS and FoundationDB/RustFS composition tests assert provider-backed bounded success and `EOVERFLOW`; the feature-built FoundationDB and TiDB Node/N-API Ozone lanes now assert the same contract with scoped cleanup prefixes. TiDB compile/Clippy and FoundationDB check evidence pass locally, but live provider/Ozone execution is hosted-only. Native mount, providers without the callback, `MOUNTX_SOURCE` parity and every platform are not yet accepted | Review terminal Ozone evidence for HTTP and every bounded-listing marker and cleanup, add/retain bounded provider rows for key-value/JavaScript-owned metadata, run mountx parity and then exercise every advertised native/mount surface; retain platform/provider matrices, restart/recovery and negative capability evidence | 5–15 d depending on advertised platforms | macOS/Linux/Windows runners, privileged mount facilities, native workstreams, provider callback contracts, mountx source, signing and provider connectivity |
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
- [x] Add one-revision W26 Ozone evidence-packet aggregation. The CI lanes now
  retain policy/base/provider logs and provider IOPS JSON with fail-closed
  artifact uploads; `w26-ozone-evidence` downloads all four packets and
  verifies exact provider sets, matching clean source revisions, policy
  positive/negative markers, provider acceptance, Ozone fault/integration and
  cleanup markers. Local synthetic packet tests and workflow checks pass; the
  aggregate hosted job remains pending on the current revision.
- [x] Make the IOPS artifact verifier reject incomplete performance evidence.
  It now requires the fixed payload-size mapping, 100% lifecycle success,
  finite elapsed/operation statistics, zero timeout and cleanup failures, and
  exact operation/statistic sample counts. Local positive and negative
  artifact tests pass; hosted provider performance and customer capacity
  remain open.

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
  supported Ozone-backed metadata provider, has dedicated durable TiDB and
  FoundationDB invocations with hard thresholds and retained artifacts, and
  records absent provider prerequisites as explicit skips; terminal hosted
  artifacts and pass markers are still open.
- [ ] P9/P12/P13: hand compatibility, CI evidence, customer incident scenarios
  and release inputs to the owning streams.
- [ ] P11: run every advertised Rust/Node/CLI/HTTP/native surface end to end
  through Ozone, retaining cross-workstream native blockers.
- [ ] P14: audit one retained CI revision and record W26 integration-ready or
  NO-GO before another stream promotes a release.

## Provisional remaining effort and blockers

| Category | Estimate | Notes |
| --- | --- | --- |
| Hosted result inspection | 0.5–1.5 d engineering plus external queue time | The historical W26 packet is green on `35585066458`; the latest published tip `12ba117` has CI `35628709359` pending, Fault injection `35628709207` queued, W08 release targets `35628709298` pending and W08 release policy `35628709428` pending; unrelated W04 production policy `35628709324` succeeded and no Live Cloudflare R2 or Live AWS S3 run was recorded. Dedicated TiDB/FoundationDB IOPS artifacts, verifier PASS markers, strict no-skip evidence and the production-config policy output still require a terminal retained W26 run. |
| Strict IOPS qualification integrity | 1–1.5 h engineering plus 0.75–1.5 h security/hosted review | The benchmark now fails closed on missing requested providers and the generic Ozone lane no longer labels skipped TiDB/FoundationDB rows as a pass. Terminal artifacts and live provider thresholds remain unmeasured. |
| IOPS artifact/profile evidence gate | 1.5–2.5 h engineering/tests plus 0.75–1.25 h security/hosted review | Generic, TiDB and FoundationDB wrappers reject targets below 1,000 or weakened 4 KiB/400/concurrency-64 settings and validate the retained JSON before pass markers. Hosted artifacts, provider performance and customer capacity remain open. |
| IOPS artifact retention gate | 0.25–0.5 h engineering/local verification plus 0.5–1 h security/hosted review | The three W26 IOPS uploads now use `if-no-files-found: error`; hosted artifact service and one-revision retained-run review remain open. |
| Credential-free production-config policy | 1.5–2.5 h implementation plus 0.5–1.5 h security/handoff review | Local all-provider positive fixtures and the insecure negative fixture pass the offline gate. Hosted CI, customer endpoint/IAM/TLS/rotation and runtime readback are separate gates. |
| Durable TiDB/Ozone remediation | 0 h for W26 acceptance | The restart isolation fix is validated by the Ozone-backed durable acceptance marker. Separate production/native/provider expansion would be a new scope. |
| Tracker/ledger publication | 0.5–1 h for this chunk | Includes focused security-diff review, current-head CI inspection, concurrent `origin/main` reconciliation, `git diff --check`, commit, push and remote verification. |
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
| 2026-09-22 — Ozone provider-backed bounded listing qualification | Added real Ozone composition assertions for bounded directory success and `EOVERFLOW` across the SQLite/R2 and PGlite/R2 `ChunkedFs` lanes, plus the standalone harness lockfile entry required by the merged R2 dependency graph. | ~0.75–1 h | 0 h local service execution; hosted Ozone composition and provider startup remain external | Locked Ozone harness compile, test discovery, strict all-target Clippy, formatting, shell syntax and diff checks passed. The ignored live tests are visible and ready, but Docker/Ozone was not available locally, so no live provider pass is claimed. |
| 2026-09-22 — Exact bounded-provider limit correction | Corrected the KV adapter to pass the caller's exact `maxEntries` to the provider (the provider may return one extra key as its overflow signal) and added a regression that records limits `[2, 3]`. | ~0.5–0.75 h | 0 h local; hosted CI runs `35611505333`/`35611505169`/`35611505545` are non-terminal | KV integration tests 13/13, strict affected-package Clippy, formatting and diff checks passed. The fix is published on `73cdc93`; no hosted result is promoted. |
| 2026-09-22 — Current-tip security review and remote reconciliation | Fast-forwarded concurrent `origin/main` changes through `3fca802`, reviewed the SDK credential-redaction and FoundationDB workflow deltas, and completed the current-revision standard security scan. | ~1.25–1.75 h | ~0.25 h remote refresh; hosted CI runs `35612976567`/`35612976712`/`35612976618`/`35612976513` are non-terminal | Scan `70af8d4e-9fb3-4d2c-b688-54a9b6535739` completed with zero reportable local findings across 16 surfaces. It is an intentionally partial production packet: customer Ozone TLS/IAM/rotation, provider-native bounded allocation, dependency provenance, 99.99%/recovery drills and operations remain deferred. |
| 2026-09-22 — Durable-provider bounded-listing extension | Added provider-backed bounded success/overflow assertions to the TiDB/RustFS and FoundationDB/RustFS composition seed/reopen paths, then compiled and linted the affected targets. | ~1.5–2 h | ~0.5 h provider/native compile gate; no live services available locally | Commit `d1c9e44`, published at `44b01a7`: TiDB locked test compilation and strict Clippy passed; FoundationDB `cargo check --tests` passed, while native test-binary linking is blocked by missing `libfdb_c`. No hosted Ozone/provider marker is promoted. |
| 2026-09-22 — FoundationDB Ozone Node/N-API bounded surface | Added the feature-built Node/N-API bounded success/overflow assertions, made the test prefix inherit the owned Ozone scope, and enabled the Node lane in the dedicated durable Ozone workflow. | ~0.75–1 h | ~0.25 h hosted workflow startup; feature-enabled native execution remains hosted | Commit `b80c19c`, published at `0842474`: Node syntax and shell syntax checks passed; the local non-feature invocation skipped safely. The hosted job must emit `FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS` for seed/reopen plus restart and cleanup markers before this provider surface is accepted. |
| 2026-09-22 — TiDB Ozone Node/N-API bounded surface | Added the feature-built public Node/N-API TiDB seed/reopen test, bounded success/overflow markers, Ozone-scoped block prefix and the dedicated workflow build/invocation. | ~1–1.5 h | ~0.25 h hosted workflow startup; TiDB/N-API/Ozone execution remains hosted | Commit `ef6a876`, published at `43f8df2`: shell, JavaScript, YAML and diff checks passed; the local feature-disabled invocation skipped safely. The hosted job must emit `TIDB_NAPI_BOUNDED_READDIR_PASS` for seed/reopen plus the durable restart, Ozone integration and cleanup markers before the extension is accepted. |
| 2026-09-22 — Published-tip security scan and CI reconciliation | Ran the standard security scan against the published durable-provider revision and reconciled the current-head hosted workflow states. | ~1.25–1.75 h | ~0.5–1 h scan/hosted status wait | Scan `5ad61e60-20e3-4223-885a-d4b516d49bb1` completed with zero reportable findings across 16 W26-relevant surfaces, but semantic coverage is explicitly partial and independent workers did not return within bounded waits. Current `44b01a7` CI/Fault/W08 runs are canceled and Live Cloudflare R2 failed; none is evidence. |
| 2026-09-22 — Durable-provider Ozone IOPS qualification wiring | Added early positive-integer validation, direct TiDB Node benchmark execution, FoundationDB container-side benchmark execution with a writable owned artifact volume, retained JSON copy-out, provider-specific pass markers and always-upload workflow steps for dedicated durable-provider Ozone jobs. | ~1.5–2.25 h | ~0.25 h remote reconciliation; hosted provider startup and terminal artifacts pending | Code commit `de9d267`, published in reconciled tip `414a469`. Shell syntax, benchmark syntax/unit tests, YAML parse and diff checks passed. The current tip's CI run `35619958098` was canceled before jobs started and Live Cloudflare R2 run `35619958070` failed; no performance result is promoted. |
| 2026-09-22 — Focused security-diff review of IOPS harness | Reviewed the durable-provider IOPS shell/workflow surfaces for command injection, credential exposure, path escape and false pass markers; completed the prompt-driven diff scan. | ~0.75–1.25 h | 0 h local; hosted/customer control validation deferred | Scan `5fc5a943-07bb-4979-9f37-efd87a7f505e` completed with zero reportable findings. Coverage is explicitly partial: hosted credential/isolation, provider TLS/IAM/rotation, retained artifact access and live provider execution remain open gates. |
| 2026-09-22 — Credential-free Ozone production-config policy | Added `scripts/verify-w26-ozone-production-config.mjs` and SQLite/PGlite/TiDB/FoundationDB positive fixtures plus an insecure HTTP negative fixture; wired the policy into the `ozone` CI job without provider connections or credentials. | ~1.5–2.5 h | ~0.25 h remote reconciliation; hosted CI and customer security evidence pending | Local `node --check`, all four positive markers, negative rejection, YAML parse and diff checks passed. Commit `b8d8fb1` was merged with concurrent mainline changes and published at `4d8b5fd`; current CI `35622054417` is canceled and Live R2 `35622054352` failed, so no hosted result is promoted. |
| 2026-09-22 — Focused security-diff review of production-config policy | Reviewed parser/resource safety, inline-secret rejection, HTTPS/TiDB TLS enforcement, provider allowlists, CI secret exposure and the negative fixture; completed the prompt-driven diff scan. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `d74e3e86-2e0a-45cf-9819-e31f428eb5d4` completed with zero reportable findings. Coverage is explicit about external Ozone IAM/TLS/rotation, provider-native security, hosted execution and operational SLO/recovery gates. |
| 2026-09-22 — Expanded Ozone security negative paths | Added independent inline-secret, FoundationDB lease-authority and TiDB TLS-verification negative fixtures and explicit CI rejection assertions. | ~0.75–1.25 h | ~0.25 h remote reconciliation; hosted CI pending | Commit `7018b59` was merged with concurrent mainline changes and published at `6a1b94b`; four positive and four negative local policy checks, YAML parsing, syntax and diff checks passed. |
| 2026-09-22 — Focused security-diff review of expanded policy | Reviewed the expanded workflow rejection assertions, fixture credential-like values and environment-only TiDB TLS override; completed the prompt-driven diff scan. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `7addeeb5-4601-4951-aca9-becffb9bd4b9` completed with zero reportable findings. Changed-fixture source inventory was empty because only workflow/JSON changed, while all changed artifacts were manually inspected; hosted/provider coverage remains deferred. |
| 2026-09-22 — Strict provider-configuration IOPS qualification | Added `--require-configured`, provider-scoped generic Ozone IOPS selection and strict TiDB/FoundationDB invocations so a skipped requested provider cannot produce a qualification pass. | ~1–1.5 h | ~0.25 h remote reconciliation; hosted provider execution pending | Commit `d46e091` was merged with concurrent mainline changes and published at `74fe4c7`. Benchmark unit tests, missing-provider regression, Node/shell syntax, YAML parsing and diff checks passed locally. |
| 2026-09-22 — Focused security-diff review of strict IOPS qualification | Reviewed strict status aggregation, missing-configuration diagnostics, provider-list environment flow, shell command construction, artifact paths and the workflow environment. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `60269206-bb22-4b78-aaf7-f05d16ffcca0` completed with zero reportable findings across five source surfaces; changed workflow/documentation files were also manually reviewed. Hosted runner isolation, provider TLS/IAM/rotation and live performance remain deferred. |
| 2026-09-22 — IOPS artifact/profile integrity gate | Added the credential-free artifact verifier, fixed 4 KiB/400-iteration/concurrency-64 qualification profile, target floor, exact provider-set check, cleanup/lifecycle assertions and wrapper validation before pass markers. | ~1.5–2.5 h | ~0.25 h remote reconciliation; hosted provider execution pending | Commit `66f3670` was merged with concurrent mainline changes and published at `00d2b80`. Benchmark unit tests, verifier negative cases, Node/shell syntax and diff checks passed locally; no live Ozone/provider result is claimed. |
| 2026-09-22 — Focused security-diff review of IOPS artifact/profile gate | Reviewed untrusted JSON handling, regular-file/symlink policy, exact provider/profile checks, shell environment flow, output paths, marker ordering and secret exposure. | ~0.75–1.25 h | 0 h hosted; customer/provider controls deferred | Scan `1d97028f-e153-4b49-9fac-c3a8c1fc1117` completed with zero reportable findings across five changed source surfaces. Hosted artifact access, provider TLS/IAM/rotation, customer capacity and SLO/recovery controls remain deferred. |
| 2026-09-22 — Fail-closed W26 IOPS artifact retention | Changed the generic, TiDB and FoundationDB Ozone IOPS upload steps to fail when an expected JSON artifact is absent. | ~0.25–0.5 h | ~0.25 h remote reconciliation; hosted run pending | Commit `c0f8370` was merged with concurrent mainline changes and published at `12ba117`. CI YAML parsing, shell syntax, benchmark unit tests and diff checks passed locally; no hosted artifact result is promoted. |
| 2026-09-22 — Focused security-diff review of artifact retention | Reviewed workflow path scope, upload behavior, secret exposure, evidence-integrity bypasses and interaction with the `always()` upload steps. | ~0.5–1 h | 0 h hosted; artifact service/customer controls deferred | Scan `18010cad-ed69-4da3-b0a9-57163091e878` completed with zero reportable findings. Hosted artifact authorization/retention and provider/customer controls remain deferred. |
| 2026-09-22 — Mainline reconciliation after security-ledger publication | Merged concurrent origin/main changes after publishing the security chunk and refreshed the exact hosted workflow state for the merged revision. | ~0.25 h | ~0.25 h remote fetch/merge and hosted status lookup | Merge tip `9b3b90b` is verified on `origin/main`; CI `35624670863` and W08 release runs `35624670789`/`35624670708` are pending, while Fault injection `35624670707` is queued. No current-tip W26 result is promoted. |
| 2026-09-22 — Concurrent FUSE/mainline reconciliation | Merged the next concurrent mainline update after the ledger publication and refreshed hosted state again so the ledger remains tied to the actual remote tip. | ~0.25 h | ~0.25 h remote fetch/merge and hosted status lookup | Merge tip `3681f27` is verified on `origin/main`; CI `35624846453` and W08 release runs `35624846413`/`35624846541` are pending, while Fault injection `35624846354` is in progress. No current-tip W26 result is promoted. |
| 2026-09-22 — One-revision W26 Ozone evidence packet | Added the aggregate packet verifier, retained policy/base/provider logs, provider JSON artifact paths, explicit negative-policy markers and the fail-closed `w26-ozone-evidence` CI job. | ~2–3 h | ~0.5 h remote reconciliation; current CI `35630094815` pending and Fault injection `35630094834` in progress | Local benchmark/evidence unit tests, Node/shell syntax, YAML parse and diff checks passed. Code commit `08f4530` was merged with concurrent mainline changes and published at `097ed00`; the aggregate hosted packet is not yet terminal evidence. |
| 2026-09-22 — Focused security-diff review of evidence packet | Reviewed retained-log handling, artifact substitution/cross-revision checks, marker aggregation, shell pipelines and secret exposure. | ~0.75–1.25 h | 0 h hosted; provider/customer controls deferred | Scan `c67ae8e0-99af-4ade-8284-a612d599b5e4` completed with zero reportable findings across the changed verifier/test surfaces. Hosted runner/artifact authorization, provider TLS/IAM and customer Ozone controls remain external. |
| 2026-09-22 — W26 ledger publication for evidence packet | Refreshed the current remote/hosted status, updated W26.11, P8/P10 percentages, production NO-GO boundaries, remaining actions, estimates and session log. | ~0.5–1 h | ~0.25 h remote fetch/merge/push | Documentation is being published as a separate chunk after the implementation commit; no hosted result is promoted from pending/in-progress state. |
| 2026-09-22 — IOPS metric and payload-map integrity | Extended the W26 artifact verifier and synthetic tests to reject incomplete lifecycle counters, non-finite percentiles, timeout/cleanup failures and payload-size mismatches. | ~0.75–1.25 h | ~0.25 h remote reconciliation; hosted provider result pending | Commit `e875ba6` was merged with concurrent mainline changes and published at `1775895`. Benchmark/evidence unit tests, Node/shell syntax, YAML parse and diff checks passed; no hosted performance result is promoted. |
| 2026-09-22 — Focused security-diff review of metric integrity | Reviewed untrusted numeric fields, false pass conditions, sample-count consistency and artifact-content handling. | ~0.75–1.25 h | 0 h hosted; provider/customer controls deferred | Scan `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564` completed with zero reportable findings across the verifier/test surfaces. |

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

The Ozone provider-backed bounded-listing qualification was committed as
`e2ea888`, merged with concurrent mainline changes, and published at `ec58ba5`;
the published revision was verified to match `origin/main`. The SQLite/R2 and
PGlite/R2 ignored composition tests now assert bounded success and
`EOVERFLOW`. Current CI run `35610948086` and Fault injection run `35610947492`
are queued, while Live Cloudflare R2 run `35610947637` is in progress; none is
promoted to W26 evidence yet.

The exact bounded-provider limit correction was committed as `73cdc93` and
verified on `origin/main`. The KV adapter now passes the caller's exact limit
to `get_keys_bounded`, and the 13-test suite records the expected provider
limits. Concurrent W08/W25 and SDK/FoundationDB changes were then
fast-forwarded through `3fca802`; the current CI attempt has CI run
`35612976567` pending, Fault injection `35612976712` queued, Live Cloudflare
R2 `35612976618` queued and Live AWS S3 `35612976513` queued. None is promoted
to W26 evidence.

The durable-provider IOPS qualification chunk was committed as `de9d267` and
published after reconciling concurrent `origin/main` changes at `414a469`.
`scripts/test-tidb.sh` now runs the public Node split-TiDB/R2 benchmark with
the hard `--min-iops 1000` target and retained JSON output. The FoundationDB
harness runs the same benchmark inside the feature-enabled client container,
uses its owned writable `/fdb` volume for the result, copies the JSON to the
workflow artifact path and emits `FOUNDATIONDB_OZONE_IOPS_PASS` only after the
provider run and artifact complete. Both dedicated CI jobs retain their JSON
artifacts even on failure. Positive-integer validation, shell syntax, runner
syntax/unit tests, YAML parsing and diff checks passed locally. Current tip
run `35619958098` was canceled before jobs started and Live Cloudflare R2 run
`35619958070` failed; neither is evidence.

Focused security-diff scan `5fc5a943-07bb-4979-9f37-efd87a7f505e` covered the
W26 IOPS shell/workflow surfaces and found zero reportable findings. Its
coverage is intentionally partial: hosted credential/isolation, provider
TLS/IAM/rotation, retained artifact access and live TiDB/FoundationDB/Ozone
execution remain deferred production gates.

The current-revision standard security scan
`70af8d4e-9fb3-4d2c-b688-54a9b6535739` targeted `3fca802` and completed with
zero reportable local findings across 16 surfaces. Its coverage is explicitly
partial: customer Ozone TLS/IAM/certificate rotation, provider-native bounded
allocation, dependency/platform provenance, 99.99%/recovery drills and
production operations remain deferred. This closes the local scan action, not
the production security gate.

The durable-provider bounded-listing extension was committed as `d1c9e44` and
published with concurrent mainline changes at `44b01a7`; the published revision
was verified to match `origin/main`. TiDB's locked chunked test target and
strict Clippy passed; FoundationDB `cargo check --tests` passed, while local
test-binary linking is blocked by missing `libfdb_c`. The new live TiDB,
FoundationDB and Ozone markers remain hosted gates, so no provider pass is
promoted from the local compile evidence.

The current published-tip standard security scan
`5ad61e60-20e3-4223-885a-d4b516d49bb1` targeted `44b01a7` and completed with
zero reportable findings across 16 W26-relevant surfaces. Semantic coverage is
explicitly partial: the parent fallback was used after independently launched
workers did not return within bounded waits. Customer Ozone TLS/IAM/rotation,
provider-native allocation, dependency/native provenance, 99.99%/recovery and
operations remain open. The `44b01a7` hosted workflows (CI, fault injection,
W08 policy and Live Cloudflare R2) are canceled or failed and are not evidence.

The FoundationDB Ozone Node/N-API bounded-listing extension was committed as
`b80c19c`, merged with concurrent mainline changes, and published at `0842474`;
the published revision was verified to match `origin/main`. The dedicated
Ozone FoundationDB job now builds the feature-enabled addon in the pinned
client image, runs the Node seed/reopen bounded success/overflow checks, and
keeps its R2 prefix under the owned Ozone cleanup scope. Local syntax checks
passed and the non-feature invocation skipped safely; hosted markers remain
pending and are not promoted to acceptance.

The TiDB Ozone Node/N-API bounded-listing extension was committed as
`ef6a876`, merged with concurrent mainline changes, and published at `43f8df2`;
the published revision was verified to match `origin/main`. The Ozone TiDB job
now installs and builds the public addon, runs the provider-backed Node
seed/reopen bounded success/overflow test, and scopes its R2 prefix under the
owned Ozone run. Shell, JavaScript, YAML and diff checks passed locally; the
hosted markers remain pending and are not promoted to acceptance.

The credential-free Ozone production-config policy chunk was committed as
`b8d8fb1`, merged with concurrent mainline changes, and published at `4d8b5fd`;
the expanded negative-path chunk `7018b59` was subsequently merged with
concurrent mainline changes and published at `6a1b94b`. The published revision
was verified to match `origin/main`. The policy script and fixtures cover
SQLite, PGlite, TiDB and FoundationDB metadata, durable settings, HTTPS R2
blocks, scoped prefixes, exact external secret references and TiDB TLS options
without contacting a provider. The expansion independently rejects inline
credentials, unsafe FoundationDB lease authority and TiDB TLS verification
downgrades. Local four-positive/four-negative executions, Node syntax, YAML
parsing and diff checks passed. Focused scans `d74e3e86-2e0a-45cf-9819-
e31f428eb5d4` and `7addeeb5-4601-4951-aca9-becffb9bd4b9` completed with zero
reportable findings; hosted/customer controls remain partial/deferred. Latest
tip CI `35623755790` is queued, W08 release targets `35623755638` are pending,
W08 release policy `35623755613` is in progress and Live Cloudflare R2
`35623755595` failed, so no current-tip result is promoted.

The strict provider-configuration IOPS chunk was committed as `d46e091`,
merged with concurrent mainline changes, and published at `74fe4c7`; the
published revision was verified to match `origin/main`. The storage benchmark
now fails in qualification mode when any requested provider is skipped, and
the generic Ozone composition lane explicitly qualifies only SQLite/R2 and
PGlite/R2 while dedicated TiDB/FoundationDB lanes remain strict. Local unit,
missing-provider, syntax, YAML and diff checks passed. Focused scan
`60269206-bb22-4b78-aaf7-f05d16ffcca0` completed with zero reportable findings
across five source surfaces; hosted/provider coverage remains partial. Latest
tip CI `35628078285` is pending, Fault injection `35628078322` is queued, W08
release targets `35628078265` and policy `35628078261` are pending, W04
production policy `35628078220` is queued and Live Cloudflare R2 `35628078323`
is queued; none is W26 acceptance evidence.

The IOPS artifact/profile integrity chunk was committed as `66f3670`, merged
with concurrent mainline changes, and published at `00d2b80`. The generic,
TiDB and FoundationDB wrappers now reject a target below 1,000 or a weakened
4 KiB/400-iteration/concurrency-64 profile, then validate the exact requested
provider set, no skipped/configuration-failed rows, successful cleanup and
per-size lifecycle success in the retained JSON before emitting a pass marker.
Local benchmark unit tests, verifier negative cases, Node/shell syntax and
diff checks passed. Focused scan `1d97028f-e153-4b49-9fac-c3a8c1fc1117`
completed with zero reportable findings across five changed source surfaces;
hosted/provider/customer controls remain partial or deferred.

The fail-closed IOPS artifact-retention chunk was committed as `c0f8370`,
merged with concurrent mainline changes, and published at `12ba117`. The
generic, TiDB and FoundationDB JSON uploads now use
`if-no-files-found: error`, so a missing expected artifact fails the CI
evidence job. CI YAML parsing, shell syntax, benchmark unit tests and diff
checks passed. Focused scan `18010cad-ed69-4da3-b0a9-57163091e878` completed
with zero reportable findings; hosted artifact authorization/retention and
provider/customer controls remain deferred. Latest tip CI `35628709359` is
pending, Fault injection `35628709207` is queued, W08 release targets
`35628709298` and policy `35628709428` are pending, W04 production policy
`35628709324` succeeded but is unrelated, and no Live Cloudflare R2 result was
recorded; none is W26 acceptance evidence.

The documentation chunk `3741b29` was merged with concurrent mainline changes
and published at `fd670b1`; it was superseded by the expanded security chunk
published at `6a1b94b`, then reconciled with concurrent mainline changes and
published at `9b3b90b`, followed by the concurrent FUSE/mainline update
published at `3681f27`, the strict qualification publication at `74fe4c7`,
the concurrent reconciliation at `80344aa`, the later reconciliations at
`3552344` and `75b900f`, the IOPS-verifier publication at `00d2b80`, and the
latest fail-closed artifact-retention publication at `12ba117`. Its CI run
`35628709359`, W08 release targets `35628709298`, W08 release policy
`35628709428` and Fault injection `35628709207` are pending or queued; W04
production policy `35628709324` succeeded but is unrelated, and these states
are not W26 acceptance evidence.

The one-revision W26 Ozone evidence-packet chunk was committed as `08f4530`
and published after reconciling concurrent `origin/main` changes at `097ed00`.
It retains the policy/base/provider logs and generic, TiDB and FoundationDB
IOPS JSON artifacts, adds explicit negative-policy pass markers, and makes the
`w26-ozone-evidence` aggregate job fail closed when any artifact, source
revision, provider marker, integration/recovery marker or cleanup marker is
missing. Local benchmark/evidence unit tests, Node/shell syntax, YAML parsing
and diff checks passed. Security scan
`c67ae8e0-99af-4ade-8284-a612d599b5e4` completed with zero reportable findings;
hosted/provider/customer coverage remains explicitly deferred. The subsequent
concurrent mainline merge published `f3aae7d`; its CI run `35630613146` was
canceled before jobs started, Fault injection `35630613141` is pending, W04
production policy `35630613331`, W08 release targets `35630613180` and W08
release policy `35630613275` are queued or pending, and no current-tip W26
result is promoted.

The W26 IOPS metric/payload integrity chunk was committed as `e875ba6` and
published after concurrent mainline reconciliation at `1775895`. The artifact
verifier now requires the fixed payload-size mapping, complete lifecycle and
statistic sample counts, 100% success, zero timeout/cleanup failures and finite
operation statistics before accepting a provider result. Local benchmark and
synthetic evidence tests, Node/shell syntax, YAML parsing and diff checks passed.
Security scan `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564` completed with zero
reportable findings; hosted provider performance and customer capacity remain
deferred, and no matching current-tip hosted W26 result was visible at the
publication snapshot.
