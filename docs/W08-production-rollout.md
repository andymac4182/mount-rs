# W08 TiDB production rollout

This document is the operational companion to
[`W08-progress-ledger.md`](W08-progress-ledger.md). It tracks the production
qualification of the W08 TiDB metadata and RustFS/S3 block composition after
the demo. A green demo, a local provider test, or a hosted qualification job
does not authorize a production rollout.

## Current decision

| Field | Status |
| --- | --- |
| Workstream | W08 — TiDB metadata with RustFS/S3-compatible immutable blocks |
| Functional qualification | Complete for the defined hosted scope: durable 3PD/3TiKV restart, provider fencing and ambiguous commit, live Linux TiDB/RustFS Node/CLI/FUSE, ARM Node, Ubuntu NFS and macOS native-NFS rows passed in retained terminal jobs |
| Production rollout | **NO-GO** |
| Provisional production baseline | **15%**; planning only, not a release-readiness measurement |
| Current implementation capability | TLS-capable provider, Rust SDK, CLI, N-API, guarded TLS and production-config policy verifiers, explicit replicated-durable P01 topology and external-secret-manager P02 policies, the W08 rollout-ledger consistency verifier/test, a fail-closed `--require-go` admission guard before protected production-candidate builds, a machine-readable nine-gate production-evidence packet/validator, artifact-manifest and locked-Cargo CycloneDX SBOM tooling wired into release policy, three-asset `SHA256SUMS` coverage, a dedicated non-cancelling hosted release-policy gate, a hosted Linux x86_64/macOS arm64 target-package/download/attestation matrix, protected production-candidate release admission, and bounded HTTP `/healthz`/`/readyz` probes are implemented; local unit/Clippy, CLI-schema, policy, tracking-control, evidence-shape, real-artifact, SBOM, asset-integrity, target-matrix, workflow-shape and hosted compile/guard checks are tracked separately |
| Primary reason | No approved production topology, credential/IAM policy, backup/restore drill, upgrade/rollback rehearsal, production collector/SLOs, capacity envelope, security sign-off, named on-call ownership, executed incident drills, canary or release-owner approval is recorded |
| Evidence rule | Every production result must name the revision, provider/image versions, topology, environment identity, test/run/job ID, terminal status, owner, cleanup result and rollback outcome |

The detailed ledger remains the source of truth for percentages, session time,
evidence boundaries and provisional estimates. This document is the source of
truth for the deployment contract and rollout sequence; the operator procedures
and timed-drill checklist are in
[`W08-operations-runbook.md`](W08-operations-runbook.md). Do not check a gate
from a queued, skipped, cancelled, credential-free, installation-only or
planning result.

## Production gate ledger

| Gate | Status | Required exit evidence |
| --- | --- | --- |
| P01 — deployment scope, topology and support matrix | Open — 25% | Approved managed/self-hosted TiDB/PD/TiKV and block-store topology, regions, HA/quorum, network/TLS policy, resource limits, supported versions, tenancy, IaC and a production-like staging smoke/restart result; the checked-in `verify-w08-production-topology.mjs` policy now requires a replicated-durable shape with 3 PD, 3 TiKV, 2 SQL frontends, majority quorum, pinned coherent versions, private TLS networking and the durable resource floor. This is repository shape control only. A fresh local single-node v8.5.7 smoke attempt entered PD startup but exited 125 on `Bad response from Docker engine`; the preceding durable attempt failed closed below the 10 GiB Docker floor. Neither produced `TIDB_ACCEPTANCE` or production evidence. |
| P02 — secrets, IAM, rotation and audit | Open — 20% | Secret-manager injection, least-privilege metadata/block identities, rotation and revocation without data loss, break-glass procedure, audit and redaction evidence; the checked-in P02 policy now requires an external secret manager, workload/managed identity, exactly the three production references, bounded rotation with overlap/revocation, redacted access auditing and a two-person break-glass procedure reference. This is shape control only; the existing production-config gate rejects inline secret strings and requires external env references. |
| P03 — backup, restore and disaster recovery | Open — 10% | Defined RPO/RTO and retention, encrypted backups/versioning, clean-environment restore, metadata/block consistency, corruption/partial-object handling and recovery sign-off; the checked-in `verify-w08-production-backup.mjs` policy now requires transactionally consistent TiDB metadata snapshots, immutable/versioned encrypted blocks, isolated restore with no production-writer access, corruption/partial-object/region-loss cases, bounded 60-minute RPO and 240-minute RTO, a second region and data-owner/release-owner sign-off references. This is repository shape control only. |
| P04 — upgrade, compatibility and rollback | Open — 10% | Rehearsed TiDB/RustFS/client version matrix, schema/config migration, rolling upgrade, interrupted-upgrade recovery, retained-data rollback and compatibility sign-off; the checked-in `verify-w08-production-upgrade.mjs` policy now requires pinned current/previous versions, all five consumer surfaces, expand-contract/versioned migration, forward/backward compatibility, interrupted recovery, rolling quorum preservation, retained rollback artifacts/config/data checks, writer fencing and fresh-client readback. This is repository shape control only. |
| P05 — observability, SLOs and alerting | Open — 15% gate weight; local/hosted HTTP contract passed | The HTTP transport's unauthenticated `/healthz` and `/readyz` contract, `Cache-Control: no-store` and `X-Content-Type-Options: nosniff` are locally tested and passed in terminal hosted job `106340034907`; the checked-in `verify-w08-production-observability.mjs` policy now requires private managed metrics, structured redacted logs, redacted traces, retention floors, 99.9% availability and 0.1% write-error SLO bounds, dual on-call routes, paging, test alert, 15-minute acknowledgement and no secret-bearing alert payloads. Exit still requires provider-aware checks where applicable, production collector, dashboards, live SLO/error-budget observation, paging, retention/redaction and an exercised alert route. |
| P06 — capacity, load and soak | Open — 20% | The bounded TiDB/RustFS soak harness is configured in hosted composition CI and passed its current seed/reopen qualification at 64 operations, concurrency 8 and 65,536-byte payloads. The checked-in `verify-w08-production-capacity.mjs` policy now requires baseline/peak/saturation/failover/soak profiles, write/read/truncate/reopen coverage, p95/p99/error/throughput thresholds, external CPU/memory/disk/network/IOPS collection, utilization/headroom/growth/cost limits, SQL frontend/TiKV/PD/RustFS failover with fresh-client integrity readback, a four-hour soak with reopen cycles and owned cleanup, and named workload/performance/cost/operations owners. Exit still requires representative production-shaped measurements, resource growth, failover, scaling, cost and sign-off evidence; this policy is repository shape control only. |
| P07 — security, transport and hardening | Open — 20% | TLS and certificate rotation, network segmentation, authz/tenant isolation, dependency/image/SBOM review, threat-model findings, audit checks and a credentialed TLS handshake; the checked-in `verify-w08-production-security.mjs` policy now requires TLS 1.3 with CA-chain/hostname validation, bounded rotation overlap/revocation, private segmented metadata/block/client networking with default-deny ingress/egress, workload identity, resource-and-prefix tenant isolation, least privilege, supply-chain/provenance scans, required threat-model status, immutable redacted audit events and external endpoint/CA references for a credentialed handshake. This is repository shape control only; the live endpoint, certificates, network controls, scanner results, audit sink and security sign-off remain external. |
| P08 — failure drills, runbooks and on-call | Open — 25% | The operator runbook and D01–D09 drill definitions are implemented and the credential-free `verify-w08-production-operations.mjs` policy requires mapped gate references, evidence-record completeness, incident controls, cleanup/rollback outcomes, 24x7 escalation routes and a maximum 15-minute acknowledgement. Exit still requires timed client/provider/lease/partition/partial-write/restart/restore drills, operator diagnosis and rollback steps, integrity checks, on-call tabletop, terminal redacted records and named acknowledgement; the policy is repository shape control only. |
| P09 — release provenance, canary and go/no-go | Open — 10%; verifier, artifact-wiring, dedicated real-artifact, unsigned-SBOM, all-asset-integrity, target-matrix, current-main manual-attestation and W08.31 qualification slices passed | `scripts/verify-w08-release-manifest.mjs` validates W08 source/repository identity, artifact SHA-256/size, `SHA256SUMS`, GitHub Actions workflow/run provenance and explicit signature/SBOM/canary states; `scripts/write-w08-release-manifest.mjs` derives those fields from actual artifact bytes. `scripts/write-w08-release-sbom.mjs` and `scripts/verify-w08-release-sbom.mjs` generate and validate a 288-component CycloneDX 1.5 dependency graph bound to the artifact and source commit. Both release workflows create `SHA256SUMS` after the manifest and unsigned SBOM, cover all three assets and verify every entry; the preview workflow then re-downloads and verifies them. The tag release invokes pinned GitHub `actions/attest@v4.2.2` for provenance and CycloneDX SBOM, verifies repository/signer-workflow/source-commit/tag identity with `gh attestation verify`, and finalizes the manifest's signature/SBOM controls; the target matrix exposes an explicit `workflow_dispatch` `attest=true` path. Dedicated `.github/workflows/w08-release-policy.yml` proves the Ubuntu artifact path. `.github/workflows/w08-release-targets.yml` builds/tests Linux x86_64 and macOS arm64, uploads each four-file asset set and verifies each downloaded set. Hosted runs `35624385556`, source `2ab3cf1`, `35627761501`, source `50a33ac` at dispatch, `35631063978`, source `cf75835`, and latest `35641555767`, source `2bbd0266`, reached terminal success for both target builds, both download-verification jobs and both target attestation jobs; the latest assets and attestations were independently reverified. The approved tag-triggered release has not run. Exit still requires that publication, target-platform package/SBOM checks in the release registry, staged canary with live SLO observation, rollback result and explicit release-owner approval. |

