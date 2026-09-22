# W07 FoundationDB/RustFS production operations runbook

This runbook is the operational companion to
[`foundationdb-production-rollout.md`](foundationdb-production-rollout.md)
and the W07 section of [`WORK_TRACKER.md`](../WORK_TRACKER.md). It covers
FoundationDB metadata fenced by a protected shared provider-time authority,
RustFS/S3-compatible immutable blocks, and the Rust, CLI, Node/N-API and
native consumers.

It is a controlled template until the selected production-like environment,
provider identities, monitoring route and on-call owner are recorded. The
current rollout decision remains **NO-GO**. A demo, local Docker run or hosted
qualification job is not an executed production drill.

## Evidence record

Create one redacted record for every deployment, drill and incident. Never put
URLs containing passwords, access keys, tokens, certificates, cluster-file
contents or tenant secrets in this document, tickets or shell history.

| Field | Required value |
| --- | --- |
| Record ID | `recordId`: incident, change, drill or release identifier; unique within the packet |
| Tested revision | `revision`: immutable Git revision and artifact digest |
| Configuration | `configuration`: approved config digest; secret-manager and certificate version references only |
| Provider versions | FoundationDB server/client, RustFS/S3 provider, Rust/Node/CLI/N-API versions |
| Topology | Region, zones, coordinators, replication/storage policy and network boundary |
| Authority | `authority`: authority prefix, writer identity reference and consumer identity reference; never credentials |
| Environment | Staging, canary or production identifier; never a secret |
| People | `people`: incident commander, operator, release owner and approver |
| Time | `timestamps.startedAt`, `completedAt` and `cleanedUpAt` as ISO-8601 timestamps in non-decreasing order; incident detection/acknowledgement/mitigation/recovery times remain in the linked record |
| Result | `result`: terminal PASS/FAIL, measured RPO/RTO/SLO impact, rollback and cleanup outcome |

The machine-readable packet verifier requires every evidence record to carry
all of the fields named above, plus a concrete topology, environment, test/run
ID, terminal status, owner, cleanup outcome, rollback outcome and evidence
reference. It rejects placeholders, malformed timestamps, reversed lifecycle
times and duplicate `recordId` values. This is a fail-closed tracking control;
it does not authenticate the referenced environment or owner.

## Admission and preflight

Run these checks before admitting a writer or promoting a release. The config
policy is credential-free. The provider gate must run only through the
approved secret/runtime path against the named staging topology.

```sh
# Contract shape only; this fixture contains no live endpoint or credential.
node scripts/verify-w07-production-config.mjs \
  /path/to/approved-w07-config.json

# Static CLI schema validation; this does not open a provider.
mount-rs validate-config --config /path/to/approved-w07-config.json

# Qualification harness shape; configure the real staging environment out of
# band and retain its redacted terminal log and exact revision.
MOUNT_RS_FOUNDATIONDB_TOPOLOGY=durable \
MOUNT_RS_FOUNDATIONDB_NAPI=1 \
MOUNT_RS_FOUNDATIONDB_NATIVE_CLI=1 \
  ./scripts/test-foundationdb.sh
```

The preflight record must additionally include:

- the approved multi-process FoundationDB topology, replication/storage and
  backup policy, with clean-client and restart readiness;
- exactly one write-capable authority identity for each authority prefix and
  read-only consumer identities for storage workers;
- a negative authorization test proving a consumer cannot publish or overwrite
  the authority record or access another volume prefix;
- authority host clock-skew monitoring, publication cadence shorter than the
  shortest lease TTL, restart republish and reviewed failover behavior;
- HTTPS certificate-chain and hostname verification for RustFS/S3 blocks,
  short-lived secret injection/rotation and redacted diagnostics;
- clean canary cleanup with metadata, authority and block-prefix consistency;
  and
- green cluster, authority-age, lease/fence, transaction-retry,
  maybe-committed, block-error, latency, capacity and cleanup alerts.

Do not admit writers if any check is skipped, queued, credential-free,
ambiguous or only a local schema validation. Do not replay a mutation merely
because a client saw an unknown or maybe-committed outcome.

