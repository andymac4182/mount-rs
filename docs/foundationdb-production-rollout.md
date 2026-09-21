# W07 FoundationDB production rollout

This ledger tracks the production qualification of the W07 FoundationDB
workstream after the demo. It is deliberately separate from the provider and
local/hosted qualification checks in `WORK_TRACKER.md`: a green demo, a local
Docker cluster, or a hosted fixture does not authorize a production rollout.

## Current decision

| Field | Status |
| --- | --- |
| Workstream | W07 — FoundationDB metadata with RustFS S3 chunks |
| Qualification baseline | W07.1, W07.2, W07.4, W07.6 and W07.6a are checked; W07.3, W07.5 and W07.7 remain open in `WORK_TRACKER.md` |
| Production rollout | **NO-GO** |
| Primary reason | Hosted Linux qualification is green, but no production topology, identity/ACL proof, clock/failover rehearsal, backup/restore packet, operational telemetry, production load/capacity result, complete native support decision, or release-owner sign-off is recorded |
| Evidence rule | Every result must identify the tested revision, image/provider versions, topology, test/run ID, terminal status, owner, and cleanup/rollback outcome |

The detailed tracker is the source of truth for implementation and acceptance
status. This ledger is the source of truth for the separate production track.
Do not check a production gate from demo behavior, a queued/skipped/cancelled
CI job, installation-only evidence, or a local qualification report.

## Latest qualification evidence

