# W08 TiDB/RustFS production operations runbook

This runbook is the operational companion to
[`W08-production-rollout.md`](W08-production-rollout.md) and
[`W08-progress-ledger.md`](W08-progress-ledger.md). It covers the production
composition of durable TiDB metadata with immutable S3-compatible blocks and
the Rust, CLI, Node/N-API and native consumers.

It is a controlled template until the selected production environment,
provider identities, monitoring route and on-call owner are recorded. The
current rollout decision remains **NO-GO**. A completed demo, a local check, or
the retained hosted qualification jobs do not count as an executed drill.

The credential-free runbook-shape policy is checked by
`node scripts/verify-w08-production-operations.mjs
tests/tidb/production-operations-policy.json` and its eight-case regression
suite. It validates D01–D09 coverage, evidence-record completeness, incident
closure controls, cleanup/rollback requirements and on-call acknowledgement;
it does not execute a failure injection, page an operator or create an
incident record.

The release admission path is also a controlled template until the GitHub
`w08-production` environment has required reviewers and tag/branch policy. The
`v*-cli-production-candidate*` workflow produces a protected prerelease with
Linux/macOS artifacts, target-specific manifests/SBOMs, aggregate checksums and
verified attestations; it does not by itself execute a canary, rollback or
production GO decision. Record the environment approval and resulting release
asset digests in the evidence record before beginning the canary.

## Evidence record

Create one redacted record for every deployment, drill and incident. Never put
URLs containing passwords, access keys, tokens, certificates or tenant secrets
in this document, tickets or shell history.

| Field | Required value |
| --- | --- |
| Record ID | Incident, change, drill or release identifier |
| Tested revision | Immutable Git revision and artifact digest |
| Configuration | Approved config digest; secret-manager version references only |
| Provider versions | TiDB/PD/TiKV, RustFS or object provider, client/package versions |
| Topology | Region, zones, quorum/replication, storage class and network boundary |
| Environment | Staging, canary or production identifier; never a credential |
| People | Incident commander, operator, release owner and approver |
| Time | Start, detection, acknowledgement, mitigation, recovery and cleanup timestamps |
| Result | Terminal PASS/FAIL, measured RPO/RTO/SLO impact, rollback and cleanup outcome |

## Admission and preflight

Perform these checks before admitting a writer or promoting a release. The
policy verifier is credential-free; the TLS wrapper is the provider-facing
gate and must run only through the approved secret/runtime path.

```sh
# Contract shape only; this must not contain live values in source control.
MOUNT_RS_TIDB_TLS_URL='mysql://<user>@<tidb-host>:4000/<database>?require_ssl=true' \
  node scripts/verify-w08-production-config.mjs /path/to/approved-w08-config.json

# Staging/provider gate; keep the URL and output out of logs and history.
MOUNT_RS_TIDB_TLS_URL="$SECRET_MANAGER_TIDB_TLS_URL" \
MOUNT_RS_TIDB_TLS_TEST_VOLUME_KEY="$SCOPED_TEST_VOLUME_KEY" \
  ./scripts/test-tidb-tls.sh
```

The preflight record must also include:

- for the HTTP transport, `GET`/`HEAD /healthz` returning `200` with
  `{"status":"ok"}` and `/readyz` returning `200` with the configured drive
  count; an empty registry must return `503` with `{"status":"not_ready"}`;
  both responses must include `Cache-Control: no-store` and
  `X-Content-Type-Options: nosniff`; these are listener/process and local
  configuration checks only, not remote TiDB, RustFS, TLS, collector or pager
  health;
- successful TiDB identity/version and readiness checks from the selected
  topology;
- an HTTPS block-store health/read/write check using the runtime identity and
  an owned canary prefix;
- `durable: true` backed by the approved TiKV replication, sync-log, storage,
  backup and failure policy;
- certificate-chain and hostname verification, with no TLS bypass flags;
- a negative authorization test proving a consumer cannot write outside its
  assigned volume/prefix; and
- clean canary cleanup with metadata and block consistency verified.

Do not admit writers if any check is skipped, queued, credential-free,
ambiguous, or only a local schema validation. Do not replay an operation merely
because a client saw an unknown or maybe-committed outcome.

## Normal deployment and promotion

1. The change owner records the evidence fields above and obtains the P01–P09
   approvals required for the target stage.
2. Freeze unrelated schema, provider and client changes. Verify the artifact,
   SBOM/provenance, configuration digest and rollback artifact.
3. Validate the config shape, inject short-lived credentials, run the guarded
   TLS/provider preflight, and record redacted terminal output.