## Normal deployment and promotion

1. Record the evidence fields above and obtain the P0–P14 approvals required
   for the target stage.
2. Freeze unrelated schema, provider and client changes. Verify the artifact,
   SBOM/provenance, configuration digest and retained rollback artifact.
3. Validate the config shape, inject short-lived credentials, publish the
   authority sample, run the guarded provider/consumer checks and retain only
   redacted terminal output.
4. Confirm FoundationDB quorum, replication/storage health, RustFS endpoint,
   certificate/identity policy and all capacity alerts are green.
5. Deploy one held-back canary volume with an owned authority and block
   prefix. Exercise create, multi-chunk write, partial write, truncate,
   close, reopen, fresh-client readback and cleanup through every advertised
   consumer surface.
6. Observe the approved SLO/error-budget window. Record p50/p95/p99 latency,
   throughput, error/retry rates, fencing signals, resource headroom and alert
   delivery.
7. Promote in stages only after the release owner records the canary result.
   Keep the prior artifact, metadata snapshot and block recovery procedure
   available until the observation window ends.

## Immediate incident actions

For every incident:

1. Declare the incident and assign an incident commander. Record the first
   observed revision, authority prefix, volume prefix and provider state.
2. Stop new writers and promotion. Preserve metadata, authority samples,
   block prefixes, logs, metrics, traces and provider error identifiers. Do
   not bulk-delete objects.
3. Separate the failure domain: FoundationDB quorum/storage, authority,
   RustFS/S3 blocks, client/transport, credentials/TLS, network partition,
   capacity or release artifact.
4. Apply the matching response below. Use read-only or isolated checks until
   ownership, fencing and data integrity are established.
5. Verify fresh-client reads, lease/fence state, authority freshness, block
   hashes/manifests and cleanup boundaries before resuming writes.
6. Record detection/acknowledgement/recovery times, data-loss or replay risk,
   rollback outcome and follow-up action. Service readiness alone does not
   close the incident.

## Failure response matrix

| Signal or failure | First response | Recovery and integrity checks | Do not do |
| --- | --- | --- | --- |
| FoundationDB coordinator, process or storage-node loss | Stop writers; inspect quorum, replication, storage and network health; retain the volume key | Restore the approved quorum/topology, rerun readiness and identity checks, reopen with a fresh client and verify fences plus committed metadata | Downgrade to a single node or treat a successful connection as durability proof |
| Shared authority unavailable or stale | Stop new writers; inspect authority publication age, host clock and failover state | Publish through the approved authority/failover, verify monotonic time and admit readers only after a fresh sample | Substitute a consumer’s local clock, extend leases blindly or bypass the shared oracle |
| RustFS/S3 endpoint outage or object errors | Stop new publication; preserve the owned prefix and metadata snapshot; check endpoint, region, quota and authorization | Restore provider access, verify object hashes/chunk manifests and run a fresh-client readback | Delete/recreate the prefix or replay unknown writes blindly |
| `EAGAIN`, `ESTALE`, lease expiry or stale writer | Fence the old process/identity and isolate its credentials | Acquire the approved owner, verify the authority/fence record and perform a fresh-client consistency check | Bypass fencing, reuse an old owner or weaken the TTL |
| Maybe-committed/ambiguous transaction | Stop the caller’s replay loop and preserve request/revision identifiers | Reconcile by reading authoritative metadata and block manifests; retry only under the idempotency/fencing contract | Treat an unknown result as failure and blindly replay the mutation |
| Network partition or split-brain suspicion | Stop writers on every affected side and preserve both evidence sets | Restore one approved authority, verify ownership and monotonic provider time, then admit a fresh client | Promote both partitions or infer safety from process liveness |
| Partial object/chunk or hash mismatch | Quarantine the canary/prefix and stop publication | Compare manifest, chunk hashes and metadata revision; restore a consistent snapshot or use the approved repair procedure | Delete the only copy or guess the intended bytes |
| Certificate, secret or IAM failure | Stop writes; use the secret-manager/certificate rotation procedure | Verify chain, hostname, role/prefix permissions and old-identity revocation; rerun the credentialed gate | Disable TLS, hostname verification, CA checks or least privilege |
| Capacity, quota or disk-pressure alert | Stop promotion and reduce workload only under the approved traffic policy | Restore headroom, verify no partial publication, record resource growth and update the capacity envelope | Claim production readiness from a short smoke test |
| Bad client/provider release | Stop promotion and new writers; preserve artifact/config digests | Roll back to the retained artifact/topology, verify reads, fences, ownership and SLOs, then obtain release-owner approval | Downgrade retained data without a compatibility decision |