No P01–P09 item is terminally accepted. P01/P02/P07 implementation progress is
also covered locally by
`node scripts/verify-w08-production-config.mjs
tests/tidb/production-config-policy.json`, whose positive fixture passed and
whose insecure fixture failed closed; the public CLI schema accepted the
positive fixture. This policy check does not connect to TiDB or object
storage. The later hosted `tidb-tls-compile` job `106311076905` in run
`35592902494` also passed the positive/negative production-config policy
check, alongside TLS compilation and URL guardrails. These jobs remain
compile/policy evidence only and do not close the live provider, IAM,
certificate, or production-deployment gates.

W08.28 adds `.github/workflows/w08-production-release.yml` as a protected
production-candidate path. A tag matching `v*-cli-production-candidate*` builds
and verifies Linux x86_64 and macOS arm64 assets, generates target-specific
manifests and SBOMs, verifies GitHub Sigstore provenance/SBOM attestations, and
holds prerelease publication behind the `w08-production` environment. The
environment must be configured with required reviewers and tag/branch policy;
no candidate tag or environment approval has been executed here, and this
workflow does not replace the canary, rollback or explicit production GO gates.
The implementation was published as `dff55858` in merge tip `6abc0535` on
`origin/main` after concurrent mainline reconciliation.

The live read-only production-boundary audit on **2026-09-22 07:35 AEST**
confirms that this P09 path is externally blocked: the GitHub `w08-production`
environment and its environment-secret surface both return HTTP 404,
`gh release list` shows only the `v0.1.0-cli-preview` prerelease,
`git ls-remote` finds no `v*-cli-production-candidate*` tag, and
`gh run list --workflow w08-production-release.yml` returns no production
workflow runs. The protected workflow file does exist on the remote mainline
(blob `9cacf0f19c8fc475df508684cf0ffcc591c96625`), so the current gap is
hosted environment/tag/execution configuration rather than missing repository
code. No release, canary, rollback or approval claim is inferred.

W08.29 then qualified the latest published-main target path in hosted run
`35638433010`, source `9d3a6e502eccec9ba54c00e80c98e6e1da175177`. Both Linux
x86_64 and macOS arm64 builds, downloaded bundles, provenance attestations,
CycloneDX SBOM attestations and final identity verifiers passed; the two target
bundles were independently rechecked after download. This remains target
qualification, not a production-candidate tag release, registry acceptance,
canary, rollback or GO decision. The evidence capture was committed as
`ad45bf06` and published in merge tip
`d68f14de3c13c0510237e9a88c615c1a8e74c9a3`.

W08.31 requalified the same target path in hosted run `35641555767`, source
`2bbd0266a094b06bf97d51baafe0d3a8800bfec5`. Linux x86_64 build job
`106471833712`, macOS arm64 build job `106471833967`, downloaded-asset jobs
`106473358964` and `106473359006`, and attestation jobs `106475107803` and
`106475108148` all passed. The independently downloaded Linux tarball is
`6a3de0a607ffafcedf6bd3385c3849a30208df0345120061061821109a09ac79`
(8,373,376 bytes); the macOS arm64 tarball is
`75a031c4e439ede07f0fa1a09db050b15c45f6d802a703d90b67cde52b4a941d`
(6,958,388 bytes). Local checksum, manifest, 288-component SBOM, archive and
macOS runtime checks passed, and independent SLSA/CycloneDX attestation
verification passed with exact repository, workflow, source, `main` ref and
non-self-hosted-runner constraints. This is a fresh hosted qualification
result only; it does not execute the protected candidate-tag publication,
registry acceptance, canary, rollback or release-owner approval.

W08.32 adds the fail-closed repository consistency control for the production
tracking documents. Hosted run `35647096385`, source
`2641962a6a65179abf4b8d785345fbe6af4be9b8`, job `106490142559`, completed
successfully: the current NO-GO state, all 32 implementation items, nine
production-gate rows and six simulated invalid/GO transitions passed the
rollout-ledger verifier/test, and the existing release-identity/provenance
policy step also passed. The earlier run `35646848615` was cancelled before
job creation (`jobs=[]`) and is not evidence. This is hosted tracking-control
qualification only; it does not close P01–P09 or establish candidate release,
registry, canary, rollback or release-owner approval.

W08.33 puts `node scripts/verify-w08-rollout-ledger.mjs --require-go` in a
required `admission` job before the protected production-candidate build
matrix. The current repository decision is NO-GO, so the guard must refuse a
candidate before artifact generation; its simulated complete-GO case is covered
by the local transition suite. This is a repository release-safety control only:
it does not supply the missing topology, provider, secret, registry, canary,
rollback or release-owner evidence.
The post-publication push run `35648755697` (source `86a87ed0`) and the
credential-free manual dispatch `35648898876` (resolved source `987c593b`) both
cancelled before creating jobs, so neither is hosted admission evidence.

W08.34 adds `docs/W08-production-evidence.json` and a validator that cross-checks
its decision with this rollout document. The current packet deliberately has
all nine gates open, explicit remaining actions and no evidence records. A GO
admission requires every gate to be closed and every evidence record to name a
full source revision, provider versions, topology, environment, terminal
test/run, owner, cleanup outcome, rollback outcome and evidence reference.
This is completeness/safety validation only; it cannot authenticate the
underlying provider or owner claims.
The first successful policy run after the change, `35650533691` at source
`95437f67`, predates W08.34 and therefore does not qualify this validator. The
current-tip run `35650626028` at source
`8f3a19a891b8d432ff551c04789921575bb12f4f` cancelled before creating jobs
(`jobs=[]`). A subsequent current-tip run `35651363875` at source
`14f8c344a5a7f5b2e8cb08475db3d87ecbfc23d7`, job `106504528377`, completed
successfully in 2m41s; the W08 policy step, including the evidence-packet
validator/test, and the existing release-identity/provenance policy passed.
This is hosted implementation/static qualification only; it does not close
P01–P09 or supply production provider, canary, rollback or approval evidence.

W08.35 hardens the real TiDB harness preflight: architecture, CPU and memory
are read from one formatted Docker server-info response before any isolated
network, volume or container is created. If the daemon becomes unavailable
while the CLI is rendering server fields, the harness now exits with status 2
and a clear prerequisite-boundary message, without emitting `TIDB_ACCEPTANCE`
or attempting partial topology setup. This is harness safety evidence only; it
does not close P01 or promote local Docker capacity into production evidence.

W08.36 hardens GO-packet completeness by rejecting placeholder-shaped values
such as `TBD`, `pending`, `unknown`, `TODO`, template markers and
angle-bracket substitutions in provider versions, topology, environment, run,
owner, cleanup, rollback and evidence-reference fields. The synthetic
regression case fails closed, while the checked-in packet remains NO-GO with
zero evidence records. This is evidence-shape protection only; it cannot
authenticate a provider result or create production approval.

W08.37 adds the credential-free P01 topology contract in
`tests/tidb/production-topology-policy.json`, enforced by
`scripts/verify-w08-production-topology.mjs` and its eight-case regression
suite. The policy accepts only an explicitly `replicated-durable` topology:
three PD members, three TiKV members, two SQL frontends, majority quorum,
coherent pinned TiDB component versions, private TLS-enabled networking, the
durable resource floor and tenant-isolation support metadata. The TiDB
harness's `single-node-smoke-not-replicated-acceptance` result remains a
separate smoke boundary and is deliberately rejected by this production-like
topology policy. The checks are wired into the dedicated W08 release-policy
workflow and protected candidate admission. This closes a repository
implementation/control slice of P01 only; it does not prove that the selected
staging or production topology exists, has quorum, meets capacity, or has
provider/owner approval. P01 therefore remains open.

The W08.37 implementation commit `263be2f4` was reconciled with concurrent
mainline changes and published in exact public merge `fab2a0cb`; later
source-equivalent CI/documentation updates place the public tip at
`232443e1`. Shared-wrapper workspace check and strict Clippy passed on the
merged implementation tree. Hosted W08 policy run `35709828967`, exact head
`fab2a0cb`, job `106688181078`, completed successfully in about 2m45s,
including the topology-control, rollout/evidence and release-policy artifact
checks; this is hosted implementation/static evidence only. The read-only
production boundary remained unchanged: no production-release workflow runs,
HTTP 404 for `w08-production`, preview-only release and no
production-candidate tag. Production remains NO-GO.

W08.38 adds the credential-free P02 secret/IAM contract in
`tests/tidb/production-secrets-policy.json`, enforced by
`scripts/verify-w08-production-secrets.mjs` and its eight-case regression
suite. The policy requires an external secret manager, workload or managed
identity, exactly the three production references (`MOUNT_RS_TIDB_TLS_URL`,
`R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY`), bounded rotation with overlap
and revocation-on-failure, redacted access auditing with retention, and a
two-person break-glass procedure reference. Inline values, static identity,
missing/duplicate references, unsafe rotation and unredacted audit fixtures
fail closed. The checks are wired into both W08 release workflows. This is a
repository implementation/control slice of P02 only; it does not prove a
secret-manager binding, IAM grant, rotation event, audit record or production
approval. The implementation was published as `911157ee` in reconciled public
merge `e86c9ccbd394df3b8ec23551691c199e1753ede2`. The local eight-case P02
suite, shared-wrapper Cargo check, strict Clippy, rollout/evidence controls and
workflow-shape checks passed on that merge. Hosted W08 release-policy run
`35713249813`, job `106698749633`, exact head
`e86c9ccbd394df3b8ec23551691c199e1753ede2`, completed successfully in 2m49s;
this is hosted implementation/static evidence only. P02 remains open.

