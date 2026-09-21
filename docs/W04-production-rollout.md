# W04 PGlite production rollout and operations runbook

This is the controlled rollout companion to
[`w04-progress-ledger.md`](w04-progress-ledger.md). It translates the W04
PGlite implementation and hosted qualification into production admission,
backup, recovery, rollback, and ownership evidence. It is a template until
the target deployment, data owner, operator, collector, and release owner are
recorded. The current decision is **NO-GO**.

The successful demo and the hosted W04.2 recovery jobs prove useful behavior;
they do not prove that a production data directory is persistent, that a
backup can be restored, that a previous artifact can be rolled back safely, or
that an operator will receive and act on the relevant signals.

## Scope and launch boundary

Record the advertised launch scope before executing the gates. A provider or
deployment shape that is not named here is out of scope, not implicitly
accepted.

| Field | Required decision or evidence | Current value |
| --- | --- | --- |
| Release identifier | Change/release ticket and immutable artifact revision | Not recorded — external release gate |
| Git revision | Published revision tested by every required gate | Not recorded for a production release |
| Consumer surfaces | Rust SDK, Node/N-API, CLI, and native packages actually advertised | Not recorded — release-owner decision |
| PGlite version | Pinned version and compatibility policy for existing data | Not recorded — deployment-owner decision |
| Data directory | Persistent volume, mount mode, capacity, ownership, and encryption | Not recorded — deployment-owner decision |
| Metadata/block composition | Exact PGlite metadata and block-store arrangement | Not recorded — deployment-owner decision |
| Provider scope | PGlite-only, or named external providers with real credentials/services | Provisional PGlite-only; owner confirmation required |
| Backup/restore target | Backup IDs, retention, encryption/key references, and isolated restore location | Not executed |
| Observability route | Collector, dashboard, alert policy, pager, and log/trace retention | Not connected |
| Operators | Incident commander, operator, data owner, and release approver | Not assigned |

Do not promote a demo, local filesystem, ephemeral container volume, provider
mock, credential-free policy check, or skipped hosted job into a production
pass. If the launch is PGlite-only, explicitly record the excluded provider
rows and their non-goals; if an external provider is advertised, execute that
provider's real identity, durability, cleanup, restart, and recovery gates.

## Evidence record

Create one redacted record for each staging rehearsal, canary, deployment,
backup/restore drill, rollback drill, and incident. Keep secrets, access keys,
tokens, certificates, private URLs, and customer data out of this document,
tickets, shell history, and CI logs.

| Field | Required value |
| --- | --- |
| Record ID | Change, release, drill, or incident identifier |
| Tested revision | Immutable Git revision and package/native artifact digests |
| Configuration | Approved configuration digest and secret-manager version references only |
| PGlite/provider versions | PGlite, runtime, package, and any named provider versions |
| Persistence shape | Volume identity, filesystem, mount options, capacity and encryption reference |
| Environment | Staging, canary, or production identifier; never a credential |
| People | Data owner, operator, incident commander, release owner and approver |
| Time | Start, detection, acknowledgement, mitigation, recovery and cleanup timestamps |
| Result | Terminal PASS/FAIL, measured RPO/RTO/SLO impact, rollback and cleanup outcome |
| Evidence links | Redacted logs, metrics, traces, backup IDs, manifests and review record |

## Admission and preflight

The release owner must hold the rollout if any required row is queued, skipped,
credential-free, ambiguous, or only locally validated.

1. Pin the Git revision, root package, platform packages, native artifact
   digests, lockfile, PGlite version, and approved configuration digest.
2. Confirm that the production data directory is a durable, backed-up volume,
   survives process/container replacement, is not an ephemeral workspace, and
   has the expected ownership, capacity, filesystem, and encryption controls.
3. Run the published-revision hosted W04.2 recovery gate on every advertised
   Node platform and retain the exact `Verify PGlite integration and restart
   recovery` step result. A skipped step is not a pass.
4. Install the exact release package in a clean consumer and exercise the
   advertised write, read, shutdown, restart, fresh-client readback, and
   cleanup path. Retain the package manifest and artifact digests.
5. Validate the production configuration without opening a provider connection,
   then run the real credentialed provider gate if any provider other than the
   bounded PGlite-only scope is advertised. Use short-lived credentials and
   record only secret-manager references.
6. Verify backup freshness, restore permissions, alert routing, dashboard
   links, log/trace retention, capacity headroom, and the named on-call route.
7. Obtain a written GO decision for the target stage. Until then, the release
   is **NO-GO** even when the implementation and demo are green.

## Staged rollout

1. Freeze unrelated schema, provider, package, and configuration changes.
2. Capture the pre-deploy evidence record, backup reference, configuration
   digest, artifact digests, and retained previous release.
3. Deploy one held-back canary volume with an owned test prefix/data directory.
4. Exercise create/write/truncate/close, process restart, fresh-client readback,
   version compatibility, cleanup, and the advertised consumer surfaces.
