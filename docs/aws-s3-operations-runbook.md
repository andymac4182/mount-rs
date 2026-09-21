# AWS S3 production operations runbook

This runbook is the operational companion to the [W25 production rollout
contract](aws-s3-production-rollout.md). It defines the signals, response
boundaries, and evidence required before an AWS S3-backed mount-rs workload is
admitted to production. The commands and procedures are templates until a
deployment owner, metadata owner, on-call owner, collector, pager, and
approved SLO/RPO/RTO values are recorded.

## Admission checklist

Do not promote a workload until all of the following are attached to the
release record:

- the exact artifact revision and configuration digest;
- the bucket, owned prefix, region, encryption/versioning choice, lifecycle
  policy, and approved runtime and maintenance role ARNs;
- a workload identity test using short-lived credentials, including an
  explicit expiry/rotation check;
- the selected durable metadata topology, writer-fencing result, schema
  migration result, backup/restore result, and failure-recovery result;
- collector, dashboard, alert route, escalation owner, and redaction review;
- canary smoke, rollback, and post-deploy evidence for the same artifact.

The qualification bucket and temporary PGlite server do not satisfy this
checklist. They are test evidence only.

## Signals and alert ownership

The application-owned observability facade emits bounded provider operation
signals when enabled. It does not add object keys, volume names, credentials,
or backend error messages as metric labels.

| Signal | Source | Required production use |
| --- | --- | --- |
| Provider block latency, success, error, read/write bytes | `mount_rs.operations`, `mount_rs.errors`, `mount_rs.operation.duration_ms`, `mount_rs.bytes.*` with `boundary=provider.blocks` | Dashboard p50/p95/p99 and an error-budget alert for the approved workload SLO |
| Orphan scan/protection/recent/delete counts | `mount_rs.blocks.reconcile.*` and `mount_rs.blocks.reconciled` | Alert on failed reconciliation, unexpected growth in recent/unprotected objects, and delete volume outside the approved window |
| S3 gateway request/error/latency classes | bounded `S3Session::stats()` when the gateway is deployed | Alert separately on authentication, conditional conflict, throttling, client, and server classes; never alert on raw object paths |
| Credential expiry and identity | workload identity health check plus AWS identity/audit logs | Page before expiry/rotation failure; verify the account, role, region, and bucket without printing tokens |
| Capacity, retention, and cost | S3 storage/request metrics, lifecycle reports, version inventory, and an approved budget | Alert on retained bytes/versions, incomplete multipart uploads, request-rate anomalies, and budget/headroom thresholds |

The SDK's provider metrics are instrumentation, not a deployed collector or
alert. The underlying object-store client's internal retry behavior must be
measured in the selected deployment or exposed by an approved adapter before
retry SLOs are claimed.

## Required drills

Run each drill against the production-shaped staging topology and retain
timestamps, artifact/config digests, role identity, metrics, logs, cleanup
results, and owner sign-off.

| Drill | Action | Pass condition |
| --- | --- | --- |
| O01 identity expiry | Let the short-lived workload credential approach expiry or rotate it under supervision | Requests fail closed or refresh without data loss; the alert arrives before the expiry budget |
| O02 conditional conflict | Run two independent writers against one metadata volume and one owned S3 prefix | Exactly one fenced writer publishes; the stale writer receives a defined conflict/stale result and cannot overwrite blocks or metadata |
| O03 throttling/server fault | Inject bounded `SlowDown`/5xx or equivalent provider faults | Retries stay within the approved budget, metrics distinguish retryable failure from committed ambiguity, and the runbook gives a safe recovery action |
| O04 metadata outage | Stop or isolate the metadata service during a block publication | The filesystem does not acknowledge an uncertain publication; orphan handling retains the grace window and reports counts |
| O05 restore | Restore metadata from the approved backup into an isolated target and reopen the same block prefix | Namespace references resolve, block integrity matches the pre-drill digest, and RPO/RTO meet the approved values |
| O06 schema migration | Apply the reviewed migration, reopen with the previous and new artifact where supported, then roll back in staging | Older data remains readable, incompatible future schema fails closed, and rollback ownership is explicit |
| O07 cleanup/retention | Run reconciliation and inspect current objects, versions, incomplete uploads, and lifecycle state | Only unreferenced objects older than the approved grace are removed; live/recent objects and sibling prefixes remain untouched |
| O08 canary/rollback | Deploy one canary, observe one full SLO window, then exercise the rollback path | The exact prior artifact and metadata restore procedure return the canary to a known-good read/write state |

No drill is complete from a local unit test alone. The existing local gateway,
provider, and AWS qualification tests prove code and test-account behavior;
they do not prove the selected production collector, pager, backup system,
multi-region recovery, or capacity.

## Incident actions

1. **Authentication or expiry:** stop promotion and new writers if identity
   cannot be verified. Confirm the caller account, role trust, region, and
   bucket policy through the approved audit identity. Rotate or repair the
   workload identity; never add a long-lived key to configuration or logs.
2. **Conditional conflicts or stale writer:** stop the affected writer,
   preserve metadata and the owned prefix, and inspect lease/fence and
   revision evidence. Do not retry an ambiguous metadata publication until
   the authoritative state has been reconciled.
3. **Throttling or provider errors:** use the bounded retry/error dashboards,
   reduce load or pause the canary, and verify whether each operation was
   committed before retrying. Keep the prefix intact for reconciliation.
4. **Orphans or cleanup growth:** suspend destructive cleanup, preserve the
   report and object-version inventory, confirm the metadata snapshot and grace
   period, then run the maintenance role only within its approved prefix.
5. **Metadata loss or corruption:** stop writers, preserve the S3 prefix and
   metadata backup, restore into an isolated target, validate namespace and
   block digests, and resume only after the release owner signs the RPO/RTO
   result.

## Canary and rollback record

Record these fields in the release ticket:

```text
artifact_revision=
configuration_digest=
metadata_topology=
bucket_region_prefix=
runtime_role_arn_redacted=
maintenance_role_arn_redacted=
canary_started_at=
canary_slo_window=
rollback_artifact_revision=
rollback_owner=
smoke_result=
restore_result=
evidence_links=
```

On rollback, stop new writers, preserve the metadata snapshot and S3 prefix,
restore the last known-good artifact/topology, and verify reads, writer
fencing, object ownership, and telemetry before resuming. Never bulk-delete a
prefix as a rollback shortcut.

## Current boundary

The repository currently has local telemetry tests, a reviewable CloudFormation
resource contract, and live qualification-account AWS tests including PGlite
fencing and a local metadata restore/reopen drill. Production collector
wiring, retry measurement, credential-rotation evidence, cost/retention
alerts, approved metadata ownership, staging drills, canary, rollback, and
post-deploy smoke remain open W25 gates.