W08.39 adds the credential-free P03 backup, restore and disaster-recovery
contract in `tests/tidb/production-backup-policy.json`, enforced by
`scripts/verify-w08-production-backup.mjs` and its eight-case regression suite.
The policy requires transactionally consistent `tidb-br` metadata snapshots
with revision capture, encrypted immutable/versioned block retention,
isolated restore with a separate identity and no production-writer access,
fresh-client readback, corruption/partial-object/region-loss cases, bounded
60-minute RPO and 240-minute RTO, a second region and data-owner/release-owner
sign-off references. Weak retention, missing consistency, unsafe restore
access, unbounded objectives and incomplete DR sign-off fail closed. The
checks are wired into both W08 release workflows. This is a repository
implementation/control slice of P03 only; it does not prove a backup,
restore, second region, provider recovery drill or production approval. P03
remains open. The implementation was published as `896330af` in reconciled
public merge `924009af061d119404b2ee1f59e86506f7b1cbd2`; shared-wrapper Cargo
check and strict Clippy passed on that merge. Push-triggered hosted run
`35716566398` ended cancelled with `jobs=[]`. Dispatched run `35716709958` /
job `106709806523` eventually completed `success` at stale source
`32b85965deef60c84d90f5fb353f45fb6001a303`; this is hosted policy/static
evidence for the repository control only, not a provider restore or production
result, so P03 remains open.

W08.40 adds the credential-free P04 upgrade, compatibility and rollback
contract in `tests/tidb/production-upgrade-policy.json`, enforced by
`scripts/verify-w08-production-upgrade.mjs` and its eight-case regression
suite. The policy requires pinned current/previous component versions, the
Rust SDK, CLI, N-API, FUSE and NFS consumer surfaces, rehearsed wire/data-read
compatibility, expand-contract schema and versioned config migration,
forward/backward compatibility, interrupted-upgrade recovery, rolling
quorum-preserving upgrade gates, retained artifact/config/data rollback,
writer fencing, fresh-client readback, a 30-day artifact-retention floor and
service-owner/release-owner sign-off references. Destructive migrations,
missing client surfaces, quorum loss, weak retention and incomplete rollback
controls fail closed. The checks are wired into both W08 release workflows.
This is a repository implementation/control slice of P04 only; it does not
prove a live upgrade, rollback, provider compatibility or production approval.
P04 remains open. The implementation was published as `016052b7` in
reconciled public merge `66164b78e03309bf9c36ae01ab7e98fc61925373`; shared-
wrapper Cargo check and strict Clippy passed on that merge. Push-triggered
hosted run `35718296219` ended cancelled with `jobs=[]`. Dispatched run
`35716709958` / job `106709806523` completed `success` at stale source
`32b85965deef60c84d90f5fb353f45fb6001a303`, but that source predates W08.40;
no terminal hosted P04 qualification is claimed and P04 remains open.

W08.41 adds the credential-free P05 observability, SLO and alerting contract in
`tests/tidb/production-observability-policy.json`, enforced by
`scripts/verify-w08-production-observability.mjs` and its eight-case regression
suite. The policy requires private managed metrics, structured redacted logs,
redacted tail-sampled traces, retention floors, 99.9% availability and 0.1%
write-error SLO bounds, a 30-day error-budget window, primary and secondary
on-call routes, paging, a test alert, 15-minute acknowledgement, runbook
linkage, no secret-bearing alert payloads and the `/healthz`/`/readyz`
no-store/no-sniff contract. Missing redaction, weak SLOs, absent paging, slow
acknowledgement and invalid health paths fail closed. The checks are wired
into both W08 release workflows. This is a repository implementation/control
slice of P05 only; it does not prove a live collector, dashboard, pager
delivery, alert acknowledgement or production approval. P05 remains open.

The W08.41 implementation was committed as `c0782e3c` and published in
reconciliation merge `e04c28a3058690d08eff4bd7dc528808d57d35cd`; concurrent
mainline work then fast-forwarded the exact public tip through the ledger
merge `725a4370` to `3884c194e705bf672d3d94a4aab5fc548908c9b3`, which contains
W08.41. The local P05 policy/test, shared-wrapper Cargo check, strict Clippy, all P01–P05
policy/test pairs, rollout/evidence controls, workflow parses and diff check
passed. Hosted run `35720816013` for `e04c28a3` and follow-on run
`35720853772` for `65776be8`, and ledger-checkpoint run `35721816135` for
`725a4370` ended cancelled before job creation (`jobs=[]`). Current-tip run
`35721851677` for `3884c194` was pending at the latest observation, so no
current-tip hosted qualification evidence exists. P05 remains open:
the live collector, dashboards/error budget, alert delivery and acknowledgement,
named on-call ownership, provider-aware staging evidence and production
approval are still required.

W08.42 adds the credential-free P06 capacity, load and soak contract in
`tests/tidb/production-capacity-policy.json`, enforced by
`scripts/verify-w08-production-capacity.mjs` and its eight-case regression
suite. The policy requires five workload profiles (baseline, peak, saturation,
failover and soak), write/read/truncate/reopen operations, explicit concurrency
and duration, p95/p99/error/throughput limits, external CPU/memory/disk/network/
IOPS collection, resource utilization/headroom/growth/cost limits, SQL
frontend/TiKV/PD/RustFS failover with fresh-client integrity readback, a
four-hour soak with reopen cycles and owned cleanup, and named workload,
performance, cost and operations owners. Missing profiles, weak thresholds,
resource telemetry, failover, cleanup or ownership fail closed. This is a
repository implementation/control slice of P06 only; it does not execute a
production-sized workload or close the capacity, cost, failover or approval
gate.

The W08.42 implementation was committed as `5dedf181` and published in
reconciliation merge `d6b1bceac918770e4700e3f44aebb94f1ebb513e`; concurrent
mainline documentation then advanced the exact public tip through
`de5c9ebe2c14fb7ae2f2aecb861c6fbf22f446c4` to
`186b0d081086abbf91bafd80017a15bc2ad71288`, which contains W08.42. The local
P06 policy/test, shared-wrapper Cargo check, strict Clippy, all P01–P06
policy/test pairs, rollout/evidence controls, workflow parses and diff check
passed. Hosted run `35723892801` for `d6b1bcea` ended cancelled with `jobs=[]`;
run `35723996718` for `de5c9ebe` was queued; current-tip run `35724261088` for
`186b0d08` was pending at the latest observation. No current-tip terminal
hosted qualification exists. P06 remains open until representative provider
workloads produce measured latency, throughput, error, resource, failover,
soak, scaling and cost evidence with named sign-off.

W08.43 adds the credential-free P07 security and transport contract in
`tests/tidb/production-security-policy.json`, enforced by
`scripts/verify-w08-production-security.mjs` and its eight-case regression
suite. The policy requires TLS 1.3, CA-chain and hostname validation, bounded
certificate rotation with overlap/revocation, private segmented
metadata/block/client networking with default-deny ingress/egress, workload
identity, resource-and-prefix tenant isolation, least privilege and separated
admin/two-person break-glass access, dependency/image/SBOM/provenance scans
with critical-vulnerability blocking, required threat-model status and owner,
immutable redacted audit events, a credentialed TLS handshake using external
endpoint/CA references with cleanup, and named security/network/platform
owners. Weak controls fail closed. This is a repository implementation/control
slice of P07 only; it does not execute a credentialed handshake or provide
production security sign-off.

The W08.43 implementation was committed as `d5d99e2d` and reconciled and
published in merge `be86fd30cd8221fe0a14813288b0c9f99faa8e14`. Concurrent
mainline work then advanced the exact public tip to
`a7e459e6a9749384d409e4f53d6938df9a4414b9`, which contains W08.43. The local
P07 policy/test passed with
`W08_PRODUCTION_SECURITY_POLICY_PASS tls=1.3 rotation_max_days=90 zones=3
identity=workload-identity scan_retention_days=90 audit_retention_days=90
handshake=credentialed` and
`W08_PRODUCTION_SECURITY_TEST_PASS cases=8`; shared-wrapper Cargo check and
strict Clippy also passed on the reconciled public source. Hosted run
`35725892916` for exact `be86fd30` ended cancelled with `jobs=[]`; current-tip
run `35725961204` for `a7e459e6` was pending at the latest observation, so no
current-tip terminal hosted qualification exists. P07 remains open until
credentialed TLS and certificate-rotation, network/authz/tenant isolation,
supply-chain, threat-model, immutable-audit and security-sign-off evidence is
executed in the approved environment.

W08.44 adds the credential-free P08 operations/readiness contract in
`tests/tidb/production-operations-policy.json`, enforced by
`scripts/verify-w08-production-operations.mjs` and its eight-case regression
suite over `docs/W08-operations-runbook.md`. The policy requires all D01–D09
failure drills with P01–P09 mappings, required signals and recovery checks,
cleanup/rollback outcomes and owner roles; a complete redacted evidence record
with immutable revision, configuration, provider, topology, environment,
people, timestamps and terminal result; incident commander, writer-stop,
evidence-preservation, failure-domain and fresh-client/integrity closure
controls; and 24x7 primary/secondary routes, escalation linkage, tabletop
coverage and a maximum 15-minute acknowledgement. Local policy/test output
passed with `W08_PRODUCTION_OPERATIONS_POLICY_PASS drills=9 evidence_fields=9
incident_domains=7 ack_minutes=15 runbook=docs/W08-operations-runbook.md` and
`W08_PRODUCTION_OPERATIONS_TEST_PASS cases=8`. This remains an implementation
and tracking boundary: the runbook is still a controlled NO-GO template and no
drill, page, incident record or production operational sign-off was executed.

The subsequent public-tip source verification at
`76c2b1a863c23afe71c0591d0a480433e1b9078d` passed the locked offline workspace
tests and strict Clippy with `-D warnings` using a bounded external Cargo
target. This is implementation evidence only; credentialed provider,
production topology, operational, canary and approval gates remain open.