| Run | Result | Boundary |
| --- | --- | --- |
| `foundationdb-soak-durable`, 2026-09-21, arm64, tested tree `3b64a98` (published as `39a20b6`) | **PASS** — three pinned FoundationDB 7.4.7 servers with `double`/SSD configuration; one bounded composition soak round; replicated-node restart; authority republish; fresh-client RustFS reopen; owned cleanup | Disposable loopback/non-secure Docker qualification only. It does not prove production identity/ACL/TLS, power-loss or backup recovery, capacity/cost, multi-day soak, hosted CI, native platform support or operator readiness. |
| `foundationdb-network-cleanup`, 2026-09-21, arm64, tested tree `65b521c` (published as `6d2a5d4`) | **PASS** — shared RustFS/FDB client-network endpoint, endpoint reachability probe, durable composition, FoundationDB node restart/reopen and owned network disconnect/cleanup | Local disposable Docker qualification only; it does not prove hosted CI, production identity/ACL/TLS, capacity, backup/restore or operator readiness. |
| [Hosted run `35598049389`](https://github.com/andymac4182/mount-rs/actions/runs/35598049389), job `106327338587`, revision `65c52b9`, Ubuntu 24.04 | **PASS — hosted qualification** — pinned RustFS and FoundationDB image matches; durable three-server `double`/SSD topology; shared-network endpoint reachability (`403`); multi-chunk composition; one soak round; live Node/N-API; Linux FUSE/native CLI gate; service-node restart; authority republish; fresh-client reopen; `FOUNDATIONDB_TEST_PASS`, `RUSTFS_COMBO_PASS` and final RustFS integration pass | Terminal hosted Linux evidence for this revision only. It is not production identity/ACL/TLS, power-loss or backup/restore, capacity/cost, multi-day soak, macOS, or release approval. |
| Hosted attempt `35591729423`, revision `1ea183e`, Linux `foundationdb-rustfs` | **NO HOSTED PASS** — FoundationDB configured/readiness/image-match markers passed, then the client failed to reach the published RustFS endpoint at `host.docker.internal:32768`; the workflow was later superseded | Historical blocker that motivated the shared-network fix in `6d2a5d4`; the terminal rerun is recorded above. |

The hosted pass above is retained as qualification evidence for the
restart/fencing and harness gates, not as production acceptance. Its markers
included
`FOUNDATIONDB_RUSTFS_CHUNKED_PASS`, `FOUNDATIONDB_SOAK_PASS rounds=1`,
`FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS` and
`FOUNDATIONDB_TEST_PASS topology=durable`.

The follow-up network-cleanup run also emitted
`FOUNDATIONDB_RUSTFS_NETWORK_READY`,
`FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE status=403`,
`FOUNDATIONDB_TEST_PASS topology=durable manifests=tests/foundationdb/Cargo.toml+integrations/mount-rs-foundationdb/Cargo.toml platform=linux/arm64 service_restart=pass soak_rounds=0`,
`RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS` with exit 0.

The earlier hosted failure identified the Linux container-network boundary
rather than an FDB transaction failure. The fix attaches the owned RustFS
container to the FoundationDB client network under `mount-rs-rustfs` and
disconnects it during owned cleanup. The terminal hosted rerun above passed the
composition, N-API, Linux native CLI/FUSE and restart phases; the earlier run
remains retained as the failure that motivated the fix.

The repository now also contains the manually dispatched
`.github/workflows/foundationdb-production.yml` gate. It uses non-cancelling
concurrency, runs the durable composition with one bounded soak round plus
Node, Linux native CLI/FUSE, restart and fresh-client checks, and retains a
redacted terminal log artifact. This makes the hosted qualification result
auditable despite unrelated mainline pushes; it remains qualification evidence
and cannot close the production gates below.

## Production gate ledger

| Gate | Status | Required exit evidence |
| --- | --- | --- |
| P0 — scope, support matrix, SLO/RPO/RTO and ownership | Open; config policy implemented | Named production topology, supported FoundationDB/RustFS/metadata/client versions, traffic envelope, SLOs, RPO/RTO, on-call owner, rollback authority, and approved non-goals |
| P1 — production FoundationDB topology and rehearsal | Not started; config policy requires durable/shared-provider shape | Repeatable multi-process/HA topology, replication and storage policy, pinned images, network policy, capacity limits, clean-client readiness, restart/failover and deployment rehearsal |
| P2 — metadata and block-provider matrix | Qualification only | Explicit production provider choices and supported combinations; secure staging runs for FoundationDB metadata, RustFS/AWS-compatible blocks, Node, Rust CLI and native clients |
| P3 — authority identity, ACLs, TLS and secret lifecycle | Not started; config policy checks shared-provider mode, HTTPS blocks and external references | One write-capable authority identity per prefix; read-only consumer identities; actual tenant/credential/ACL negative test proving consumers cannot publish or overwrite; secret injection/rotation, TLS policy and redacted logs |
| P4 — replicated durability and storage failure protection | Not started | Backup/replication and sync policy review; node, disk, process and power-loss boundaries; integrity checks for metadata, fences, authority samples and immutable blocks; recovery evidence on the intended storage class |
| P5 — fencing, ambiguous commit and failover recovery | Partial qualification; local and hosted durable restart evidence | Secure multi-node tests covering stale writers, lease expiry/renewal, maybe-committed reconciliation, network delay/partition, authority loss, reviewed failover and no split-brain publication |
| P6 — backup, restore and disaster recovery | Not started | Consistent metadata/authority/block backup definition, encrypted retention, clean-environment restore, hash/revision verification, measured RPO/RTO and provider/region-loss procedure |
| P7 — observability, alerts and runbooks | Not started | Metrics and alerts for cluster health, authority publication age/errors, lease-fence/ESTALE, transaction retry/maybe-committed EIO, block errors, latency, capacity and cleanup/space pressure; tested on-call runbook |
| P8 — load, capacity, soak and cost envelope | Harness + one-round local and hosted qualification; latency marker added; production evidence open | The opt-in harness supports bounded repeated real FoundationDB/RustFS composition rounds with unique prefixes and cleanup, and now emits p50/p95/p99 operation-latency and throughput markers for future rounds; one durable round passed locally and on hosted Linux; production-shaped workload, concurrency, duration, retry/error budget, resource growth, safe capacity and scaling triggers are still required |
| P9 — upgrade, rollback and compatibility | Not started | Forward/backward keyspace and configuration compatibility, rolling provider/client upgrade, failed-upgrade rollback, retained-data downgrade boundary, lockfile/image/artifact provenance |
| P10 — security, privacy, tenancy and audit | Not started | Threat-model review, prefix/tenant isolation, data classification, encryption, audit retention, dependency/image review, abuse/rate limits, closed findings or approved exceptions |
| P11 — native client, mount and platform support | Qualification only; hosted Linux Node/CLI evidence | An explicit advertised platform matrix; clean-install, native FDB client, Node/CLI, FUSE/NFS/FSKit lifecycle, concurrent access, restart/recovery and packaging/signing evidence for every advertised platform |
| P12 — release packaging, CI promotion and canary | Qualification CI plus policy gate | Locked and signed artifacts, SBOM/provenance, protected environment approvals, production-like canary, holdback, promotion checks, rollback automation and retained evidence packet |
| P13 — incident, failover and recovery rehearsal | Not started | Timed operator exercises for authority loss, cluster loss, stale client, storage exhaustion, bad deploy, credential expiry and restore; paging, runbook, integrity and RTO evidence |
| P14 — final launch audit and go/no-go | Not started | One-revision audit of P0–P13, known-limitations record, release-owner decision, canary exit evidence and explicit GO or NO-GO |

No P0–P14 gate is currently terminally accepted. A production gate may move to
complete only when the exit evidence is from the named production-like
environment and the owner records the result; implementation tests alone do
not close operations, security, native, or release gates.

## Credential-free production configuration policy

The repository now includes
`scripts/verify-w07-production-config.mjs` and positive/negative fixtures under
`tests/foundationdb/`. The gate checks the configuration shape that the
configuration-driven CLI accepts: split-store FoundationDB metadata, an
absolute cluster-file path, `durable: true`, the protected `shared-provider`
authority mode and non-empty stable prefixes; it also requires durable HTTPS
RustFS blocks and external `R2_ACCESS_KEY_ID`/`R2_SECRET_ACCESS_KEY`
references. It rejects placeholders, inline secrets, remote plaintext HTTP,
authority modes that are not suitable for independent production writers and
other unsafe shapes.

Run it without credentials or network access:

```sh
node scripts/verify-w07-production-config.mjs \
  tests/foundationdb/production-config-policy.json
```

The dedicated hosted qualification workflow runs both fixtures. A
`W07_PRODUCTION_CONFIG_POLICY_PASS` line is only static deployment-shape
evidence: the verifier does not open FoundationDB or RustFS, cannot prove
ACLs, certificate trust, replication, backups, capacity, monitoring or owner
approval, and does not change the **NO-GO** decision.

The real composition test now emits
`FOUNDATIONDB_LATENCY_PASS workload=composition` with operation count,
p50/p95/p99 microsecond latency, total duration and aggregate operation
throughput. This makes later staging/load runs retain measurable workload
evidence; the existing hosted run predates this marker and is not retroactively
upgraded.

The operational execution template is
[`W07-operations-runbook.md`](W07-operations-runbook.md). It defines the
admission checks, failure responses, backup/restore procedure, timed D01–D09
drills, observability handoff and evidence fields. Every drill remains
`Not executed — external production gate` until it runs against the named
production-like environment.

## FoundationDB deployment contract

The production lease path must use `with_production_lease_oracle` with a
protected `FoundationDbSharedLeaseOracle` (or an application-owned oracle that
declares `LeaseAuthorityKind::SharedProvider`). The default, development,
system-clock, and persisted single-authority paths remain fail-closed or
development-only for independent writers.

The deployment must prove and continuously enforce all of the following:

- exactly one write-capable authority identity owns each authority prefix;
- storage workers have read-only access to the authority record and cannot
  publish or overwrite it;
- the authority host has a monitored clock-skew bound and publishes more often
  than the shortest lease TTL;
- the authority republishes after restart before consumers are admitted;
- authority loss fails closed, or uses an explicitly reviewed failover
  authority; and
- credentials, cluster files, certificates, and rotation material are injected
  at runtime and never committed or printed.

These are deployment controls. The library API and a shared FoundationDB
`Database` handle cannot prove the credential/tenant/ACL boundary by
themselves, so a production negative test with the actual identities is
required.

## Rollout sequence

1. Close P0 with the production target, support matrix, SLO/RPO/RTO, owners and
   non-goals.
2. Rehearse the secure, replicated FoundationDB/RustFS topology and record the
   exact images, cluster configuration, identity policy and storage class.
3. Run the locked provider, composition, native, failure, backup/restore,
   observability, load/soak and security gates using the procedures in
   [`W07-operations-runbook.md`](W07-operations-runbook.md) against that
   staging topology.
4. Deploy one canary with a holdback. Record the artifact digest, configuration
   digest, authority identity, smoke result, metrics, cleanup and rollback
   result before expanding.
5. Promote in stages only after the approved SLO window and alert/runbook
   checks pass. Keep the prior artifact and restore procedure available.
6. On rollback, stop new writers, preserve the metadata/authority snapshot and
   block prefix, restore the last known-good artifact/topology, verify reads,
   fences and ownership, then resume only after the release owner approves.

## Evidence and blocker rules

- Local Docker is useful qualification evidence but is not production
  authentication, power-loss durability, capacity, or operator evidence.
- A hosted FoundationDB/RustFS job is revision-specific provider evidence; an
  active, queued, skipped, cancelled, or failed job is not a pass.
- FoundationDB service restart, RustFS restart, authority republish, fresh
  client reopen, native CLI, Node, and macOS/Linux acceptance are separate
  boundaries and must retain their own run IDs.
- Missing credentials, infrastructure, certificates, monitoring, native
  runners, security review, or approvers are explicit blockers. They must not
  be replaced with synthesized configuration or a weaker local test.
- Every production result must retain redacted logs/markers, cleanup status,
  rollback outcome, and the exact revision tested. Later mainline changes do
  not retroactively change an earlier result.

The open gates above are the required follow-up to the demo. The release
decision remains **NO-GO** until the evidence packet and owner sign-off close
the applicable gates for the advertised production scope.
