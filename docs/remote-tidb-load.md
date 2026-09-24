# 100-client, 10-server TiDB test

For timed 4 KiB read/write throughput ramps, use the
[saturation benchmark](remote-tidb-saturation.md). This lifecycle test measures
correctness under concurrency, rather than steady-state storage IOPS.

This opt-in integration gate runs ten distinct QUIC server listeners, each with
an independently opened filesystem coordinator. All coordinators use the same
TiDB metadata and block scope. Ten clients per server connect before a shared
start barrier releases all 100 clients. Client and server instances run in one
local test process; this is not a cross-host or ten-process deployment test.

Drive namespace metadata and file blocks live in TiDB. Each server's partition,
Drive, issuer-policy, and grant catalog uses the current SQLite catalog adapter.
The gate checks the real TiDB release identity before using provider tables; a
MySQL substitute or an absent endpoint cannot silently pass. Transport stress
uses a test authenticator, so OIDC signing and external discovery costs are
outside its timing measurements.

## Run against a disposable cluster

```sh
./scripts/test-remote-tidb.sh cluster
```

This reuses the existing pinned TiDB/PD/TiKV harness, readiness checks, provider
regressions, component restarts, and ownership-checked cleanup. Its default
replicated topology requires at least 10 GiB Docker memory and four CPUs. The
remote packet runs before and after the component restart sequence, each with a
new unique Drive scope; it also verifies fresh filesystem reopen within each run.
The stable provider fixture supplies separate component-restart evidence.

For a smaller local actual-component run:

```sh
MOUNT_RS_TIDB_TOPOLOGY=single ./scripts/test-remote-tidb.sh cluster
```

Single topology still runs all 100 clients and ten mount server instances, but
uses one PD and one TiKV and does not establish replicated durability.

## Run against an existing test endpoint

Provide `MOUNT_RS_TIDB_URL` in the child environment, then run:

```sh
./scripts/test-remote-tidb.sh existing
```

Use a disposable test database. Each run creates a unique volume key and test
records. The external mode does not restart the database or change global
settings. The URL is not included in command text or the test result output.

## CI

A manual run of the Remote Drives workflow includes the replicated TiDB scale
job and retains its output artifact. Ordinary package tests compile this gate but
leave it ignored because it requires an actual database. Hosted CI results must
be checked separately from local execution.

## Workload and checks

`MOUNT_RS_REMOTE_TIDB_ROUNDS` selects 1–20 rounds per client (default 2).
Dimensions remain fixed at 100 clients and ten servers. Each lifecycle opens a
new file, writes a distinct 4 KiB client/round payload, closes, renames, reopens,
reads and compares contents, closes, and synchronizes. Acknowledged writes are
recorded before attempting rename, so a later failure cannot erase the write
from the verification ledger. Mutations are not replayed on transport failure.

The test checks denied Drive and Partition mutations and their absence from the
stored namespace, then shuts down servers/providers and opens a fresh independent
TiDB filesystem to verify all acknowledged contents and namespace changes.
It reports per-client/per-server completion, lifecycle p50/p95/p99, throughput,
acknowledged writes/renames, successful fresh verifications, and unexpected failures.
The overall packet has a 600-second timeout and bounded cleanup/verification
phases. Any incomplete client, lost acknowledged file, content mismatch, or
unexpected failure fails the test.

## Executed result (2026-09-24)

The final local run used TiDB v8.5.7 with actual PD/TiKV components on arm64
Docker (14 CPUs, approximately 8 GB memory), in the explicit single topology.
All 100 clients completed two rounds, ten clients per server:

| Check | Result |
| --- | --- |
| Independently opened QUIC servers | 10 |
| Connected clients before start barrier | 100 |
| Acknowledged writes / renames | 200 / 200 |
| Freshly verified files | 200, exact root namespace |
| Completed lifecycles per server | 20 each |
| Unexpected failures | 0 |
| End-to-end packet elapsed | 15.14 seconds |
| Completed workload operations/s | 105.68 |
| Lifecycle p50 / p95 / p99 | 2,919 / 7,155 / 8,315 ms |

Packet elapsed includes connection setup, workload, shutdown, and fresh
verification; each lifecycle contributes eight successful wire operations.
The reused direct provider, concurrent consumer, and ambiguous-commit gates also
passed, and the owned containers/volumes/network were cleaned up. These results
establish same-host coordination and acknowledged-state retention across fresh
filesystem reopen, not replicated durability or production performance.

Normal remote-package tests, formatting, and strict workspace Clippy passed.
The replicated topology and hosted manual scale job are configured but have not
been executed for this change.
The six existing remote Kani decision proofs were rerun after the dependency
update: all passed with zero failed checks and 25/25 covers reached. This does
not formally verify the distributed TiDB workload or database implementation.