## Backup, restore and rollback procedure

Metadata, the authority record and immutable blocks form one logical filesystem
state. A backup is incomplete unless it records the matching metadata revision,
authority sample/fence state and block prefix/object versions, retention,
encryption and integrity hashes.

1. Quiesce writers and record the last committed revision, authority sample
   and block manifest.
2. Create encrypted metadata, authority and block backups under the approved
   retention policy. Record backup IDs, key IDs and consistency timestamps,
   never secrets.
3. Restore into an isolated environment with separate credentials and no
   production writer access.
4. Verify the cluster/keyspace version, volume ownership, authority/fence
   state, chunk hashes, truncation boundaries and fresh-client reads.
5. Measure restore duration and recovered point. Compare the result with the
   approved RPO/RTO and record missing/partial-object behavior.
6. For a release rollback, stop writers, preserve the failed revision and
   restore the last known-good artifact/config. Rerun admission,
   ownership/fencing and read-only canary checks before reopening writes.

No backup, restore or rollback result is recorded for W07 until this procedure
has executed against the named staging/production provider and has data-owner
and release-owner sign-off.

## Timed drill matrix

Every row requires a named owner, terminal result, timestamps, redacted logs,
integrity checks and cleanup/rollback outcome. The current status of every row
is **Not executed — external production gate**.

| Drill | Failure injected | Required signal | Recovery evidence | Gate |
| --- | --- | --- | --- | --- |
| D01 | FoundationDB process/storage-node loss | Cluster/quorum/replication alert; no unsafe writer admission | Fresh-client reopen, committed metadata and fencing checks | P1/P4/P5/P13 |
| D02 | Shared authority loss or stale publication | Authority-age/error alert; consumers fail closed | Approved failover/republish, monotonic sample and measured recovery | P3/P5/P7/P13 |
| D03 | RustFS/S3 outage or timeout burst | Block error/latency alert | Scoped recovery, manifest/hash verification and no orphan leak | P3/P5/P7/P13 |
| D04 | Stale writer/expired lease | `ESTALE`/fencing alert | Old identity fenced; new owner and fresh-client readback | P3/P5/P13 |
| D05 | Network partition/split-brain suspicion | Partition/ownership alert | One authority restored; no divergent publication; integrity check | P1/P3/P5/P13 |
| D06 | Interrupted publication or partial block | Manifest/hash/integrity alert | Consistent restore or approved repair; no guessed replay | P4/P5/P6/P13 |
| D07 | Certificate expiry or credential revocation | TLS/IAM alert | Rotation, revocation, redacted audit and credentialed provider pass | P3/P7/P13 |
| D08 | Clean-environment backup restore | Restore job and RPO/RTO alert | Metadata/authority/block consistency, fresh reads and measured RPO/RTO | P4/P6/P13 |
| D09 | Bad client/provider deployment | Release/canary alert | Artifact rollback, read/fence/ownership checks and approval | P9/P12/P13/P14 |

## Observability and alert handoff

The following signals must be mapped to the selected collector and pager
before P7/P13 can close. Metric names are placeholders until the production
telemetry contract is approved; this table is not evidence that the signals
are currently emitted.