Tested source base `c9df268902335934dbe2c369de881803ca376bcd` was freshly
reverified locally after the 9P N-API server-lifecycle gate isolation, the
WebDAV bounded propfind/copy failure fix, the
FoundationDB storage/test qualification changes,
9P undefined-UID preservation, W07 lease-authority telemetry, N-API
postbuild/session-metadata changes, the W26 fenced-metadata publication fast
path, the 9P platform-type alias and 9P direct-probe
absence-field normalization and
WebDAV shared-resource ordering qualification, plus the concurrent WebDAV
mounted-restart cleanup correction, chunked lease-release fix, 9P synchronous
mount inspection, FoundationDB qualification-harness updates and S3
conditional-put/session-concurrency gateway coverage,
N-API provider-network cleanup, RustFS/Ozone lockfile refreshes, 9P
bounded-reader fix, 9P frame-assembler, WebDAV native-concurrency, R2 upload
coalescing, N-API
declaration/P9 normalization, 9P codec, S3-session-concurrency, session-parity,
chunked durability, FUSE, S3-test, WebDAV and W05 provider-matrix lockfile
updates: the full locked workspace test suite exited 0 and strict workspace
Clippy with `-D warnings` exited 0.
The four W08 rollout/evidence policy commands also passed with 36 functional
items, 9 open production gates, 7 rollout tests and 11 evidence tests; the
packet remains NO-GO with zero evidence records. Changed N-API JavaScript and
package JSON also passed static checks. This is source-health and tracking-
control evidence only and does not close any production gate. Provider/native
rows requiring TiDB, RustFS, PGlite, R2, FUSE or NFS remained explicit opt-in
skips. This exact merged source was requalified after the 9P workflow/server,
WebDAV, FoundationDB, 9P, W07 and N-API source changes; no source result is
inferred from documentation-only evidence.

After origin/main advanced with the WebDAV N-API test update `de78011` and
concurrent documentation, exact merged source
`068c42d6f540e31e88368e59546f19dd595f1070` was freshly requalified. The full
locked workspace test and strict workspace Clippy exited 0; changed N-API
JavaScript/package checks, all four W08 rollout/evidence validators/tests,
native-9P workflow YAML parsing and `git diff --check` also passed. This is
source-health and tracking-control evidence only; provider/native rows remain
explicit opt-in skips and production P01–P09 remain open.

After a subsequent origin/main advance with W01/W07/S3 N-API and session
source changes, exact merged source
`a47dbb75b9830fc83e17cc7dfa78b06ad3b9077b` was freshly requalified. The full
locked workspace test and strict workspace Clippy exited 0; all runnable tests
passed, provider/native rows remained explicit opt-in skips, and the affected
N-API, W07 and W08 static/policy checks also passed. This is source-health
evidence only and does not close P01–P09 or change the NO-GO decision.

After origin/main advanced with the S3 delete-error source fix `d5c3f672` and
its gateway coverage, exact merged source
`6f8548a9179083ee088d4c1f66ded8d4c3b5b87e` was freshly requalified. The full
locked workspace test and strict workspace Clippy exited 0; all runnable tests
passed, provider/native rows remained explicit opt-in skips, and affected
N-API/package, W08 policy/evidence and native-9P workflow-shape checks passed.
This is source-health evidence only and does not close P01–P09 or change the
NO-GO decision.

After origin/main advanced with the WebDAV listener restart/backpressure and
bounded recursive-mutation fixes, duplicate-header handling and related N-API
changes, exact merged source
`7752dd2e26ab5f423dfd984c1f9e5887465fd9c9` was freshly requalified. The full
locked workspace test and strict workspace Clippy exited 0; all runnable tests
passed, the WebDAV test group reported 26 passing tests, provider/native rows
remained explicit opt-in skips, and affected N-API/package, W08 policy/evidence
and native-9P workflow-shape checks passed. This is source-health evidence only
and does not close P01–P09 or change the NO-GO decision.

Hosted W08 policy run `35681936375` at source
`e7be3769dd7c6722ce096c481f30d042d7895dbe`, job `106600554617`, completed
successfully in 2m48s. Its rollout-ledger and release-identity/provenance
checks are hosted implementation/static evidence only; they do not create
provider, candidate-release, canary, rollback or owner-approval evidence.

The later hosted W08 policy run `35682838216` at source
`1eef1d9df6c9b05350c1857a72237bb7e5603fac`, job `106603416051`, completed
successfully in 2m47s with the rollout-ledger and release-identity/provenance
checks green. The hosted native-9P workflow run `35682638941` at source
`007e6545d1b25d708abfa10f2120f81fba59a74a` also completed successfully; its
`native-9p` job `106602683880` and `N-API native 9P lifecycle` job
`106602684115` were both green. These are hosted implementation/native
functional qualification only. They do not supply production topology,
provider, candidate-release, registry, canary, rollback or owner-approval
evidence, so P01–P09 remain open and the decision remains NO-GO.

The public source-equivalent checkpoint
`62e86dc092dfe9816ef54ca681872ff9f51d8895` passed hosted W08 release-policy
run `35683936652`, job `106607017325`, which completed successfully in
approximately 2m47s. Its rollout-ledger and release-identity/provenance steps
  were green. This is hosted implementation/static evidence only; it does not
  provide production topology, provider, candidate-release, registry, canary,
  rollback or owner-approval evidence, so P01–P09 remain open and the decision
  remains NO-GO.

The immediately subsequent public merge tip
`622dd0dc82125ba1979ea7ebf2b6a1b11145c6bb` had W08 policy run `35684358649`
cancelled before job creation (`jobs=[]`) when concurrent public tip
`9563d2db8d73b8583212eed00f5b909cbcadf27e` arrived. The surviving current
public-source-equivalent run `35684400799` at source `9563d2db`, job
`106608087688`, completed successfully in 2m05s with both W08 policy steps
green. The cancellation is a hosted scheduling boundary, not evidence of a
source or production failure; the successful run remains implementation/static
evidence only and P01–P09 remain open.

The current public source-equivalent checkpoint
`1626d5381625d81696fa28624342f07087593760` passed hosted W08 release-policy
run `35684975387`, job `106609795509`, which completed successfully in 2m46s
with both hosted policy steps green. This remains hosted implementation/static
evidence only; it does not provide production topology, provider,
candidate-release, registry, canary, rollback or owner-approval evidence, so
P01–P09 remain open and the decision remains NO-GO.

After origin/main advanced with the FoundationDB/TiDB storage changes, 9P/W07
telemetry updates and N-API test expansion, exact merged source
`858682f63ca5b2da3f3610a8d8d641d77c2a904e` was freshly requalified. The full
locked workspace test and strict workspace Clippy exited 0; all runnable tests
passed, provider/native rows remained explicit opt-in skips, and affected
N-API/package, W08 policy/evidence and native-9P workflow-shape checks passed.
This is source-health evidence only and does not close P01–P09 or change the
NO-GO decision.

The published source-equivalent checkpoint
`a47c0cd91699deea9888d3d87aeb04b64fdb6576` passed hosted W08 release-policy
run `35685756342`, job `106612127535`, which completed successfully in 2m47s
with both hosted policy steps green. This remains hosted implementation/static
evidence only; it does not provide production topology, provider,
candidate-release, registry, canary, rollback or owner-approval evidence, so
P01–P09 remain open and the decision remains NO-GO.

The public source-equivalent checkpoint
`1e64bc250b5e863620f069aad173945dc474c5b4` passed hosted W08 release-policy
run `35686337804`, job `106613902895`, which completed successfully in 2m48s
with both hosted policy steps green. This remains hosted implementation/static
evidence only; it does not provide production topology, provider,
candidate-release, registry, canary, rollback or owner-approval evidence, so
P01–P09 remain open and the decision remains NO-GO.

The latest pushed W08 ledger checkpoint `7946979c9e5f3c91e4914a281bdcd2153f983706`
passed hosted W08 policy run `35689651032`, job `106623733056`, in 2m41s with
both the rollout-ledger and release-identity/provenance steps green. The
checkpoint was documentation-only relative to exact Rust qualification
`761cb9d0`; this is hosted implementation/static evidence only and does not
provide provider, production topology, candidate-release, registry, canary,
rollback or owner-approval evidence. P01–P09 remain open and the decision
remains NO-GO.

Exact merged source `4f9120a2e61a260ea14af0953bcc3f7dc5cafe3` was then
requalified after the concurrent S3 streamed-response fix: the shared-target
full locked workspace test and strict Clippy exited 0, the changed N-API
JavaScript passed `node --check`, all W08 rollout/evidence validators/tests
passed, and `git diff --check` passed. Hosted W08 policy run `35690334790`,
job `106625842251`, completed successfully in 2m05s with both policy steps
green. The later public documentation-only tip is `7801ca48`; the run is
implementation/static evidence only and does not close provider, production,
candidate-release, canary, rollback or owner-approval gates. P01–P09 remain
open and the decision remains NO-GO. Runs `35690233250` and `35690379506`
were cancelled before job creation with `jobs=[]` and are not evidence.

The current public source `dccd8351690ba21b4ea01ab8680369c76c442041` was
then freshly requalified after the S3 pipelined-response coverage and
concurrent W01/W26 updates: the shared-target full locked workspace test and
strict Clippy exited 0, changed N-API/package and workflow YAML checks passed,
and W07/W08 tracking validators plus `git diff --check` passed. Hosted W08
policy run `35691426889`, job `106629152293`, completed successfully in 2m24s
with both policy steps green. This is exact source-health and hosted
implementation/static evidence only; the intervening cancelled runs had
`jobs=[]` and are not evidence. No provider credentials, production
deployment, candidate tag, canary, rollback or owner approval exists; P01–P09
remain open and the decision remains NO-GO.