4. Start or verify TiDB/PD/TiKV quorum and block-store health. Confirm the
   readiness, certificate, identity, replication and capacity alerts are
   green.
5. Deploy one held-back canary volume with an owned prefix. Exercise create,
   partial write, truncate, close, reopen and fresh-client readback through
   the advertised consumer surfaces.
6. Observe the agreed SLO/error-budget window. Record p50/p95/p99 latency,
   error rates, fencing/retry signals, resource headroom and alert delivery.
7. Promote in stages only after the release owner records the canary result.
   Keep the previous artifact, metadata snapshot and block recovery procedure
   available until the observation window ends.

## Immediate incident actions

For every incident:

1. Declare the incident and assign an incident commander. Record the first
   observed revision, provider state and affected volume/prefix.
2. Stop new writers and promotion. Preserve metadata, block prefixes, logs,
   metrics, traces and provider error identifiers. Do not bulk-delete objects.
3. Separate the failure domain: metadata, block store, client/transport,
   credentials/TLS, network partition, capacity, or release artifact.
4. Apply the matching response below. Use read-only or isolated checks until
   ownership, fencing and data integrity are established.
5. Verify fresh-client reads, lease/fence state, block hashes/manifests and
   cleanup boundaries before resuming writes.
6. Record detection/acknowledgement/recovery times, data loss or replay risk,
   rollback outcome and follow-up action. The incident is not closed on
   service readiness alone.

## Failure response matrix

| Signal or failure | First response | Recovery and integrity checks | Do not do |
| --- | --- | --- | --- |
| TiDB frontend, PD or TiKV unavailable | Stop writers; inspect quorum, readiness, network and certificate errors; retain the affected volume key | Restore the approved quorum/topology, rerun identity/provider checks, reopen with a fresh client and verify fences plus committed metadata | Downgrade the topology or treat `SELECT 1` as durability proof |
| Block provider unavailable or object errors | Stop new publication; preserve the owned prefix and metadata snapshot; check endpoint, region, quota and authz | Restore provider access, verify object hashes and chunk manifests, then run a fresh-provider readback | Delete/recreate the prefix or replay unknown writes blindly |
| `EAGAIN`, `ESTALE`, lease expiry or stale writer | Fence the old process/owner and isolate its credentials; do not weaken TTL or retry across ownership | Acquire the approved owner, reopen the provider, verify the lease/fence record and perform a read-only consistency check | Bypass fencing, reuse an old owner, or turn an unknown result into success |
| Maybe-committed/ambiguous commit | Stop the caller’s replay loop and preserve the request identifier/revision | Reconcile by reading the authoritative metadata and block manifest; retry only under the provider’s idempotency/fencing contract | Blindly replay a mutation that may already be durable |
| Network partition or split-brain suspicion | Stop writers on both sides and preserve both evidence sets | Restore one approved authority, verify ownership and monotonic provider time, then admit a fresh client | Promote both partitions or infer safety from process liveness |
| Partial object/chunk or hash mismatch | Quarantine the canary/prefix and stop publication; retain metadata and object versions | Compare manifest, chunk hashes and metadata revision; restore a consistent snapshot or run the approved repair procedure | Delete the only copy or repair by guessing the intended bytes |
| Certificate, secret or IAM failure | Stop writes; use the secret manager/certificate rotation procedure; preserve redacted error evidence | Verify new chain, hostname, role/prefix permissions and revocation of the old identity; rerun the guarded provider gate | Disable TLS, hostname verification, CA checks or least privilege |
| Capacity, quota or disk-pressure alert | Stop promotion and reduce workload only under the approved traffic policy | Restore headroom, verify no partial publication, record resource growth and update the capacity envelope | Claim production readiness from a short smoke test or silently overcommit capacity |
| Bad client or provider release | Stop promotion and new writers; preserve the artifact/config digest | Roll back to the retained artifact/topology, verify reads, fences, ownership and SLOs, then obtain release-owner approval | Downgrade retained data without a compatibility decision |

## Backup, restore and rollback procedure

The metadata and immutable blocks are one logical filesystem state. A backup is
not complete unless it records the matching TiDB metadata snapshot/revision and
the corresponding block prefix/object versions, retention, encryption and
integrity hashes.

1. Quiesce writers and record the last committed revision and block manifest.
2. Create encrypted metadata and block backups under the approved retention
   policy. Record backup IDs, key IDs and consistency timestamps, not secrets.
3. Restore into an isolated environment with separate credentials and no
   production writer access.
