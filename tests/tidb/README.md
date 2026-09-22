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
Raft-backed storage engine. Each TiKV container also receives an explicit
`nofile` limit of 200,000 by default because v8.5.7 refuses to start below
123,880 descriptors; override `MOUNT_RS_TIDB_TIKV_NOFILE_LIMIT` only with a
value at or above that floor. Readiness and restart phases each have their own
`MOUNT_RS_TIDB_STARTUP_TIMEOUT_SECONDS` deadline (default 300 seconds), and
the harness requires at least 10 GiB of Docker memory and 4 CPUs before
launching the full durable topology. Set
`MOUNT_RS_TIDB_ALLOW_UNDERPROVISIONED=1` only when an explicit diagnostic
attempt is wanted; that mode is not a durable acceptance result and is marked
`diagnostic-underprovisioned-not-durable-acceptance` in the final result.

Before topology setup the harness performs one formatted Docker server-info
probe for architecture, CPU and memory. If the daemon cannot return that
response, the command exits with status 2 as an environment prerequisite
failure and creates no test network, volume or container; no
`TIDB_ACCEPTANCE` result is emitted.

The test performs these checks in order:

1. Wait for all PD/TiKV stores and the TiDB status endpoint.
2. Run `SELECT tidb_version(), VERSION()` through the MySQL wire client. A
   MySQL-compatible server that does not identify as TiDB fails before the
   provider creates any tables.
3. Run the ignored `mount-rs-tidb` provider contract with a unique volume key.
4. In `durable` mode, restart one PD member, the TiDB frontend, and one TiKV
   container in sequence. The harness waits for PD quorum and the TiKV store
   count after each relevant phase, then verifies the same metadata revision
   and immutable block remain available.

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

The CI workflow runs the durable provider lane on `ubuntu-24.04` through
`./scripts/test-tidb.sh` and runs the durable TiDB/RustFS composition through
`./scripts/test-tidb-rustfs.sh`. Both jobs keep the official v8.5.7 images
and the RustFS endpoint pinned by their harnesses; a job is evidence only when
the final `TIDB_ACCEPTANCE` line reports
`evidence=durable-multinode-restart`. The composition job also retains the
scoped fixture until the TiDB restart phase and verifies the fresh-client
reopen before cleanup.

## Consumer and evidence boundary

This packet records what the current repository can actually qualify. TiDB is
a workspace Rust crate with ignored, service-backed tests. The Rust SDK,
N-API factory, and Rust/Node CLI configuration have explicit TiDB selection
paths; native-mount selection remains a separate evidence boundary.

### Runnable lanes