The reconciled current public source
`5a6d6507c6deac160f54a246a2d715c05fc35268` was freshly requalified after the
S3 rejected-body reuse coverage and concurrent W01/W04/W05/W07 updates. The
full locked workspace test and strict Clippy exited 0; all runnable tests
passed, provider/native rows remained explicit capability-gated skips, changed
N-API/package checks and five workflow YAML parses passed, and W07/W08 tracking
validators/tests plus `git diff --check` passed. Hosted W08 policy run
`35692664144`, job `106632773424`, completed successfully in 2m48s with both
policy steps green. The run's only annotations are GitHub platform deprecation
notices, not W08 failures. This is exact source-health and hosted
implementation/static evidence only; no provider credentials, production
deployment, candidate tag, canary, rollback or owner approval exists. P01–P09
remain open and the decision remains NO-GO.

The subsequent public S3 abandoned-download test source `09eca17b` was
reconciled with the W08 ledger in exact merge
`0a28ecef0152c4cc2c80ca27b19bde16faf5f05e`. The full locked workspace test and
strict Clippy exited 0; all runnable tests passed, changed N-API/package checks,
five workflow YAML parses, W07/W08 tracking validators/tests and
`git diff --check` passed, while provider/native rows remained explicit
capability-gated skips. The post-publication hosted W08 policy retest is still
pending for this merge; the last terminal hosted policy result is
`35692664144` / job `106632773424` for source `5a6d6507`. No provider
credentials, production deployment, candidate tag, canary, rollback or owner
approval exists. P01–P09 remain open and the decision remains NO-GO.

The latest source-bearing public reconciliation added the 9P stale-generation,
TiDB autocommit, N-API session and stable fault-qualification changes through
public source `8e76aa44`; its W08 policy attempts `35693830823` and
`35693839548` both cancelled before job creation with `jobs=[]`, so neither is
hosted evidence. The exact local merge
`b996cc735ed0d5c2cb6977fb6b63cb623a005bba` includes that source plus the W08
ledger and passed the full locked workspace test, strict Clippy, six workflow
YAML parses, N-API/package checks, W07/W08 tracking validators/tests and
`git diff --check`. The hosted W08 retest for this merge is pending
publication. No provider credentials, production deployment, candidate tag,
canary, rollback or owner approval exists. P01–P09 remain open and the
decision remains NO-GO.

The next public source `76eb2914bf8b9011cfe87984bdcd9068b29f4f3e` added the
S3 graceful-close coverage and release-gate workflow updates; its W08 policy
run `35694308868` cancelled before job creation with `jobs=[]`, so it is not
hosted evidence. The exact local merge
`d54ea71e85eb6dae1b2c161831f75ab13727b968` includes that source plus the W08
ledger and passed the full locked workspace test, strict Clippy, seven
workflow YAML parses, N-API/package checks, W07/W08 tracking validators/tests
and `git diff --check`. The hosted W08 retest for this merge is pending
publication. No provider credentials, production deployment, candidate tag,
canary, rollback or owner approval exists. P01–P09 remain open and the
decision remains NO-GO.

The current public source `5e3680117c26f5b7bb1b9280eec65a350b3dd2f7` added
the FoundationDB lease-authority transaction change. Its exact W08 policy run
`35694753920`, job `106639089448`, remained `queued` at observation and is not
hosted evidence. The exact local merge
`6f64f6a6686a38c6009c4cdf913285b7ff2e1e1f` includes that source plus the W08
ledger and passed the full locked workspace test, strict Clippy, seven
workflow YAML parses, N-API/package checks, W07/W08 tracking validators/tests
and `git diff --check`. The post-publication hosted retest for this merge is
pending. No provider credentials, production deployment, candidate tag,
canary, rollback or owner approval exists. P01–P09 remain open and the
decision remains NO-GO.

The following public reconciliation added W26/site documentation and N-API
session-test surface changes through
`e7850fb41775351503e5aa685484906b3a3cbbe4`. Its W08 policy run
`35695239705` is pending and is not hosted evidence. The exact local merge
`2575770676d806e4ad2d64e9ac0ea0fea9da66d1` includes that public source plus
the W08 ledger; targeted N-API syntax/package checks, W07/W08 tracking
validators/tests and `git diff --check` passed, with no new Rust source change
in this range. No provider credentials, production deployment, candidate tag,
canary, rollback or owner approval exists. P01–P09 remain open and the
decision remains NO-GO.

The W08 ledger was published in `2d46d704`; its exact hosted policy run
`35695597625` cancelled before job creation with `jobs=[]`. Public docs-only
reconciliation then advanced through `640ddf9c` to current tip
`7cfdafbca77ab1597a17c7a134011bf8677a70c3`; run `35695648791` for
`640ddf9c` also cancelled with `jobs=[]`, while current run `35695725156` is
pending. These are hosted scheduling boundaries, not evidence. The current
tree is source-equivalent to the targeted-clean `25757706` tree; no new Rust,
provider or native result is claimed. No provider credentials, production
deployment, candidate tag, canary, rollback or owner approval exists. P01–P09
remain open and the decision remains NO-GO.

The next W08 ledger publication `60fcef71` was pushed to shared `origin/main`,
but its exact W08 policy run `35696013114` cancelled before job creation with
`jobs=[]`. Shared main then advanced through `10cb081c` (exact W08 run
`35696137073`, also cancelled with `jobs=[]`) and the W07/W04 CI-and-ledger
control tip `8834abd3`. The reconciled ledger merge `a8c78312` was safely
pushed to shared `origin/main`; its exact W08 policy run `35696819458` then
cancelled before job creation with `jobs=[]`. Shared main then advanced
through W04/W26/site documentation tips to public merge `09b554a1`, then
through the 9P/N-API audit tip `75c149f8` and reconciled public merge
`20330a36`. Its W08 run `35697338233` reached terminal success in
`w08-release-policy` job `106647435583` in 2m43s, and later exact public run
`35697637897` reached success in job `106648176391` in 2m41s; these are hosted
implementation/static evidence only. Shared main then advanced through 9P
audit documentation tip `a3305079` and reconciled public merge `ce984ad0`;
run `35697911432` reached success in job `106648941097` in 2m40s. Shared main
then advanced through 9P hosted-gate documentation tip `cc4c65d1` and
reconciled public merge `a73197b6`; the exact current public run
`35698296320` is pending and is not evidence. The current exact public tip is
`a73197b6819714791aa45065ec026fd09025c823`. These scheduling boundaries and
source-equivalent mainline changes add no provider/native or production
qualification. The latest retained terminal hosted W08 policy pass is
`35697911432` / job `106648941097` for source `ce984ad0`. No provider
credentials, production deployment, candidate tag, canary, rollback or owner
approval exists. P01–P09 remain open and the decision remains NO-GO.

The latest source-bearing shared checkpoint
`323820683fcd3c063ab392c81eb12906ec8ab2e0` passed the full locked workspace
test and strict Clippy with warnings denied on an isolated non-incremental
Cargo target; changed N-API/package checks, six workflow YAML parses, all
W07/W08 tracking/evidence suites and diff hygiene also passed. Hosted W08 run
`35699181097`, job `106653083216`, completed successfully in 2m42s. This is
implementation/static qualification only. Shared main then advanced with
documentation-only updates to public tip
`b31291c1cda67ecb1928970a766d6e17af841b56`; current run `35699754200` is
queued and is not evidence. No provider/native or production gate is closed;
P01–P09 remain open and the decision remains NO-GO.

The latest exact source-health requalification covered shared source
`33f52cdaaefcee268cf633d6c0852d0bc685b975` after the FUSE and TiDB-test/CI
updates. The full locked workspace test and strict Clippy with `-D warnings`
passed using isolated target `/private/tmp/mount-rs-w08-qual-TeImd6`; all
runnable tests passed and provider/native rows remained explicit opt-in skips.
Six workflow YAML files, changed N-API/package checks, W07/W08 tracking and
evidence validators/tests, and `git diff --check` also passed. Public
`origin/main` then advanced documentation-only to
`3a14de2f31862cec1ecd40206c4703aacb4f5309`. Hosted W08 run `35700945120`
has that exact head SHA and was still `in_progress` at the 17:45 AEST audit,
so it is pending and not evidence. No provider/native or production gate is
closed; P01–P09 remain open and the decision remains NO-GO.

A fresh read-only production-boundary audit at **2026-09-22 17:43–17:45
AEST** found no `w08-production-release.yml` runs, HTTP 404 for the protected
`w08-production` environment, only the prerelease `v0.1.0-cli-preview`, and no
`v*-cli-production-candidate*` tag. The separate W08 policy run above is a
non-production implementation/static workflow and does not change this
boundary. No production mutation, candidate publication, canary, rollback or
approval was attempted or inferred; P01–P09 remain open.

The subsequent source-bearing public updates `fe4a6bbf` and `7de703da`
(chunked mutation batching and FUSE forced-unmount read draining) were
requalified in exact public source `245258d9`: the full locked workspace test,
strict Clippy, six workflow YAML parses, changed N-API/package checks, W07/W08
tracking/evidence suites and diff hygiene all passed. Public tip `0044d020`
then added only W05 documentation. W08 run `35701909098` for published merge
`e44506ab` and run `35702023377` for `7de703da` were cancelled by concurrent
mainline scheduling; current run `35702239279` targets `0044d020` and was
pending at 17:59 AEST. These are implementation/static scheduling boundaries,
not provider or production acceptance, so P01–P09 remain open and the
decision remains NO-GO.

The W08 ledger refresh is now published in merge
`fb6ef40c7774aefb7251dba5867104798953381c`, with local `HEAD`, `origin/main`
and the public main ref equal and clean at that SHA. Concurrent changes after
the exact W08 qualification are outside the W08 Rust/provider/native path; no
new W08 provider or production result is inferred. Hosted W08 run
`35702979994` targets the exact published merge and was still `pending` at
18:07 AEST, so it is not evidence. P01–P09 remain open and the decision
remains NO-GO.