4. Verify schema, volume ownership, provider-clock/fence state, chunk hashes,
   truncation boundaries and fresh-client reads.
5. Measure restore duration and recovered point. Compare the result with the
   approved RPO/RTO and record missing/partial-object behavior.
6. For a release rollback, stop writers, preserve the failed revision and
   restore the last known-good artifact/config. Re-run admission checks,
   ownership/fencing checks and read-only canary before reopening writes.

No backup, restore or rollback result is recorded for W08 until this procedure
has been executed against the named staging/production provider and signed by
the data owner and release owner.

## Timed drill matrix

Every row requires a named owner, a terminal result, timestamps, redacted logs,
integrity checks and cleanup/rollback outcome. The current status of all rows
is **Not executed — external production gate**.

| Drill | Failure injected | Required signal | Recovery evidence | Gate |
| --- | --- | --- | --- | --- |
| D01 | TiDB frontend loss and replacement | Readiness/error alert; no unsafe writer admission | Fresh client, persisted metadata and fencing checks | P03/P05/P08 |
| D02 | TiKV or PD member loss/quorum pressure | Provider health and capacity alert | Replication/quorum recovery, restart/reopen and measured RTO | P01/P03/P08 |
| D03 | Block endpoint outage or 5xx/timeout burst | Block error/latency alert | Scoped retry/recovery, manifest/hash verification and no orphan leak | P03/P05/P08 |
| D04 | Stale lease/old writer | `ESTALE`/fencing alert | Old identity fenced; new owner and fresh-client readback | P02/P08 |
| D05 | Network partition or split-brain suspicion | Partition/ownership alert | One authority restored; no divergent publication; integrity check | P01/P05/P08 |
| D06 | Partial object or interrupted publication | Manifest/hash/integrity alert | Consistent restore or approved repair; no guessed replay | P03/P08 |
| D07 | Certificate expiry or credential revocation | TLS/IAM alert | Rotation, revocation, redacted audit and successful guarded provider test | P02/P07/P08 |
| D08 | Clean-environment backup restore | Restore job and RPO/RTO alert | Metadata/block consistency, fresh reads and measured RPO/RTO | P03/P08 |
| D09 | Bad client/provider deployment | Release/canary alert | Artifact rollback, read/fence/ownership checks and approval | P04/P09 |

## Observability and alert handoff

The following signals must be mapped to the selected production collector and
pager before P05/P08 can close. Metric names are placeholders until the
collector contract is approved; this table is not evidence that the signals
are currently emitted.

| Signal | Minimum alert condition | Operator action |
| --- | --- | --- |
| Metadata availability/readiness | Failed readiness or sustained provider errors | Stop writers; follow TiDB/quorum response |
| Metadata and block latency | p95/p99 above the approved SLO or error-budget burn | Hold promotion; inspect provider, network and capacity |
| Fencing/lease errors | Unexpected `EAGAIN`/`ESTALE` or renewal failures | Fence suspect owner; investigate split brain |
| Ambiguous commits | Any unclassified maybe-committed outcome | Stop replay; reconcile authoritative state |
| Block integrity/cleanup | Hash mismatch, orphan growth or failed owned cleanup | Quarantine prefix; follow restore/integrity procedure |
| TLS/IAM failures | Certificate, hostname, CA or authorization failure | Rotate through approved path; never weaken verification |
| Resource/headroom | CPU, memory, disk, quota or replication headroom below threshold | Stop promotion; apply capacity runbook |

## Gate handoff checklist

| Gate | Runbook evidence required | Current status |
| --- | --- | --- |
| P01/P02 | Approved topology, identities, config digest and secret/certificate rotation record | Open — external |
| P03 | Executed D08 restore with measured RPO/RTO and metadata/block consistency | Open — external |
| P04 | D09 plus version-matrix and interrupted-upgrade rollback record | Open — external |
| P05 | Collector/dashboard/pager links and exercised alert timestamps | Open — external |
| P06 | Production-shaped load/soak report with headroom and cost limits | Open — external |
| P07 | Credentialed TLS handshake, certificate rotation, threat/security review | Open — external |
| P08 | D01–D08 timed drills, named on-call acknowledgement and incident records | Open — external |
| P09 | Signed artifact/SBOM, canary SLO window, rollback and release-owner approval | Open — external |

The release remains **NO-GO** until the applicable rows have terminal evidence
from the approved environment and named owner sign-off. Update the ledger and
tracker with exact run IDs and timestamps after each execution; do not convert
this template into a PASS by filling in plans or expected outcomes.
