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

## Machine-readable release gate summary

This compact table is the authoritative release-control surface for the
repository policy check. It tracks the production decision separately from
the W04 implementation checkboxes: a closed recovery or package gate does not
close the deployment, provider, operations, ownership, or release gates.

| Gate | Status | Boundary |
| --- | --- | --- |
| Production rollout | **NO-GO** | No production release or customer deployment is authorized |
| P01 hosted recovery qualification | CLOSED | Required Node recovery and restart evidence is terminal and exact-step verified |
| P02 artifact/package validation | CLOSED | Native package aggregation and clean-consumer smoke are terminally green |
| P03 persistent deployment and version policy | OPEN | The real volume, compatibility policy, and restart behavior are not recorded |
| P04 backup, restore, and rollback | OPEN | Production backup identity, isolated restore, RPO/RTO, and rollback drill are not executed |
| P05 provider scope and durability | OPEN | Launch scope and real provider evidence are not approved for every advertised provider |
| P06 observability and runbook | OPEN | Collector, alerts, limits, pager path, and operational drills are not connected and evidenced |
| P07 ownership and release approval | OPEN | Named data/operator/release owners and written GO/NO-GO review are pending |

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

## Current candidate qualification (supporting evidence, not production approval)

The current published candidate is `d870f900370fe5a7b4235ac7e63f8c3b33efce99`.
Manual qualification run
[35692153251](https://github.com/andymac4182/mount-rs/actions/runs/35692153251)
completed with the required macOS-latest, macOS-15-intel, Ubuntu, and ARM Node
early-rejection and PGlite/restart steps green. Its aggregate-native job
[106634465987](https://github.com/andymac4182/mount-rs/actions/runs/35692153251/job/106634465987)
passed artifact aggregation, all five native package validations, and clean
consumer install/smoke. The run itself is terminal `failure` because provider
capacity/W26 lanes failed, so this is not a production release record or GO
decision.

The credential-free production-shape workflow also passed on shared head
`25e275ab`: run
[35694307118](https://github.com/andymac4182/mount-rs/actions/runs/35694307118),
job
[106637712246](https://github.com/andymac4182/mount-rs/actions/runs/35694307118/job/106637712246),
emitted the required `W04_PGLITE_PRODUCTION_CONFIG_POLICY_PASS` marker and
four expected fail-closed negative markers for non-durable storage, inline
password, invalid TTL, and missing production TTL. This validates configuration
shape only; it does not validate a real volume, backup, provider, collector,
operator, or release decision.

The retained non-cancelling manual qualification
[35695427227](https://github.com/andymac4182/mount-rs/actions/runs/35695427227)
targets exact SHA `e7850fb41775351503e5aa685484906b3a3cbbe4`. Its ARM and
macOS-15-intel Node jobs passed the exact early-rejection and PGlite/restart
steps, but Ozone/FoundationDB failed its lifecycle capacity gate at `396.44`
IOPS versus `1000`, and both TiDB lanes failed their ambiguous-commit functional
boundary. The macOS-latest and Ubuntu Node jobs were still queued at the latest
ledger refresh. This is qualification evidence only; production remains
**NO-GO**.

The retained native package artifacts provide current candidate provenance for
the support matrix:

| Package | Artifact | SHA-256 digest |
| --- | --- | --- |
| macOS arm64 | `native-macos-latest` (ID `10679168108`) | `8863a55eeb5f549c344fdd8715e6301c88ce0bbc2a8f0293f759fab48fb00df7` |
| macOS x86_64 | `native-macos-15-intel` (ID `10678858847`) | `c875f4be5b4f3e242f0df30fcc0b4f847700ef50a8f6d31114657f6f4e43e582` |
| Linux x86_64 | `native-ubuntu-latest` (ID `10678797966`) | `7d28416c540ad863cd33fd447f5d1f348898f901288645005cb7cffd3f64a8a3` |
| Linux arm64 | `native-ubuntu-24.04-arm` (ID `10679631427`) | `78d33dd923d565542b31dcb87df22132d994dca021f89863dea7fc38efb37a5f` |
| Windows x86_64 MSVC | `native-win32-x64-msvc` (ID `10679196530`) | `0980293de9302de1afc5fd4052b58313fdec6b537a8e71e5f3af8f0448ecbff2` |

These digests are qualification artifacts, not a signed/tagged production
release. Ozone/TiDB measured `342.86` IOPS, Ozone/FoundationDB `157.33`, and
one Ozone composition `838.37` against the hard `1000` target; W26 evidence
failed closed without `OZONE_IOPS_PASS`. The advertised provider scope must be
chosen explicitly before rerunning or excluding those lanes.

Do not promote a demo, local filesystem, ephemeral container volume, provider
mock, credential-free policy check, or skipped hosted job into a production
pass. If the launch is PGlite-only, explicitly record the excluded provider
rows and their non-goals; if an external provider is advertised, execute that
provider's real identity, durability, cleanup, restart, and recovery gates.

### Credential-free PGlite launch-config policy

The repository has a static, network-free policy check for the advertised
PGlite-only CLI shape:

```sh
node scripts/verify-w04-pglite-production-config.mjs \
  /path/to/approved-w04-pglite-config.json
```

The policy requires an absolute normalized mountpoint, a `splitstore` driver
whose metadata and blocks are both PGlite, durable metadata and blocks,
distinct scoped volume keys, an external `MOUNT_RS_PGLITE_URL` reference,
bounded chunking, an owner field, and an explicit positive safe-integer
`driver.storage.lease_ttl_ms` no greater than 24 hours. The checked fixture
uses `120000` ms as a provisional example. The general CLI runtime defaults to
30 seconds for
backward compatibility, but a production configuration that omits the field
is rejected. The selected value must cover observed provider latency without
making stale-writer recovery unacceptably slow. The policy rejects inline
secret values and unknown fields.
The checked fixture under
`tests/pglite/production-config-policy.json` is a shape test, not an approved
deployment configuration. Its negative fixtures prove fail-closed behavior.
The policy does not connect to PGlite, inspect the server's persistent data
directory, prove backup consistency, or replace the deployment-owner review.
The same check runs in `.github/workflows/w04-production-policy.yml`, whose
non-cancelling concurrency group keeps a terminal policy result independent of
the long-running provider matrix. A passing policy workflow is still only
configuration-shape evidence.

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
   digests, lockfile, PGlite version, selected lease TTL, and approved
   configuration digest.
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

### Repository-local rehearsal

The focused local harness now exercises the control flow against a real
disk-backed PGlite server:

```sh
MOUNT_RS_RUN_PGLITE_SERVER_LIFECYCLE=1 \
  ./scripts/cargo-shared test --locked -p mount-rs-core \
  --test pglite_server_lifecycle -- --ignored --nocapture
```

It quiesces the filesystem, copies the data directory to an isolated restore
directory, adds a marker through a bad-release candidate, restores the
pre-candidate copy, and verifies a fresh server reads the original marker and
returns `ENOENT` for the candidate marker. A passing run emits
`PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`. This is supporting local rehearsal
evidence only: the copy is not an encrypted production backup, and it does not
measure production RPO/RTO, power-loss consistency, retention, access control,
or deployment-owner approval.

## Signals, limits, and operator response

These are required mappings to the selected collector and pager. They are not
evidence that the signals are currently emitted.

| Signal | Minimum admission/alert condition | Operator action |
| --- | --- | --- |
| PGlite readiness/open failures | Any sustained open/reopen failure or failed canary | Stop promotion; preserve the data directory and inspect version/permissions |
| Restart recovery | Recovery exceeds the approved RTO or fresh-client readback differs | Stop writers; quarantine the canary and follow restore procedure |
| Metadata/data integrity | Hash, manifest, size, truncation, or ownership mismatch | Stop publication; preserve both metadata and data evidence; do not guess-repair |
| Fencing/ownership | Unexpected stale-owner, lease, or concurrent-writer signal | Fence the suspect process and require a fresh owner/read-only check |
| Lease configuration | Selected `lease_ttl_ms` is missing, outside the approved bound, or shorter than measured provider operation latency | Hold promotion; review the configuration digest and recovery objective before admitting writers |
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