The public mainline subsequently advanced to merge
`b4e7c8686cc8fe9006e880b8eaccb4a6bc4262f3`, preserving the W08 ledger and
concurrent W07/W05/W26 updates. The latest exact W08 Rust qualification is
still `245258d9` after the FUSE/chunked source `7de703da`; no new W08
provider/native result is inferred for the later documentation/control
changes. Hosted W08 run `35703582574` targets the exact merge and remained
`pending` at 18:14 AEST. The same read-only audit found no production-release
runs, HTTP 404 for `w08-production`, only `v0.1.0-cli-preview`, and no
production-candidate tag. P01–P09 remain open and the decision remains NO-GO.

The latest source-bearing N-API/9P updates were requalified in exact merged
source `35523b9f` after public base `24b83a60`: the full locked workspace test,
strict Clippy, six workflow YAML parses, changed N-API/package checks including
the 9P lifecycle/order tests, W07 platform-evidence control, all W07/W08
tracking/evidence suites and diff hygiene passed. Public main then advanced
with documentation-only W05/W08 reconciliation through `f177e067`; hosted
W08 run `35704230143` targets that exact public tip and was `pending` at 18:20
AEST. This is source-health and hosted scheduling evidence only; no provider,
native, production, candidate, canary, rollback or approval gate is closed.
P01–P09 remain open and the decision remains NO-GO.

The exact W08 qualification tip was published as `20835a07` and verified
equal to local `HEAD`, `origin/main` and the public main ref with a clean
checkout. Its hosted W08 policy run `35704624070` targets the exact published
SHA and was still `pending` at 18:25 AEST. This is hosted scheduling evidence,
not a terminal implementation/provider/production result. The production
workflow still has no runs, the protected environment is unavailable (HTTP
404), only `v0.1.0-cli-preview` exists, and no production-candidate tag is
present; P01–P09 remain open and the decision remains NO-GO.

The subsequent TiDB storage source update `2ce6f753` was merged into exact
qualification boundary `adfae4b1` with the prior N-API/9P changes. Compile-only
locked workspace checks and strict Clippy passed, but the full linked workspace
test stopped before execution when macOS `xcrun --sdk macosx --show-sdk-path`
required an unaccepted Xcode license (link exit 69). The previous exact source
`35523b9f` remains the latest full linked-test PASS. Public main then advanced
docs-only through `bd1481ba`; hosted W08 run `35705304456` targets that exact
tip and was `pending` at 18:32 AEST. This is a native host/toolchain and hosted
scheduling boundary, not a provider or production acceptance; P01–P09 remain
open and the decision remains NO-GO.

The latest S3/N-API source-bearing merge `d81c4f4` (S3 session/gateway cleanup
and N-API in-flight crash/concurrency coverage) passed compile-only locked
workspace check and strict Clippy. The linked test remains blocked by the same
unaccepted Xcode license; `35523b9f` is still the latest full linked-test PASS.
Public main then added W07 provenance/control and docs-only updates through
`ab74870c`; hosted W08 run `35705868850` targets that exact public tip and was
`pending` at 18:38 AEST. No provider, native, production, candidate, canary,
rollback or approval gate is closed; P01–P09 remain open and the decision
remains NO-GO.

The final combined source boundary `3fe2a02d` includes PGlite source
`c791ab31` plus the NFS/WebDAV/S3/N-API and TiDB/N-API/9P updates. Compile-only
locked workspace check and strict Clippy passed; the linked test remains
blocked before execution by the unaccepted Xcode license (link exit 69), so
`35523b9f` remains the latest full linked-test PASS. Public main then advanced
docs-only through `214169ed`; hosted W08 run `35706621318` targets that exact
tip and was `pending` at 18:46 AEST. This is source-health, native-host and
hosted scheduling evidence only; no provider or production gate is closed.
P01–P09 remain open and the decision remains NO-GO.

After origin/main advanced with the 9P EOF/backpressure server fix `1179d9e3`,
N-API test expansion and site component updates, exact merged source
`d725248534eb897de4d49e916c47425e6266c05d` was freshly requalified. The full
locked workspace test and strict workspace Clippy exited 0; all runnable tests
passed, provider/native rows remained explicit opt-in skips, and affected
N-API/package, W08 policy/evidence and native-9P workflow-shape checks passed.
This is source-health evidence only and does not close P01–P09 or change the
NO-GO decision.

A fresh read-only production-boundary audit at **2026-09-22 14:23 AEST**
returned no `w08-production-release.yml` runs, HTTP 404 for the
`w08-production` environment, only prerelease `v0.1.0-cli-preview`, and no
`v*-cli-production-candidate*` tag. This is an external GitHub/API and
release-configuration blocker; no production mutation or approval was
attempted, so P09 remains open.

A fresh credential-free admission check at **2026-09-22 11:36 AEST** passed the
positive production-config fixture with a non-secret TLS-policy URL supplied
out of band; the insecure/inline-secret fixture failed closed as expected. The
pending release-manifest fixture passed, the strict accepted fixture passed
with `--require-release-acceptance`, and the invalid fixture failed closed.
These are repository policy controls only: no provider connection, artifact
signature, SBOM service, canary, rollback or approval was performed.

The latest read-only production-boundary audit at **2026-09-22 11:08 AEST**
returned HTTP 404 from the W08 production workflow query and `w08-production`
environment API; `gh release list` could not resolve the repository, and
`git ls-remote` found no `v*-cli-production-candidate*` tag. The fetched public
mainline contains the protected workflow file, so the failed API calls are not
promoted to a release claim. The last successful release observation (08:38
AEST: only `v0.1.0-cli-preview`) is retained as the latest known release state.
This remains an external
GitHub/API and release-configuration blocker; no production gate is closed.

A fresh read-only audit at **2026-09-22 11:57 AEST** returned the same HTTP 404
for the W08 production workflow query and protected environment; the release
API still could not resolve the repository, and no candidate tag was found.
The protected workflow file remains present in fetched mainline, so this is
still an external GitHub/API and release-configuration blocker rather than a
missing repository implementation. No production gate is closed.

A fresh read-only audit at **2026-09-22 12:25 AEST** returned HTTP 404 for both
the production and policy workflow queries and the protected environment;
`gh release list` still could not resolve the repository, and the candidate-tag
query returned no tag. A later branch-ref `git ls-remote` hit transient DNS
resolution failure, so it is not promoted to a public-ref claim; the exact
public equality for checkpoint `ea49dc65` was already verified at 12:24 AEST.
No release, canary, rollback or approval evidence was created, so P09 remains
externally blocked.

A local prerequisite probe at **2026-09-22 12:30 AEST** could not contact the
Docker daemon, and no live TiDB/R2/PGlite/RustFS endpoint or credential
variables were present. The representative out-of-band TLS-policy config
passed, the inline-secret config failed closed, and the strict accepted
release-manifest fixture passed with `--require-release-acceptance`. These are
repository admission controls only; they do not close P01/P02/P07/P09 or
create provider, signing, SBOM-service, canary, rollback or approval evidence.

A fresh read-only production-boundary audit at **2026-09-22 12:40 AEST** still
returned HTTP 404 for both W08 workflow queries and the protected environment;
the release API could not resolve the repository and no production-candidate
tag exists. No release, canary, rollback or approval evidence was created, so
P09 remains externally blocked.

The W08.11 release-policy job is a separate credential-free implementation
gate. It validates a manifest shape and, when supplied, an artifact checksum;
it does not sign artifacts, create an SBOM, run a canary, perform rollback or
approve a release. Its accepted fixture is synthetic and must not be promoted
to production evidence.

W08.12 wires the same policy to the actual macOS CLI preview artifact path:
the release workflow computes the tarball digest/size, publishes the manifest
beside `SHA256SUMS`, and verifies both again after download. A tag-triggered
release has not been run in this session, so this is implementation and hosted
policy evidence rather than published-release evidence.

W08.13 isolates the policy from the cancellable aggregate CI workflow in
`.github/workflows/w08-release-policy.yml` with `cancel-in-progress: false`.
Hosted run `35611883547`, source `f432441`, job `106372777281` reached terminal
success after building a real Ubuntu CLI artifact, packaging it, verifying its
checksum and artifact-derived manifest, and running direct plus extracted
binary smoke checks. This proves a stable hosted artifact path, not a published
tag release: signing, SBOM, target-platform acceptance, canary, rollback and
approval remain open.

W08.14 adds `scripts/write-w08-release-sbom.mjs` and
`scripts/verify-w08-release-sbom.mjs`. The scripts derive the `mount-rs-cli`
transitive closure from locked Cargo metadata, emit a CycloneDX 1.5 document,
and bind it to the release artifact SHA-256 and source commit. Hosted run
`35614345209`, source `9c9d0e4`, job `106381893114` emitted
`W08_RELEASE_SBOM_PASS` for 288 components and tarball SHA-256
`c3d1a3200c06830f95527dd62310455e66b4632080aee7ead3a118e4783f4543`. This is
an unsigned SBOM artifact-path pass; the manifest intentionally remains
`sbom=pending` until the approved signing/attestation process runs. The tag
release, target-platform parity, canary, rollback and approval remain open.

W08.15 moves `SHA256SUMS` generation after both the manifest and SBOM exist in
the preview and dedicated workflows. Each checksum file now covers the
tarball, `release-manifest.json` and `release-sbom.json`, and the dedicated
hosted run `35615714935`, source `5116ded`, job `106386240669` returned `OK`
for all three entries. This is asset-integrity evidence for a release
candidate, not proof of tag publication, cryptographic attestation,
target-platform parity, canary, rollback or approval.

W08.16 adds `.github/workflows/w08-release-targets.yml` for a Linux x86_64 and
macOS arm64 build/test/package matrix. Each job generates and verifies its
manifest, 288-component SBOM and three-entry checksum file, runs direct and
extracted binary version checks, uploads the tarball/manifest/SBOM/checksum
set, and a separate Ubuntu job re-downloads and verifies that set and target
identity. Hosted run `35617415427`, source `b0ca8a9`, passed build jobs
`106391572292` and `106391572540` plus download jobs `106393402528` and
`106393402680`. This is target-package and hosted artifact-boundary evidence,
not tag publication, cryptographic signing/attestation, canary, rollback or
approval.