5. Observe the approved error-budget window. Record latency percentiles,
   errors, restart time, resource headroom, cleanup results, and alert delivery.
6. Promote in small stages only after the canary result and observation window
   are reviewed by the release owner. Keep the previous artifact, compatible
   configuration, and restore procedure available until the window closes.

## Backup, restore, and rollback

PGlite metadata and its block/data files are one logical filesystem state. A
backup is incomplete unless it records the matching PGlite metadata/version
state, data directory contents or provider manifest, encryption/key reference,
retention, and consistency timestamp.

1. Quiesce writers and record the last committed operation/version boundary.
2. Create the approved encrypted backup and record only its identifier,
   integrity manifest, key reference, and consistency timestamp.
3. Restore into an isolated directory or environment with separate credentials
   and no production writer access.
4. Verify schema/version compatibility, file/block integrity, ownership and
   fencing state, truncation boundaries, fresh-client reads, and cleanup.
5. Measure restore duration and the recovered point; compare them with the
   approved RPO/RTO. Record any missing or partial data explicitly.
6. For a release rollback, stop new writers, preserve the failed revision and
   current evidence, restore the last known-good artifact/configuration, and
   rerun admission, ownership, restart, and read-only canary checks before
   reopening writes.

Never blind-replay a request with an unknown or maybe-committed outcome, delete
the only copy during cleanup, or downgrade a persisted data directory without
an explicit compatibility decision and restore point.

## Signals, limits, and operator response

These are required mappings to the selected collector and pager. They are not
evidence that the signals are currently emitted.

| Signal | Minimum admission/alert condition | Operator action |
| --- | --- | --- |
| PGlite readiness/open failures | Any sustained open/reopen failure or failed canary | Stop promotion; preserve the data directory and inspect version/permissions |
| Restart recovery | Recovery exceeds the approved RTO or fresh-client readback differs | Stop writers; quarantine the canary and follow restore procedure |
| Metadata/data integrity | Hash, manifest, size, truncation, or ownership mismatch | Stop publication; preserve both metadata and data evidence; do not guess-repair |
| Fencing/ownership | Unexpected stale-owner, lease, or concurrent-writer signal | Fence the suspect process and require a fresh owner/read-only check |
| Latency/error budget | Approved p95/p99 or error-budget threshold is breached | Hold promotion and inspect runtime, filesystem, provider and capacity |
| Disk/capacity | Volume, inode, quota, or backup headroom below the approved limit | Stop promotion; apply capacity/backup procedure before admitting writers |
| Backup/restore | Backup freshness, integrity, or restore drill is missing/failed | Keep NO-GO; do not treat a live volume as a backup |
| Package/artifact | Digest mismatch, missing optional native package, or consumer smoke failure | Stop release and restore the retained package |

The owner must replace each provisional threshold with a measured value and
link the collector alert, dashboard panel, and pager test before closing the
operational gate.

## Controlled drill matrix

Every row requires a named owner, terminal result, timestamps, redacted logs,
integrity checks, cleanup, and rollback outcome. Current status is
**Not executed — external production gate**.

| Drill | Failure injected | Required evidence |
| --- | --- | --- |
| D01 durable restart | Stop/restart the PGlite process or deployment while the volume remains attached | Fresh-client readback, version compatibility, ownership/fencing, measured recovery time |
| D02 clean backup restore | Restore the approved backup into an isolated environment | Integrity manifest, schema/version check, fresh reads, measured RPO/RTO |
| D03 bad artifact rollback | Deploy a deliberately held-back incompatible release candidate | Writers stopped, previous artifact/config restored, reads and ownership verified |
| D04 disk/capacity pressure | Exercise the approved capacity threshold without corrupting the canary | Alert delivery, admission hold, cleanup/restore result, headroom measurement |
| D05 concurrent-owner fault | Leave an old process or stale owner behind during reopen | Old owner fenced, one current owner, no divergent publication |
| D06 alert/pager path | Trigger a non-destructive readiness or error-budget condition | Collector event, pager acknowledgement, operator response timestamps |

## Release decision and sign-off

The W04 production decision remains **NO-GO** until the ledger links terminal
evidence for the following rows on the same advertised release scope:

- [ ] persistent production PGlite data directory and version policy;
- [ ] backup, isolated restore, RPO/RTO, and rollback drill;
- [ ] provider scope and real provider evidence for every advertised provider;
- [ ] artifact/package digest and clean-consumer validation;
- [ ] observability, limits, alert delivery, and incident runbook;
- [ ] named data owner, operator/on-call, and release approver; and
- [ ] written GO/NO-GO review against all of the above.

| Role | Name/identifier | Decision | Date | Evidence link |
| --- | --- | --- | --- | --- |
| Data owner | Not assigned | Pending | — | — |
| Operator/on-call | Not assigned | Pending | — | — |
| Release owner | Not assigned | Pending | — | — |
| Approver | Not assigned | Pending | — | — |