The FoundationDB integration now exposes bounded process-local snapshots from
`FoundationDbLeaseAuthority::stats()` and
`FoundationDbSharedLeaseOracle::stats()`. The authority snapshot supplies
publication attempts/successes/failures, the last persisted provider-time
sample and local observation timestamps; the reader snapshot supplies
authority-read attempts/successes/failures, the last observed provider time
and local observation timestamps. An embedding service may map these to the
authority-age/error and reader-failure signals below. They are implementation
inputs only: counters reset with a new handle, timestamps are diagnostic
observations rather than lease time, and P7 remains open until a named
collector/pager receives and exercises the signals in the approved
production-like environment.

After each authority publication attempt and shared-authority read, the
FoundationDB integration emits a bounded structured event through the
embedding application's `tracing` subscriber. The stable event target is
`mount_rs.foundationdb.authority`; publication events use
`event_name=mount_rs.foundationdb.lease_authority` and `operation=publish`,
while reader events use `event_name=mount_rs.foundationdb.lease_oracle` and
`operation=read`. Each event carries only the fixed `outcome` label, attempt /
success / failure counters and last provider-time/local-observation
timestamps; zero means that an observation has not happened in that handle.
There are no cluster paths, key prefixes, credentials or provider error
messages in the event. An application that installs the repository's OTLP
subscriber can route these events to its approved collector, but event
emission alone is not a dashboard, alert route, clock monitor or exercised
production gate.

The qualification-log verifier fails closed unless the retained hosted log
contains both `FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING` and
`FOUNDATIONDB_AUTHORITY_STATS_PASS`. It checks that the heartbeat cadence and
forward-jump bound match the lease-publication policy and that publication and
reader attempts reconcile with their success/failure counters. This protects
the qualification evidence packet from silently losing the new authority
telemetry assertions; it is still not deployed collector or pager evidence.

| Signal | Minimum alert condition | Operator action |
| --- | --- | --- |
| FoundationDB availability/quorum | Failed readiness, replication or sustained provider errors | Stop writers; follow the quorum/storage response |
| Authority publication age | Missing, stale or failed samples; clock-skew bound exceeded | Fail closed; inspect authority/failover and clock policy |
| Metadata/block latency and errors | p95/p99 above SLO or error-budget burn | Hold promotion; inspect provider, network and capacity |
| Fencing and lease errors | Unexpected `EAGAIN`/`ESTALE` or renew failures | Fence the suspect owner; investigate split brain |
| Ambiguous commits | Any unclassified maybe-committed outcome | Stop replay; reconcile authoritative state |
| Block integrity/cleanup | Hash mismatch, orphan growth or failed owned cleanup | Quarantine prefix; follow restore/integrity procedure |
| TLS/IAM failures | Certificate, hostname, CA or authorization failure | Rotate through the approved path; never weaken verification |
| Resource/headroom | CPU, memory, disk, quota or replication headroom below threshold | Stop promotion; apply the capacity runbook |

## Gate handoff checklist

| Gate | Runbook evidence required | Current status |
| --- | --- | --- |
| P0/P1/P2 | Approved topology, support matrix, config digest, SLO/RPO/RTO, owners and provider matrix | Open — external |
| P3 | Actual authority/consumer identities, ACL-negative test, TLS and secret rotation record | Open — external |
| P4/P6 | Executed D08 restore with measured RPO/RTO and metadata/authority/block consistency | Open — external |
| P5 | D01–D06 failure/fencing/recovery records with no split-brain publication | Open — external |
| P7 | Collector/dashboard/pager links and exercised alert timestamps | Open — external |
| P8 | Production-shaped load/soak report with p50/p95/p99, headroom and cost limits | Open — external |
| P9/P12 | Upgrade/rollback, signed artifact/SBOM, canary SLO window and rollback result | Open — external |
| P10/P11 | Security review, advertised platform matrix and terminal native/package evidence | Open — external |
| P13/P14 | Timed incident records, known limitations and release-owner GO/NO-GO | Open — external |

The release remains **NO-GO** until the applicable rows have terminal evidence
from the approved environment and named owner sign-off. Update the ledger and
tracker with exact run IDs and timestamps after each execution; do not convert
this template into a PASS by filling in plans or expected outcomes.