W08.17 wires the production-facing attestation path; the tag-triggered CLI
release execution remains open and the manual target path is qualified below.
The tag-triggered CLI release job grants OIDC and attestation
permissions only at that release job, runs pinned `actions/attest@v4.2.2` for
the exact tarball and CycloneDX SBOM, verifies both predicates with
`gh attestation verify` against the repository, signer workflow, source commit,
tag ref and hosted-runner policy, then records `signature=verified` and
`sbom=verified` in the finalized manifest before rebuilding `SHA256SUMS`. The
target matrix has an explicit manual `attest=true` path that performs the same
per-target qualification. Local YAML/embedded-Bash validation and the local
GitHub CLI flag-surface check passed. No approved tag has run, so the
production release signature and registry acceptance remain unclaimed; the
successful manual target qualification is recorded below.

The first live target qualification (`35620932700`, source `11a7b22`) passed
both target builds and both downloaded-asset checks. Its two attestation jobs
failed during setup because GitHub rejected the shortened action ref; no action
step, OIDC token or Sigstore bundle was created. The workflow now uses the
full verified v4.2.2 SHA; the later terminal reruns are recorded in W08.21 and
W08.22 below.

The corrected qualification (`35622899242`, source `2f43721`) then passed both
target builds, both downloaded-asset checks and both attestation-generation
steps for provenance and CycloneDX SBOMs. Its final verification steps failed
only because the workflow supplied the mutually exclusive `--signer-repo` and
`--signer-workflow` options to `gh attestation verify`. W08.20 removes the
redundant repository option from both workflows; that run is not counted as a
verification PASS.

The corrected manual qualification (`35624385556`, source `2ab3cf1`) passed
both target builds, both downloaded-asset checks, both provenance attestations,
both CycloneDX SBOM attestations and both final `gh attestation verify` steps.
The Linux tarball was SHA-256
`5f7f3c6345013144d8889b107cde41c9e5b69d688e21a975ba6fbb33ce2507d6` and the
macOS arm64 tarball was SHA-256
`add8e365c0ab1c0390267531144b77b6c7cf7f338d03ce0a549a5783c834fc8e`.
Repository attestation records `48984689`, `48984696`, `48984678` and
`48984687` were created, with Rekor entries `2906371944`, `2906371970`,
`2906371892` and `2906371927`. This qualifies the hosted target-attestation
path; it is not a published tag release, canary, rollback or approval.

The current-main manual retest (`35627761501`, source `50a33ac` at dispatch)
again passed both target builds, both downloaded-asset checks, both provenance
attestations, both CycloneDX SBOM attestations and both final
`gh attestation verify` steps. The Linux tarball was SHA-256
`432049a39cd648bda95a9aeaaf81d5bb2933267c467f69b617e6e3f718d5303b`
(8,306,821 bytes); the macOS arm64 tarball was SHA-256
`647f4d0fe7cb7f8446ed6eba3a2c1ad4b4f7413ad59afa10d68617b86ec10421`
(6,926,739 bytes). Repository attestation records `48993605`, `48993613`,
`48993668` and `48993679` were created, with Rekor entries `2906426581`,
`2906426592`, `2906426797` and `2906426824`. This is a current-main-at-
dispatch hosted qualification retest; it is not a published tag release,
canary, rollback or approval.

The current published-main qualification (`35631063978`, source `cf75835`)
again passed both target builds, both downloaded-asset checks, both provenance
attestations, both CycloneDX SBOM attestations and both final
`gh attestation verify` steps. The Linux tarball was SHA-256
`54220877022f67640aa7b470a8eaa09b24df1d867e00463533642b6fa462182a`
(8,315,540 bytes); the macOS arm64 tarball was SHA-256
`6df488c71a0eb65674af9adf7660b870424b4e47227d832028180947dbb83b39`
(6,918,800 bytes). Repository attestation records `49001528`, `49001543`,
`49001531` and `49001545` were created, with Rekor entries `2906464391`,
`2906464436`, `2906464411` and `2906464449`. This is hosted qualification
from published main; it is not a published tag release, canary, rollback or
approval.

The first explicit target-matrix dispatch (`35620392878`, source `0a4de6f`)
was accepted but cancelled before job creation because concurrent `main` pushes
occupied the old shared pending concurrency group. W08.18 now keys the target
workflow by event type and ref so a manual attestation qualification does not
compete with push-triggered runs. The cancelled run is recorded as a no-job
boundary, not a PASS or a provider failure; it is superseded by the terminal
manual qualification `35624385556` documented above.

## Deployment contract

The following is a shape, not a production configuration. Replace placeholders
only through an approved deployment change; never commit live URLs, keys,
passwords, tokens, certificates or tenant identifiers.

```json
{
  "version": 1,
  "transport": "auto",
  "driver": {
    "kind": "splitstore",
    "storage": {
      "metadata": {
        "kind": "tidb",
        "connection": { "env": "MOUNT_RS_TIDB_TLS_URL" },
        "volume_key": "<stable-owned-volume-key>",
        "durable": true
      },
      "blocks": {
        "kind": "r2",
        "endpoint": "https://<approved-object-endpoint>",
        "bucket": "<private-bucket>",
        "prefix": "<owned-volume-prefix>",
        "access_key_id": { "env": "R2_ACCESS_KEY_ID" },
        "secret_access_key": { "env": "R2_SECRET_ACCESS_KEY" },
        "durable": true
      },
      "chunk_size_bytes": 4194304,
      "owner": "<stable-writer-owner>"
    }
  }
}
```

The metadata URL must be supplied through the approved secret/runtime path and
must include `require_ssl=true`. The guarded acceptance wrapper also rejects
`verify_ca=false`, `verify_identity=false` and `built_in_roots=false`:

```sh
MOUNT_RS_TIDB_TLS_URL='mysql://<user>:<secret>@<tidb-host>:4000/<database>?require_ssl=true&verify_ca=true&verify_identity=true' \
  ./scripts/test-tidb-tls.sh
```

The URL above is illustrative and must not be copied with a real secret into a
shell history, issue, log or repository. The wrapper emits
`TIDB_TLS_ACCEPTANCE_PASS` only after the real credentialed provider contract
has completed. Its `MOUNT_RS_TIDB_TLS_VALIDATE_ONLY=1` mode is policy
validation only and cannot close P07.

Before a staging or production deployment is started, run the credential-free
deployment policy check against the approved config artifact:

```sh
MOUNT_RS_TIDB_TLS_URL='mysql://<user>@<tidb-host>:4000/<database>?require_ssl=true' \
  node scripts/verify-w08-production-config.mjs /path/to/approved-w08-config.json
```

The verifier requires a `splitstore` contract with durable TiDB metadata and
durable HTTPS S3/R2 blocks, fixed external secret references, a positive chunk
size and no placeholders or inline secret strings. It never opens a provider
connection. The checked-in
`tests/tidb/production-config-policy.json` fixture is a synthetic policy test,
not a production endpoint or deployment approval.

Consumers that use TLS must ship the opt-in client graph:

```sh
CARGOFLAGS="--locked --features rustls" \
  pnpm --dir integrations/mount-rs-napi build

./scripts/cargo-shared check --locked -p mount-rs-sdk --features rustls
./scripts/cargo-shared check --locked -p mount-rs-cli --features rustls
```

`durable: true` is an explicit caller assertion. It must be backed by the
selected TiKV replication, sync-log, storage, backup and failure policy; it is
not inferred from a URL or from a successful `SELECT 1` acknowledgement.

## Evidence commands and boundaries

