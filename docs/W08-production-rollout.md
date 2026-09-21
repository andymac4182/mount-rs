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
| Current implementation capability | TLS-capable provider, Rust SDK, CLI, N-API, a guarded TLS acceptance wrapper, fail-closed production-config and release-provenance policy verifiers, an artifact-manifest generator and locked-Cargo CycloneDX SBOM generator/verifier wired into the CLI preview workflow, three-asset `SHA256SUMS` coverage, a dedicated non-cancelling hosted release-policy gate, a hosted Linux x86_64/macOS arm64 target-package and download-verification matrix, tag-release and protected production-candidate GitHub Sigstore provenance/SBOM attestation paths with strict identity verification, an opt-in target-matrix attestation path, and bounded HTTP `/healthz`/`/readyz` probes are implemented; local unit/Clippy, CLI-schema, policy, real-artifact, SBOM, asset-integrity, target-matrix, workflow-shape and hosted compile/guard checks are tracked separately |
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
| P01 — deployment scope, topology and support matrix | Open — 25% | Approved managed/self-hosted TiDB/PD/TiKV and block-store topology, regions, HA/quorum, network/TLS policy, resource limits, supported versions, tenancy, IaC and a production-like staging smoke/restart result; the checked-in policy gate now verifies the required deployment shape only |
| P02 — secrets, IAM, rotation and audit | Open — 20% | Secret-manager injection, least-privilege metadata/block identities, rotation and revocation without data loss, break-glass procedure, audit and redaction evidence; the policy gate rejects inline secret strings and requires external env references only |
| P03 — backup, restore and disaster recovery | Open — 10% | Defined RPO/RTO and retention, encrypted backups/versioning, clean-environment restore, metadata/block consistency, corruption/partial-object handling and recovery sign-off |
| P04 — upgrade, compatibility and rollback | Open — 10% | Rehearsed TiDB/RustFS/client version matrix, schema/config migration, rolling upgrade, interrupted-upgrade recovery, retained-data rollback and compatibility sign-off |
| P05 — observability, SLOs and alerting | Open — 15% gate weight; local/hosted HTTP contract passed | The HTTP transport's unauthenticated `/healthz` and `/readyz` contract, `Cache-Control: no-store` and `X-Content-Type-Options: nosniff` are locally tested and passed in terminal hosted job `106340034907`; exit still requires provider-aware checks where applicable, production collector, dashboards, SLO/error-budget thresholds, paging, retention/redaction and an exercised alert route |
| P06 — capacity, load and soak | Open — 20% | The bounded TiDB/RustFS soak harness is configured in hosted composition CI and passed its current seed/reopen qualification at 64 operations, concurrency 8 and 65,536-byte payloads; exit still requires representative workload baseline/peak/saturation/failover/soak results with p50/p95/p99 latency, throughput, errors, resource growth, headroom, scaling and cost limits |
| P07 — security, transport and hardening | Open — 20% | TLS and certificate rotation, network segmentation, authz/tenant isolation, dependency/image/SBOM review, threat-model findings, audit checks and a credentialed TLS handshake; local policy validation requires HTTPS blocks and TLS-required TiDB input |
| P08 — failure drills, runbooks and on-call | Open — 25% | The operator runbook and D01–D09 drill definitions are now implemented; exit still requires timed client/provider/lease/partition/partial-write/restart/restore drills, operator diagnosis and rollback steps, integrity checks, on-call tabletop and acknowledgement |
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

The subsequent public-tip source verification at
`76c2b1a863c23afe71c0591d0a480433e1b9078d` passed the locked offline workspace
tests and strict Clippy with `-D warnings` using a bounded external Cargo
target. This is implementation evidence only; credentialed provider,
production topology, operational, canary and approval gates remain open.

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
