# Apache Ozone S3 gateway integration gate

This package exercises `mount-rs-r2::R2BlockStore` against the real Apache
Ozone S3 Gateway. It is intentionally separate from the unit tests and from
the live Cloudflare R2 gates. The Rust tests reject non-loopback endpoints, so
this lane cannot silently turn into a cloud-service test.

## Pinned service

`scripts/test-ozone.sh` uses the official Apache Ozone 2.2.1 all-in-one image
from `ghcr.io/apache/ozone`, pinned to the architecture-specific child digest:

```text
linux/amd64  sha256:88cf042bc3b810a66a85ab3fcd7b1558a44bb9d914a28d64e30338ac6780b9a6
linux/arm64  sha256:c7ba6ee740323de7da970d8b8ea373d43c42fe22d31d72077092f5511c5d83ed
```

The all-in-one image starts the Ozone services and exposes the S3 Gateway on
port `9878`. The harness publishes that port only on `127.0.0.1`, uses fixed
throwaway credentials for the local non-secure gateway, and creates one
test-owned bucket and prefix. It does not provision or contact a cloud
service. The explicit FoundationDB composition mode is the only exception: it
also publishes the port on the local Docker bridge so its disposable client
container can reach the gateway. This is a single-container, single-cluster topology with anonymous
Ozone data volumes; it does not claim multi-node replication, Kerberos/TLS
authentication, power-loss durability, or production placement policy.

## Contract coverage

The real gateway tests cover:

- immutable block publication with full and range reads;
- binary block bodies large enough to cross the configured ChunkedFs block
  boundary when the composition lanes are enabled;
- missing-block mapping to `ENOENT`;
- create-only object publication and duplicate-create rejection;
- stale conditional reads and stale conditional writes, with the original
  object verified unchanged;
- successful ETag compare-and-swap updates;
- concurrent block publication;
- a bounded gateway request failure while the real gateway service is stopped;
- reopening a committed block through a fresh client after stopping and
  restarting the same Ozone service.

The shell harness bounds Docker actions, gateway and bucket readiness, the
bucket-bootstrap client, service stop/start, cargo test processes, and cleanup.
The Rust contract tests also impose bounded request timeouts, including the
stopped-gateway failure assertion. By default the harness removes the
test-owned container, its anonymous Ozone data volumes, and its temporary
fixture directory. Set `MOUNT_RS_OZONE_KEEP=1` only when an explicit retained
container is needed for diagnosis. The timeout controls are
`MOUNT_RS_OZONE_STARTUP_TIMEOUT_SECONDS` (180 seconds),
`MOUNT_RS_OZONE_ACTION_TIMEOUT_SECONDS` (30 seconds),
`MOUNT_RS_OZONE_CLIENT_TIMEOUT_SECONDS` (10 seconds),
`MOUNT_RS_OZONE_STOP_TIMEOUT_SECONDS` (30 seconds), and
`MOUNT_RS_OZONE_TEST_TIMEOUT_SECONDS` (600 seconds). Independent provider
composition children are supervised by
`MOUNT_RS_OZONE_COMPOSITION_TIMEOUT_SECONDS` (1200 seconds) with a separate
`MOUNT_RS_OZONE_COMPOSITION_STOP_GRACE_SECONDS` (30 seconds) cleanup grace.
The child harness owns its readiness, test, and resource cleanup lifecycle;
the supervisor gives its cleanup trap time to finish before the Ozone harness
removes the Ozone service.

## Run

From the repository root:

```sh
./scripts/test-ozone.sh
```

Docker and a running Docker daemon are prerequisites. If they are unavailable,
the script exits without claiming integration coverage passed.

## ChunkedFs compositions

The W26.3 composition packet is a separate, ignored, explicitly-gated lane.
It composes the real Ozone S3 Gateway block store with each of these
independent metadata providers:

- file-backed SQLite metadata;
- a real disk-backed PGlite PostgreSQL-wire metadata service from
  `tests/pglite/server.mjs`.

The seed and mutation phases use seven-byte chunks and exercise multi-chunk
writes, partial writes, shrink and extend truncation, range reads, provider
CAS conflicts, expired-writer fencing, and deletion of every test block
object. A fresh `ChunkedFs` reopen then reads the persisted file with a
4096-byte chunk configuration. The composition harness also removes its
PGlite data directory and SQLite file. It never replaces Ozone or PGlite with
an in-memory or mock service.

Install the existing PGlite service dependencies once, then run the exact
opt-in gate from the repository root:

```sh
pnpm --dir tests/pglite install --frozen-lockfile
MOUNT_RS_OZONE_COMPOSITIONS=1 ./scripts/test-ozone-compositions.sh
```

The wrapper refuses to run without the explicit environment gate or without
the PGlite service dependencies. Docker, a running Docker daemon, Node, and
the loopback-only Ozone service remain required. The underlying ignored Rust
tests are invoked with `--ignored` only by this wrapper; a normal
`cargo test --manifest-path tests/ozone/Cargo.toml` does not claim composition
coverage. If Docker, PGlite, or either provider is unavailable, the lane is a
blocked/non-passing attempt rather than a fabricated pass.