| Command or job | What it can close | What it cannot close |
| --- | --- | --- |
| `./scripts/test-tidb.sh` with final `TIDB_ACCEPTANCE evidence=durable-multinode-restart` | Real pinned TiDB/PD/TiKV qualification, identity, provider contract and component restart/reopen | Production capacity, power-loss/fsync, backup/restore, IAM, TLS, Node/CLI/native support |
| `./scripts/test-tidb-rustfs.sh` with the retained composition markers | Real TiDB/RustFS seed, partial/truncate/reopen, fencing/CAS, cleanup and bounded consumer/native composition | Production object-store policy, region loss, canary, load/soak or release approval |
| `MOUNT_RS_TIDB_TLS_URL=... ./scripts/test-tidb-tls.sh` | Credentialed TLS-required TiDB provider contract and URL verification policy | Production IAM/rotation, certificate lifecycle, target topology, public consumer package and canary evidence |
| `MOUNT_RS_TIDB_TLS_VALIDATE_ONLY=1 ... ./scripts/test-tidb-tls.sh` | Credential-free configuration-policy validation | Any provider, TLS handshake, credential or production claim |
| `MOUNT_RS_TIDB_TLS_URL=... node scripts/verify-w08-production-config.mjs <config>` | Credential-free deployment-shape, durable-store, HTTPS-block, external-secret-reference and TLS-required policy validation | Topology, IAM grants, certificate trust, backup/restore, capacity, monitoring, provider handshake or production approval |
| `MOUNT_RS_TIDB_RUSTFS_SOAK_OPERATIONS=... ./scripts/test-tidb-rustfs.sh` with terminal `TIDB_RUSTFS_SOAK_PASS` | Bounded real TiDB/RustFS concurrent write/read integrity, client latency percentiles and throughput | Production workload representativeness, resource headroom/cost, multi-hour soak, failover, SLO approval or production capacity |
| `./scripts/cargo-shared check/test/clippy ... --features rustls` | Public TLS graph compilation, unit tests and lint | A network endpoint, certificate trust, provider identity or deployment |
| Hosted `tidb-tls-compile` job `106304951579` | Revision-specific TLS feature and fail-closed guard evidence | Live provider, production identity, capacity, observability or release gates |
| Hosted `tidb-tls-compile` job `106311076905` in run `35592902494` | Terminal TLS feature, URL-guard and production-config policy evidence for source `4326c54` | Live provider, IAM, certificate trust, topology, capacity, observability or release gates |
| Hosted `tidb-rustfs` job `106319766691` in run `35595664981` | Terminal bounded seed/reopen TiDB/RustFS soak evidence for source `c5532e3`; both phases recorded 64 operations, concurrency 8, 65,536-byte payloads and zero errors with p50/p95/p99/throughput markers | Production workload representativeness, resource headroom/cost, multi-hour soak, failover, SLO approval or production capacity |
| `./scripts/cargo-shared test --locked -p mount-rs-http` plus strict Clippy | Local HTTP health/readiness contract: 8 unit tests and 10 integration tests passed, including healthy/not-ready and method-boundary cases | Provider/object-store/TLS health, collector/dashboard/pager, SLO/error-budget approval and production alert execution |
| `./scripts/cargo-shared test --locked -p mount-rs-http --all-features` plus strict Clippy | Local application telemetry and OTLP propagation path: 8 unit tests and 13 integration tests passed with `-D warnings` | Deployed collector/exporter, provider/object-store/TLS health, dashboard/pager, SLO/error-budget approval and production alert execution |
| Hosted CI run `35599817215`, source `b26819e`, `http-observability` job `106333141914` | Terminal hosted all-features HTTP health/readiness, telemetry/OTLP tests and strict-Clippy evidence | Deployed collector/exporter, provider/object-store/TLS health, dashboard/pager, SLO/error-budget approval and production alert execution |
| `./scripts/cargo-shared test --locked -p mount-rs-http --all-features` plus strict Clippy on source `2159976` | Local response-header hardening: health/readiness cache-control and `nosniff` assertions pass with 8 unit and 13 integration tests | Deployed collector/exporter, provider/object-store/TLS health, dashboard/pager, SLO/error-budget approval and production alert execution |
| Hosted CI run `35601990956`, source `f09fbe9`, `http-observability` job `106340034907` | Terminal hosted health-header, telemetry/OTLP and strict-Clippy evidence; `2f7919a` is in the tested source ancestry | Deployed collector/exporter, provider/object-store/TLS health, dashboard/pager, SLO/error-budget approval and production alert execution |
| `node scripts/verify-w08-release-manifest.mjs tests/tidb/release-manifest-policy.json` plus the accepted and invalid fixtures | Credential-free release identity/provenance policy validation; strict acceptance requires verified signature/SBOM and passed canary states | Real artifact generation/checksum attachment, signature/SBOM creation and verification, target-package acceptance, canary, rollback, approval and release registry evidence |
| Hosted CI run `35606873984`, source `fecec0e`, `w08-release-policy` job `106356402785` | Terminal hosted policy success for pending, strict-accepted and expected-negative fixture paths | Real release artifact/provenance, signing/SBOM, target-platform packages, canary, rollback, approval and production deployment |
| `CARGO_TARGET_DIR=/private/tmp/mount-rs-w08-release-target ./scripts/cargo-shared build --locked --release -p mount-rs-cli` plus tarball/`SHA256SUMS`/manifest verification | Local `mount-rs 0.1.0` release artifact and checksum/manifest/content path passed; tarball SHA-256 `da0782596d7402ce64869f2037075d95b273f59f389b9493e26a0ba1e39822c7`, size `6910193` | No published tag, registry, signing/SBOM, target-platform matrix, canary, rollback or approval evidence |
| Hosted CI run `35609172786`, source `66544b5`, `w08-release-policy` job `106363893748` | Terminal hosted generator plus pending/strict-accepted/expected-negative policy evidence | Tag-triggered macOS release publication, asset inspection, signing/SBOM, target-platform acceptance, canary, rollback, approval and production deployment |
| Hosted CI run `35611883547`, source `f432441`, `w08-release-policy` job `106372777281` | Terminal success from the dedicated non-cancelling workflow: real Ubuntu CLI release build/package, `SHA256SUMS`, artifact-derived manifest, tar-content check, direct/extracted `--version` checks and pending/strict-accepted/expected-negative fixture paths | Tag-triggered macOS release publication, published asset inspection, signing/SBOM, target-platform acceptance, canary, rollback, approval and production deployment |
| Hosted CI run `35614345209`, source `9c9d0e4`, `w08-release-policy` job `106381893114` | Terminal success from the dedicated non-cancelling workflow: real Ubuntu CLI artifact, `SHA256SUMS`, artifact-derived manifest, 288-component CycloneDX SBOM, tar-content check, direct/extracted `--version` checks and policy fixtures; SBOM marker carried tarball SHA-256 `c3d1a3200c06830f95527dd62310455e66b4632080aee7ead3a118e4783f4543` | Tag-triggered macOS release publication, published asset inspection, cryptographic signing/attestation, target-platform package/SBOM acceptance, canary, rollback, approval and production deployment |
| Hosted CI run `35615714935`, source `5116ded`, `w08-release-policy` job `106386240669` | Terminal success from the dedicated non-cancelling workflow: real Ubuntu CLI artifact, artifact-derived manifest, 288-component CycloneDX SBOM, and three-entry `SHA256SUMS` verification (`tarball: OK`, `release-manifest.json: OK`, `release-sbom.json: OK`) | Tag-triggered macOS release publication, downloaded asset inspection, cryptographic signing/attestation, target-platform package/SBOM acceptance, canary, rollback, approval and production deployment |
| Hosted CI run `35617415427`, source `b0ca8a9`, target jobs `106391572292`, `106391572540`, `106393402528`, `106393402680` | Terminal success for Linux x86_64 and macOS arm64 build/test/package plus downloaded-asset verification. Linux artifact SHA-256 `875a06fb975d8a294141f5b70841c481768655ac3c5c56a721604216ee12ff2c`; macOS arm64 artifact SHA-256 `50f466bbe22b2fa09ecd8f1246fb278bd2b190fed52f940de21bb8d414baa9bd`; all target jobs passed manifest, 288-component SBOM, three-entry checksums, tar content and target identity | Tag-triggered release publication, registry asset inspection, cryptographic signing/attestation, target-platform release/SBOM approval, canary, rollback, approval and production deployment |
| Hosted CI run `35624385556`, source `2ab3cf1`, target jobs `106415027916`, `106415027629`, `106416531412`, `106416531464`, `106416627127`, `106416627177` | Terminal success for Linux x86_64 and macOS arm64 build/test/package, downloaded-asset verification, provenance attestation, CycloneDX SBOM attestation and final `gh attestation verify`. Linux tarball SHA-256 `5f7f3c6345013144d8889b107cde41c9e5b69d688e21a975ba6fbb33ce2507d6` (8,277,051 bytes); macOS arm64 SHA-256 `add8e365c0ab1c0390267531144b77b6c7cf7f338d03ce0a549a5783c834fc8e` (6,910,097 bytes); repository attestation records `48984689`, `48984696`, `48984678`, `48984687`; Rekor entries `2906371944`, `2906371970`, `2906371892`, `2906371927` | Approved tag-triggered release publication, release-registry asset/SBOM acceptance, staged canary, rollback, approval and production deployment |

The retained W08 functional evidence is CI run `35585066458` at source
`9c098e5`, where the W08-relevant jobs were terminal successes. Its aggregate
workflow was later cancelled by main-branch concurrency and is not reported as
an aggregate-green release result. The later current-main run
`35595664981` likewise had terminal W08-relevant successes (`tidb`,
`tidb-rustfs`, `tidb-tls-compile` and `native-fuse`) but ended with aggregate
`failure` because unrelated FoundationDB, Windows Rust, Ozone and Node matrix
jobs failed; it is not an aggregate-green release result either.

## Rollout sequence

1. Close P01 with the approved target topology, support matrix, resource and
   tenancy limits, SLO/RPO/RTO, owners and explicit non-goals.
2. Close P02 and P07 in a staging environment: inject short-lived credentials,
   enforce TLS and certificate verification, run the guarded provider test,
   prove negative IAM/tenant cases, and retain redacted logs.
3. Execute the procedures and timed drills in
   [`W08-operations-runbook.md`](W08-operations-runbook.md), then close P03–P05
   against that same staging topology with restore, upgrade, rollback,
   collector/paging and measured recovery evidence.
4. Close P06 and P08 with production-shaped workload, soak, fault injection,
   runbook and on-call exercises. Record resource headroom and rollback timing.
5. Confirm the dedicated W08 policy and Linux/macOS target matrix are terminal
   on the release candidate, including unsigned SBOM, three-asset checksum and
   downloaded-asset verification; run the target matrix's approved
   `workflow_dispatch` attestation qualification; configure the protected
   `w08-production` environment and run the approved
   `v*-cli-production-candidate*` tag workflow. Retain its actual Linux/macOS
   target assets, target-specific manifests/SBOMs, aggregate `SHA256SUMS` and
   verified attestation outputs. W08.31 is the latest current-main-at-dispatch
   qualification, but it is not the protected candidate-tag result. Close P09 only after target-platform parity, a
   held-back canary, live SLO observation, rollback verification and
   release-owner approval. The synthetic accepted fixture, local tarball and
   hosted CI artifacts are not sufficient.
6. Promote in stages only after the evidence packet passes the final audit. On
   rollback, stop new writers, preserve metadata and block snapshots, restore
   the last known-good artifact/topology, verify reads/fences/ownership, and
   resume only after the release owner approves.

## Blocker rule

Missing production infrastructure, credentials, certificates, secret-manager
access, monitoring, workload targets, security review, on-call participation
or release approval is an explicit blocker. It must not be replaced with a
local mock, a MySQL-compatible substitute, a compile result, a cancelled job or
an inferred production claim. The rollout remains **NO-GO** until every gate
required by the advertised production scope has terminal evidence and named
owner sign-off.
