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
| Current implementation capability | TLS-capable provider, Rust SDK, CLI, N-API, a guarded TLS acceptance wrapper, a fail-closed production-config policy verifier and bounded HTTP `/healthz`/`/readyz` probes are implemented; local unit/Clippy, CLI-schema, policy and hosted compile/guard checks are tracked separately |
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
| P05 — observability, SLOs and alerting | Open — 15% gate weight; local/hosted baseline passed; hardening requalification open | The HTTP transport's unauthenticated `/healthz` and `/readyz` contract, `Cache-Control: no-store` and `X-Content-Type-Options: nosniff` are locally tested; the baseline all-features hosted contract passed, but exit still requires a current hosted hardening pass, provider-aware checks where applicable, production collector, dashboards, SLO/error-budget thresholds, paging, retention/redaction and an exercised alert route |
| P06 — capacity, load and soak | Open — 20% | The bounded TiDB/RustFS soak harness is configured in hosted composition CI and passed its current seed/reopen qualification at 64 operations, concurrency 8 and 65,536-byte payloads; exit still requires representative workload baseline/peak/saturation/failover/soak results with p50/p95/p99 latency, throughput, errors, resource growth, headroom, scaling and cost limits |
| P07 — security, transport and hardening | Open — 20% | TLS and certificate rotation, network segmentation, authz/tenant isolation, dependency/image/SBOM review, threat-model findings, audit checks and a credentialed TLS handshake; local policy validation requires HTTPS blocks and TLS-required TiDB input |
| P08 — failure drills, runbooks and on-call | Open — 25% | The operator runbook and D01–D09 drill definitions are now implemented; exit still requires timed client/provider/lease/partition/partial-write/restart/restore drills, operator diagnosis and rollback steps, integrity checks, on-call tabletop and acknowledgement |
| P09 — release provenance, canary and go/no-go | Open — 10% | Immutable signed artifacts, SBOM/provenance, target-platform verification, staged canary, live SLO window, rollback result and explicit release-owner approval |

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
| `./scripts/cargo-shared test --locked -p mount-rs-http --all-features` plus strict Clippy on source `2159976` | Local response-header hardening: health/readiness cache-control and `nosniff` assertions pass with 8 unit and 13 integration tests | Current hosted requalification, deployed collector/exporter, provider/object-store/TLS health, dashboard/pager, SLO/error-budget approval and production alert execution |

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
5. Close P09 with immutable artifacts, signatures/SBOM, a held-back canary,
   live SLO observation, rollback verification and release-owner approval.
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