The same composition lane can run the CI-only 1,000-IOPS qualification after
the Node addon and PGlite service are available. Set `MOUNT_RS_OZONE_IOPS=1`;
the wrapper invokes the public storage benchmark over the split PGlite/R2 path
with a 4 KiB payload, 400 lifecycle iterations and concurrency 64, and fails
below `MOUNT_RS_OZONE_IOPS_MIN` (default `1000`). Set
`MOUNT_RS_OZONE_IOPS_OUTPUT` to retain the machine-readable JSON outside the
disposable harness directory. One lifecycle is write + full read/verify +
delete, so this is a controlled integration baseline rather than a customer
physical-drive capacity guarantee.

### TiDB and FoundationDB compositions

The existing real-service TiDB and FoundationDB harnesses can now keep the
Ozone gateway alive as the block store. These are separate explicit modes so
the default Ozone contract does not silently start two additional distributed
databases:

```sh
MOUNT_RS_OZONE_TIDB_COMPOSITION=1 ./scripts/test-ozone.sh
MOUNT_RS_OZONE_FOUNDATIONDB_COMPOSITION=1 ./scripts/test-ozone.sh
```

The CI workflow runs the durable FoundationDB mode in the dedicated
`ozone-foundationdb` job. A terminal pass must include Ozone gateway fault
and reopen evidence, durable FoundationDB readiness, N-API split-store reopen,
the FoundationDB/R2 workload artifact, the Ozone integration marker, and
cleanup; queued, canceled, skipped, or failed jobs are not provider acceptance.
The recovery phase restarts the Ozone gateway before the FoundationDB/R2
workload. The hosted lane is still controlled
CI qualification, not customer Ozone availability, TLS, power-loss, or
replication evidence.

The TiDB mode delegates topology, restart, and cleanup to
`scripts/test-tidb.sh` (the default is three PD nodes and three TiKV nodes)
and runs the real `mount-rs-tidb` ChunkedFs composition twice: seed against
Ozone, restart TiDB/TiKV, then reopen and clean the scoped metadata row and
Ozone block objects. The FoundationDB mode delegates the native 7.4 client
container, cluster, and cleanup to `scripts/test-foundationdb.sh`; its
ChunkedFs composition uses Ozone through the Docker host gateway and deletes
only its scoped object prefix. That explicit mode publishes the Ozone port on
the local Docker bridge so the disposable client container can reach it; the
default contract remains loopback-only. Both modes retain explicit CAS,
stale-writer
fencing, partial-write/truncation, binary multi-chunk, reopen, and cleanup
assertions. TiDB replication and FoundationDB durability remain deployment
properties; the tests do not turn a local development cluster into a
production durability claim.

### Node and Rust CLI configuration

[`config-pglite-ozone.json`](../../apps/mount-rs-cli/examples/config-pglite-ozone.json)
is the shared versioned configuration shape for a PGlite metadata provider and
the Ozone S3-compatible block provider. It keeps credentials as environment
references and uses the loopback gateway endpoint. The Rust and Node CLI
configuration checks validate this file without opening providers, resolving
credentials, loading native code, or claiming a live Ozone Node factory pass:

```sh
cargo test --locked -p mount-rs-cli --test cli \
  actual_binary_validates_the_loopback_ozone_provider_config_without_credentials_or_network
node examples/node-cli/index.mjs \
  --config apps/mount-rs-cli/examples/config-pglite-ozone.json --check
```

The base Ubuntu CI Ozone job runs the real gateway contract plus these
mount-free Rust/Node configuration checks. A separate `ozone-compositions`
job installs the locked PGlite service and builds the local N-API addon, then
runs the SQLite/PGlite composition against the real Ozone gateway. With
`MOUNT_RS_OZONE_NODE_COMPOSITION=1`, that same job also runs the public Node
provider matrix's PGlite-metadata/R2-block row and
`tests/ozone/node-cli.mjs`, which executes the versioned config through the
Node CLI's real `--sdk-self-test --reopen` path; it also runs the matching
ignored Rust CLI self-test against a generated versioned config. Those rows
are live provider evidence, not static validation. TiDB and FoundationDB
have dedicated hosted composition jobs: `ozone-tidb` runs the durable
three-PD/three-TiKV TiDB topology against the real Ozone gateway, while the
FoundationDB lane selects its durable three-node topology in the
`ozone-foundationdb` job. A green job is still revision-specific and does not
claim production authentication, TLS, power-loss durability, or native-mount
support.

The same Node composition gate now runs the shipped HTTP server/client path
through `scripts/test-cli-remote-ozone.sh`. On Unix it writes binary data to
both PGlite/R2 and SQLite/R2 drives, verifies full and ranged reads, rejects a
cross-drive token, gracefully shuts down, reopens, and verifies persisted data
again. The test uses a unique child of `OZONE_TEST_PREFIX` and the runner lists,
deletes, and re-lists only that prefix before returning. This is end-to-end
Ozone integration evidence for the HTTP surface; the loopback HTTP fixture does
not prove customer TLS, identity rotation, availability or native mount
support.