| Lane | Exact command | Accepts as evidence | Does not accept as evidence |
| --- | --- | --- | --- |
| Durable real-component gate | `./scripts/test-tidb.sh` | Official pinned PD/TiKV/TiDB images, TiDB identity, the ignored provider contract, and sequential PD/TiDB/TiKV restart/reopen checks. Count it only when the final line says `TIDB_ACCEPTANCE evidence=durable-multinode-restart`. | Power-loss, host-filesystem fsync, production capacity, Node, CLI, or native-mount support. |
| Durable TiDB/RustFS composition | `./scripts/test-tidb-rustfs.sh` | The RustFS harness owns a loopback S3 endpoint while the TiDB harness runs direct provider, ambiguous-commit, restart, and ChunkedFs seed/reopen/cleanup phases. | Cloudflare R2, power-loss, or native-mount support. |
| Single-node real TiDB smoke | `MOUNT_RS_TIDB_TOPOLOGY=single ./scripts/test-tidb.sh` | Actual TiDB identity and provider contract against one PD and one TiKV. | Replicated/durable topology, quorum recovery, or restart acceptance; the result is labeled `single-node-smoke-not-replicated-acceptance`. |
| Underprovisioned diagnostic | `MOUNT_RS_TIDB_ALLOW_UNDERPROVISIONED=1 ./scripts/test-tidb.sh` | A diagnostic attempt when the Docker host is below the durable resource floor. | Durable acceptance; the result is labeled `diagnostic-underprovisioned-not-durable-acceptance`. |
| Direct TiDB provider contract | `MOUNT_RS_TIDB_URL='mysql://user:password@127.0.0.1:4000/test' ./scripts/cargo-shared test --locked -p mount-rs-tidb --test tidb -- --ignored --nocapture` | A reachable server that passes `SELECT tidb_version(), VERSION()` before provider schemas are opened, then schema, UTF-8/trailing-space, provider-clock, fencing, concurrent-CAS, reconnect, block, and flush checks. | Docker topology or component restart evidence. A `mysql://` URL is only a protocol URL; MySQL or another compatible server is rejected by the identity check. |
| TLS TiDB provider contract | `MOUNT_RS_TIDB_TLS_URL='mysql://user:password@host:4000/test?require_ssl=true' ./scripts/test-tidb-tls.sh` | The wrapper requires TLS, CA and hostname verification, keeps credentials out of logs, and runs the same direct provider contract with `rustls` against the actual endpoint. | A compile gate or URL-only validation; production IAM, certificate rotation and deployment approval remain separate. |
| TiDB plus RustFS ChunkedFs | Use the opt-in command in [TiDB metadata plus RustFS chunks](#tidb-metadata-plus-rustfs-chunks). | Real TiDB identity, RustFS-backed immutable blocks, partial writes/truncate, close/reopen, fencing/CAS, and scoped block/metadata cleanup. A successful reopen prints `TIDB_CHUNKED_RUSTFS_REOPEN_PASS`. | Node/N-API, CLI, FUSE/NFS/9P/WebDAV, FSKit, or hosted-CI acceptance. RustFS is not TiDB and S3-compatible evidence is not Cloudflare R2 evidence. |

The direct provider and ambiguous-commit tests are `#[ignore]` and require an
actual TiDB endpoint. If the current checkout contains
`providers/mount-rs-tidb/tests/ambiguous_commit.rs`, its exact opt-in lane
is:

```sh
MOUNT_RS_TIDB_URL='mysql://user:password@127.0.0.1:4000/test' \
  ./scripts/cargo-shared test --locked -p mount-rs-tidb --test ambiguous_commit -- --ignored --nocapture
```

That lane uses a plaintext MySQL-wire proxy, lets TiDB finish `COMMIT`, drops
the response, and verifies an unknown outcome without automatic replay. It is
commit-outcome evidence only; callers must reconcile state before retrying. It
runs as part of both TiDB CI harnesses.

### Provider-selection audit

The following paths are the audited selection points for consumers:

- `apps/mount-rs-cli/src/config.rs` accepts `memory`, `sqlite`, `pglite`,
  `tidb`, and block-only `r2`; `apps/mount-rs-cli/src/parser.rs` exposes the
  `memory`, `host`, `sqlite`, and `splitstore` driver choices.
- `crates/mount-rs-sdk` owns the public split-store facade and depends on the
  TiDB integration behind its provider selection boundary.
- `bindings/mount-rs-napi/src/lib.rs` accepts `memory`, `sqlite`, `pglite`,
  `tidb`, and block-only `r2` for `createChunkedDriver`; unknown backend kinds
  fail rather than falling back to memory.
- `examples/node-cli/index.mjs` mirrors that provider set for mount-free SDK
  use. Its existing native integration uses a host-backed driver, so a passing
  Node native mount does not exercise TiDB or RustFS.
- `.github/workflows/ci.yml` now has dedicated `tidb` and `tidb-rustfs` jobs.
  The generic workspace job still does not run ignored service tests; the
  dedicated jobs are the service evidence boundary.

The Rust SDK, N-API factory, and Rust/Node CLI now provide runnable mount-free
TiDB/RustFS consumer rows. Do not turn those rows, a generic native transport
test, or a macOS/Linux compile into TiDB native-mount acceptance; that remaining
boundary requires a revision-matched service-backed native test on each claimed
platform.

For a TLS-required production endpoint, compile the selected public consumer
with its `rustls` feature: `cargo check --locked -p mount-rs-sdk --features
rustls`, `cargo check --locked -p mount-rs-cli --features rustls`, or
`cargo check --locked -p mount-rs-napi --features rustls`. The feature only
enables the TLS-capable client graph; the endpoint URL, certificate policy,
secret injection and live handshake still require a provider-backed test.

`scripts/test-tidb-tls.sh` is the guarded provider-backed entry point. Its
`MOUNT_RS_TIDB_TLS_VALIDATE_ONLY=1` mode checks deployment URL policy without
connecting or resolving credentials; only a run without that flag and with a
real endpoint produces `TIDB_TLS_ACCEPTANCE_PASS`.

The provider's `durable(true)` flag remains caller-declared. The `flush` hook
is an acknowledged TiDB round trip, not proof of storage-engine fsync, and a
component restart in this test cluster is not power-loss or host-durability
evidence.

## TiDB metadata plus RustFS chunks

The combined lane is a separate, explicitly opt-in test. The wrapper owns the
RustFS container, credentials, unique prefix, run directory, and cleanup; the
TiDB harness owns the TiDB/PD/TiKV topology and restart sequence. The child
tests use only the inherited loopback `R2_*` environment and never remove
`RUSTFS_RUN_DIR`. The wrapper also runs the public Node SDK/CLI matrix and a
required Linux native Node CLI mount when those prerequisites are enabled.

Run it through Hooke's RustFS orchestration with a real TiDB SQL URL:

```sh
MOUNT_RS_TIDB_URL='mysql://root@127.0.0.1:4000' \
  ./scripts/test-tidb-rustfs.sh
```

The test writes binary data across seven-byte chunks, performs partial writes
and truncate, closes and reopens ChunkedFs, reads the exact durable namespace
and blocks back, and checks TiDB writer fencing plus revision CAS conflicts.
The consumer phase then selects the same public TiDB/RustFS shape through the
Node SDK and Rust/Node CLI configuration, exercises native mounted I/O with
independent Rust and Node clients, unmounts, and verifies fresh Node SDK
readback. A required native phase fails on a missing Linux FUSE prerequisite;
the generic host-backed native demo remains separately opt-in.
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
