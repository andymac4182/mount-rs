# W26 Apache Ozone production-integration contract

This document is the handoff contract for customers and the streams that will
operate and release the Apache Ozone integration. W26 prepares and qualifies
the integration; W26 does not deploy Ozone, operate customer environments,
provide backup/DR, or execute releases. There is no W26 staging environment;
the executable qualification environment is CI.

The machine-checked, credential-free contract declaration is
[`tests/ozone/production-rollout-contract.json`](../tests/ozone/production-rollout-contract.json).
Run it with:

```sh
node scripts/verify-w26-ozone-rollout-contract.mjs \
  tests/ozone/production-rollout-contract.json
```

The validator checks admission requirements without opening a network
connection or reading a credential. A `PASS` from this validator is
**declaration-only evidence**. It is not proof of a customer deployment,
Ozone replication, certificate validity, IAM policy, measured availability,
backup/restore, RPO/RTO, or capacity.

## Required customer/Ozone contract

| Area | Required declaration or evidence | W26 boundary |
| --- | --- | --- |
| Service envelope | Tier 1, 99.99% availability objective, 5-minute RPO and 5-minute RTO | W26 encodes and rejects weaker declarations; customer operations must measure and sign off the objectives |
| Ozone gateway | Qualified gateway version 2.2.1 for this packet; endpoint is injected through `MOUNT_RS_OZONE_ENDPOINT` | Customer supplies the endpoint and requalifies any other Ozone version |
| Transport | TLS required, peer and hostname verification enabled, CA bundle supplied through `MOUNT_RS_OZONE_CA_BUNDLE` | Customer owns certificate issuance, trust distribution, rotation and revocation evidence |
| Authentication | SigV4 with `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY` external references | W26 never stores or prints values; customer owns IAM, least privilege and rotation |
| Tenancy | Bucket supplied by `MOUNT_RS_OZONE_BUCKET`; object prefix must include a tenant scope (`tenant/{tenant}/mount-rs`) | Customer maps tenant identity and policy to the prefix and proves isolation |
| Ozone topology | At least three data nodes, replication factor three, three failure domains and power-loss-durable storage | Customer supplies the actual topology, storage class, quorum and failure-domain evidence |
| Metadata matrix | SQLite, PGlite, TiDB and FoundationDB are the advertised W26 provider rows | Each selected provider still needs its matching hosted CI artifact and customer-native configuration/operations review |
| Operations | Health probe, metrics, audit logging, least privilege and tenant isolation enabled | Customer connects dashboards, alerts, audit retention and paging |
| Recovery | Customer/Ozone owns backup and restore; restore drill required at least every 30 days; RPO/RTO evidence required | W26 does not build a competing backup system or claim a restore drill |

The existing production-config policy additionally validates the concrete
metadata/block configuration for each provider, HTTPS block endpoints, durable
settings, scoped prefixes, and exact external secret references. Both policy
layers are intentionally credential-free.

## Provider and client qualification boundary

The hosted W26 evidence packet must contain, on one clean source revision:

1. the Ozone gateway fault/restart/reopen and cleanup log;
2. SQLite/R2 and PGlite/R2 composition logs plus their 1,000-IOPS artifact;
3. durable TiDB/R2 composition logs plus its 1,000-IOPS artifact;
4. durable FoundationDB/R2 composition logs plus its 1,000-IOPS artifact; and
5. the policy log, including all positive and negative production-contract
   markers, and a passing `w26-ozone-evidence` aggregate verifier.

The artifact verifier requires the fixed 4 KiB, 400-iteration,
concurrency-64 workload, exact requested providers, complete write/read/verify/
delete samples, zero timeouts and cleanup failures, finite latency percentiles,
and at least 1,000 successful lifecycle IOPS per configured provider. Hosted
CI evidence is a qualification boundary; it is not a customer physical-drive
capacity guarantee.

## Security and operational admission

Before another stream treats W26 as integration-ready, the customer/deployment
owner must attach redacted evidence for:

- Ozone TLS certificate identity, trust distribution, expiry/rotation and
  revocation handling;
- SigV4/IAM policies scoped to the bucket and tenant prefixes, including a
  clean-client denial test;
- provider credentials and CA material supplied by the approved secret manager,
  with rotation/revocation tested without data loss;
- Ozone node count, replication, failure domains, persistent storage and
  power-loss behavior;
- health, error, retry, fencing, reconciliation and cleanup metrics connected
  to alerts and runbooks;
- a restore drill with measured RPO/RTO and metadata/block consistency; and
- a timed failover/incident record demonstrating the 99.99% operating model
  and five-minute recovery objective.

W26 can add CI fault and recovery cases where the fixture controls them, but a
restart/reopen test is not a power-loss, backup/restore or customer incident
drill. The W26 decision therefore remains **NO-GO** until the one-revision
packet and external handoffs are complete.

## Ownership and handoff

| Responsibility | Owner | W26 output |
| --- | --- | --- |
| Ozone deployment, topology and day-2 operation | Customer/deployment stream | Contract inputs and redacted deployment evidence |
| Backup, restore, DR and retention | Ozone/customer operations | Restore design, drill record and measured RPO/RTO |
| Release, promotion and rollback | Separate release stream | Reproducible W26 qualification commands and compatibility notes |
| Provider/client correctness and CI qualification | W26 | Code, tests, retained artifacts and explicit NO-GO/ready decision |

