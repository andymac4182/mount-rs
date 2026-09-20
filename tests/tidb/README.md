# Real TiDB integration test

Run the local gate from the repository root:

```sh
./scripts/test-tidb.sh
```

The default `durable` topology starts the official, version-pinned
`pingcap/pd:v8.5.7`, `pingcap/tikv:v8.5.7`, and `pingcap/tidb:v8.5.7`
images as three PD nodes, three TiKV nodes, and one TiDB SQL frontend. The
script selects `linux/arm64` on Apple Silicon and `linux/amd64` on an x86
Docker host, then verifies that each pulled image resolves to that platform.
The image repositories and tags are the PingCAP-published images; this gate
does not use MySQL, TiDB `unistore`, or `mocktikv`.

The harness assigns stable, per-run IP addresses on a private Docker bridge so
PD, TiKV, and TiDB do not depend on Docker's embedded DNS during bootstrap.
Each TiKV mounts the scoped [tikv-test.toml](./tikv-test.toml) profile, which
keeps caches, worker pools, and logging bounded without changing the real
Raft-backed storage engine. Readiness and restart phases each have their own
`MOUNT_RS_TIDB_STARTUP_TIMEOUT_SECONDS` deadline (default 300 seconds), and
the harness requires 10 GiB of Docker memory before launching the full
durable topology. Set `MOUNT_RS_TIDB_ALLOW_UNDERPROVISIONED=1` only when an
explicit diagnostic attempt is wanted; that mode is not a durable acceptance
result.

The test performs these checks in order:

1. Wait for all PD/TiKV stores and the TiDB status endpoint.
2. Run `SELECT tidb_version(), VERSION()` through the MySQL wire client. A
   MySQL-compatible server that does not identify as TiDB fails before the
   provider creates any tables.
3. Run the ignored `mount-rs-tidb` provider contract with a unique volume key.
4. In `durable` mode, restart the TiDB frontend and one TiKV container, wait
   for the store to return to PD, and verify the same metadata revision and
   immutable block remain available.

The restart check demonstrates data surviving component restarts in this
Raft-backed test cluster. It is not a power-loss, host-filesystem-fsync, or
production-capacity claim. `durable(true)` remains an explicit caller
assertion in the provider and must be paired with the deployment's TiKV
replication and sync-log policy. The provider `flush` hook is an
acknowledgement round trip; `SELECT 1` is not itself a storage-engine fsync.

## Isolation and cleanup

The harness requires Docker and `curl`, publishes only the TiDB SQL/status
ports to `127.0.0.1` on ephemeral host ports, and keeps the generated URL in a
child-process environment rather than printing it. Every network, container,
and data volume has a unique run identifier and a matching Docker label. The
exit trap removes only those exact resources. It never uses host networking,
`docker system prune`, daemon configuration, or a shared Compose project.
On failure it reports the phase, container state/IPs, network membership,
Docker CPU/memory stats, and bounded redacted log tails before ownership-checked
cleanup. A cleanup failure is itself reported as a non-zero result.

The local root user has an empty password, so no test secret is required. If a
future backend error contains a MySQL URL, harness diagnostics redact the
password-shaped authority before printing component logs. The provider also
redacts rejected connection-URL errors; the explicitly ignored test fails if
`MOUNT_RS_TIDB_URL` is missing rather than silently passing.

## Modes and CI

`MOUNT_RS_TIDB_TOPOLOGY=single ./scripts/test-tidb.sh` starts one PD and one
TiKV for a lighter actual-component smoke test. Single-node mode is not
evidence for replicated durability and intentionally skips the restart
assertion. Keep the default `durable` mode for persistence/restart evidence.

The following bounded overrides are available for CI runners with a different
Docker architecture or a deliberately selected released image tag:

```sh
MOUNT_RS_TIDB_DOCKER_PLATFORM=linux/amd64 \
MOUNT_RS_TIDB_VERSION=v8.5.7 \
  ./scripts/test-tidb.sh
```

The runner should provide Docker with enough memory for a three-PD,
three-TiKV test cluster; PingCAP's quick-start guidance recommends at least
10 GiB RAM and 4 CPUs for that topology. CI should cache the three official
images, run this script from a clean checkout with `cargo test --locked`, and
retain the script's unique-resource cleanup behavior.

## TiDB metadata plus RustFS chunks

The combined ChunkedFs lane is a separate, explicitly opt-in test. Hooke/main
owns the RustFS container, credentials, unique prefix, run directory, and
cleanup; the test only uses the inherited loopback `R2_*` environment and
never removes `RUSTFS_RUN_DIR`. It defaults its scoped volume key and cleanup
fixture from `RUSTFS_COMBO_PREFIX` and `RUSTFS_RUN_DIR` when the optional
`MOUNT_RS_TIDB_*` overrides are absent.

Run it through Hooke's RustFS orchestration with a real TiDB SQL URL:

```sh
MOUNT_RS_TIDB_URL='mysql://root@127.0.0.1:4000' \
MOUNT_RS_TIDB_CHUNKED_RUSTFS=1 \
MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN=1 \
RUSTFS_COMBO_NAME=tidb-metadata-rustfs-chunks \
RUSTFS_COMBO_TIMEOUT_SECONDS=900 \
RUSTFS_COMBO_COMMAND='cargo test --locked -p mount-rs-tidb --test chunked_rustfs -- --ignored --nocapture' \
  ./scripts/test-rustfs.sh
```

The test writes binary data across seven-byte chunks, performs partial writes
and truncate, closes and reopens ChunkedFs, reads the exact durable namespace
and blocks back, and checks TiDB writer fencing plus revision CAS conflicts.
The restart fixture is a scoped manifest: it records the RustFS prefix, TiDB
volume key, and every block written by the seed phase. The reopen phase rejects
any prefix or volume-key mismatch before deleting anything, verifies every
tracked RustFS object is absent after deletion, and verifies the TiDB metadata
row is absent after cleanup.
`MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN=1` performs the close/reopen and scoped
cleanup in one combo invocation; a later invocation with
`MOUNT_RS_TIDB_EXPECT_PERSISTED=1` can verify persistence across an external
service restart. Persisted mode must set all three scope values explicitly:
`MOUNT_RS_TIDB_RUSTFS_PREFIX`, `MOUNT_RS_TIDB_CHUNKED_VOLUME_KEY`, and
`MOUNT_RS_TIDB_CHUNKED_RUSTFS_FIXTURE` to a path outside the transient
`RUSTFS_RUN_DIR`; it will refuse the harness-generated defaults. The URL must
identify TiDB via `SELECT tidb_version(), VERSION()`; MySQL, mock, and
in-memory substitutes are rejected.

Upstream references:

- [TiDB local test cluster](https://docs.pingcap.com/tidb/stable/quick-start-with-tidb/)
- [Official TiKV Docker topology](https://raw.githubusercontent.com/tikv/tikv/master/docker-compose.yml)
- [PingCAP TiDB images](https://hub.docker.com/r/pingcap/tidb/tags)
- [PingCAP TiKV images](https://hub.docker.com/r/pingcap/tikv/tags)
- [PingCAP PD images](https://hub.docker.com/r/pingcap/pd/tags)
