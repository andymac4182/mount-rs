# mount-rs FoundationDB integration

This crate stores the mount-rs namespace and immutable blocks in a
FoundationDB keyspace. It is registered in the core workspace, while its native
client remains behind the opt-in `foundationdb` feature.

The native backend is behind the opt-in `foundationdb` Cargo feature. A normal
workspace build therefore does not try to link a FoundationDB client:

```shell
cargo check -p mount-rs-foundationdb --features foundationdb
cargo test -p mount-rs-foundationdb --features foundationdb
```

The feature-off package is a portable workspace shell; it does not claim to
provide a storage backend. The commands above are the implementation gate and
must run on a supported native-client target with the matching FDB runtime.
On unsupported targets, enabling all Cargo features intentionally leaves this
package empty so client-free Windows and cross-platform workspace checks stay
portable; no FoundationDB backend is advertised there.

## Supported native-client platforms

The initial provider gate follows the platform support published by
`foundationdb-rs`:

| Target | Provider status | Required native client |
| --- | --- | --- |
| Linux x86_64 | supported; upstream binding Tier 1 | FoundationDB 7.4 `libfdb_c.so` |
| macOS x86_64 | supported; upstream binding Tier 2 | FoundationDB 7.4 `libfdb_c.dylib` |
| macOS arm64 | supported; upstream binding Tier 2 | FoundationDB 7.4 `libfdb_c.dylib` |
| Linux arm64 | supported when the matching ARM64 client is supplied; validated by the Docker gate | FoundationDB 7.4 `libfdb_c.so` |
| Other targets | not enabled by this crate | no unverified fallback is selected |

The Rust feature selects the 7.4 C API headers, but it does not install or
embed the native client library or a cluster. The runtime must be able to load
the matching `libfdb_c` and read a cluster file. Caller-owned handles must boot
the FoundationDB network once and keep the returned `NetworkAutoStop` guard
alive until every database handle has been dropped. Consumer-facing code can
use `FoundationDbStorage::connect`, which owns the process-scoped network
guard:

```rust,no_run
let storage = mount_rs_foundationdb::FoundationDbStorage::connect(
    "/etc/foundationdb/fdb.cluster",
    mount_rs_foundationdb::FoundationDbStorageOptions::new("my-volume"),
)?;
```

The constructor intentionally does not select a lease clock. Block operations
and metadata reads are available, but lease acquisition/publication returns
`ENOTSUP` until the application supplies a protected shared `LeaseOracle` with
`with_oracle`/`with_oracle_arc`.

`FoundationDbStorageOptions::with_durable(true)` is an explicit application
assertion. A successful FoundationDB commit is durable according to the
cluster's configured storage policy, but this crate cannot infer whether a
development single-server cluster is failure tolerant.

## Lease oracle boundary

FoundationDB provides transactional ordering and commit versions, but it does
not expose an authoritative server wall clock. The core `MetadataStore` contract
requires provider-determined lease expiry, so this crate does not select a lease
clock by default. Applications must supply a protected shared `LeaseOracle` with
`with_oracle`/`with_oracle_arc`; if they do not, lease operations fail closed with
`ENOTSUP` while block operations and metadata reads remain available.

Production callers should use `with_production_lease_oracle`. That method
requires the implementation to declare `LeaseAuthorityKind::SharedProvider`
through `LeaseOracle::authority_kind`; unverified, development, and
single-authority clocks are rejected when the storage handle opens. The
declaration is an application trust boundary, not a conversion of a local
clock into a distributed authority.

The authority prefix is checked against FoundationDB's hard key-size limit at
construction time, so an oversized prefix fails before the first transaction.

For a FoundationDB-hosted authority, `FoundationDbLeaseAuthority` is the
write-side publisher and `FoundationDbSharedLeaseOracle` is the read-only
consumer view:

```rust,no_run
let authority = mount_rs_foundationdb::FoundationDbLeaseAuthority::connect(
    "/etc/foundationdb/fdb.cluster",
    "mount-rs/lease-authority",
    mount_rs_foundationdb::FoundationDbLimits::default(),
)?;
authority.publish_system_now_ms().await?;
// In a separate worker process, use read-only FoundationDB credentials.
let oracle = mount_rs_foundationdb::FoundationDbSharedLeaseOracle::connect(
    "/etc/foundationdb/fdb.cluster",
    "mount-rs/lease-authority",
    mount_rs_foundationdb::FoundationDbLimits::default(),
)?;
let storage = mount_rs_foundationdb::FoundationDbStorage::connect(
    "/etc/foundationdb/fdb.cluster",
    mount_rs_foundationdb::FoundationDbStorageOptions::new("my-volume")
        .with_production_lease_oracle(oracle),
)?;
```

Run the publisher in one protected authority service and give storage workers
only the read capability for its authority keyspace. The shared reader never
advances time or falls back to a worker's local clock; an unpublished or
unavailable authority returns an error and leases fail closed. The authority
still needs an operational clock-skew bound and recovery policy.

The crate also exposes an explicit `with_persisted_lease_oracle` option for a
single trusted authority or development/test cluster. That oracle stores one
encoded Unix-epoch millisecond value under the volume's `meta/lease-oracle` key
and advances it transactionally using `max(persisted_time, local_system_time)`.
All writers then validate leases against the same durable value rather than
using their local wall clocks directly, but the host clock remains an input to
lease expiry and is not made safe merely by persistence.

This is a durable logical clock with an explicit availability/safety tradeoff,
not a trusted distributed wall-clock service and not proof that production lease
semantics are solved by persisting `max(persisted_time, local_system_time)`.
The host clock that advances the oracle must be monitored for forward jumps and
the oracle keyspace must be writable only by the storage authority. A forward
jump can make a lease expire early; the transactional fence still prevents the
old owner from publishing, but fencing does not make the expiry instant safe.
A backward jump or a stalled authority can delay expiry and therefore reduce
availability. Deployments requiring bounded real-time expiry must supply and
protect a stronger shared clock/lease authority through `LeaseOracle` and must
document its clock-skew assumptions. The `with_oracle`/`with_oracle_arc` APIs
remain available for explicit test or application injection (the `with_clock`
names remain compatibility aliases), while
`with_production_lease_oracle` is the guarded production entry point.

`FoundationDbStorageOptions::without_lease_oracle` makes that fail-closed mode
explicit and is also the behavior of `new`/`default`. `SystemLeaseClock` is
available only through the explicitly named `with_system_clock` opt-in for
single-host development; it is not a distributed authority.

Fencing remains transactional: every renew, release, and publication compares
the stored owner, fence, and expiry in the same transaction that changes state.
An old owner therefore cannot publish after a newer owner has acquired the
fence, even if its local clock is stale. FDB commit versions provide transaction
serialization only; they are not converted into `expires_at_ms`, because a
commit version is not a wall-clock duration. The persisted logical fence and
core revision remain the concurrency tokens, while the persisted oracle remains
the expiry authority. This avoids the unsafe conversion of FDB commit versions
into wall-clock durations.

## Ambiguous commit handling

`foundationdb-rs` distinguishes errors that are known not to have committed
from `maybe_committed` errors, where the mutation may already be durable but the
client lost its acknowledgement. Lease acquire/renew/release and metadata
publication use a non-idempotent transaction policy: a `maybe_committed` result
is returned as `EIO` and the closure is not replayed. Callers must reopen/read
the metadata and reconcile before retrying; otherwise a committed acquire can
look like a conflict, or a committed renew can look stale. The deterministic
unit regression models this committed-but-lost-ack fault explicitly.

Content-addressed block writes and block deletion/read transactions retain the
idempotent retry policy. Repeating a block write verifies the same digest/value,
so it does not allocate a second lease or revision. A failed non-idempotent
metadata operation must never be relabeled `EAGAIN` or `ESTALE` merely because a
subsequent reconciliation observes the state it may have committed.

## Limits

The implementation keeps values well below FoundationDB's hard 100,000-byte
value limit and keys below the 10,000-byte key limit. FoundationDB's hard
affected-data limit is 10,000,000 bytes (decimal), not 10 MiB. The default
block limit is 64 KiB, namespace JSON is split into 8 KiB values, and one
publication is limited to 512 KiB of namespace JSON. Before opening the
transaction, publish accounts for shard mutations, read/write conflict-range
endpoints, the manifest and lease reads, and the compaction range; it rejects
a payload whose publication would exceed 10,000,000 affected bytes. The old
contents of a cleared range are not charged by FoundationDB, but the clear
mutation and its conflict range still have key overhead. Larger namespaces
require a future immutable-manifest publication design rather than silently
exceeding FoundationDB transaction limits.

The block bound is part of the persisted chunker compatibility check, not just
a late block-write error. Call `validate_chunker_config` during filesystem
composition (and use the same `FoundationDbLimits` passed to the storage
options):

```rust,no_run
mount_rs_foundationdb::validate_chunker_config(
    &namespace.default_chunker,
    mount_rs_foundationdb::FoundationDbLimits::default(),
)?;
```

`publish` and `load` repeat the check for every file layout, so an oversized
fixed-size layout cannot enter or be consumed from this provider. A caller that
deliberately raises `max_block_bytes` must stay at or below FoundationDB's hard
value limit and must make the corresponding chunker choice explicit.

## Real database verification

The opt-in integration test uses `MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE` (or the
platform default cluster file when `MOUNT_RS_FOUNDATIONDB_USE_DEFAULT=1`). It
uses a unique key prefix, proves the ordinary constructor fails closed, then
exercises the FoundationDB-hosted shared authority with two independent
readers, a backward time sample, forward recovery, and stale-writer fencing.
It then explicitly selects the persisted single-authority lease oracle and
exercises block immutability, lease fencing, revision-conflict handling,
metadata reload, and deletion against the real cluster. It separately
exercises the explicit opt-out with
`without_lease_oracle` to prove the explicit fail-closed `ENOTSUP` path.

The CI gate must be separate from the portable workspace gate:

1. Select Linux x86_64 or macOS x86_64/arm64, and pin the FoundationDB server
   and C client to a version compatible with the `fdb-7_4` feature.
2. Install the native client library and headers, verify the loader path and
   `fdbcli`/client version, and create a disposable single-node test cluster.
   Export its cluster-file path as `MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE`.
3. Run `cargo test -p mount-rs-foundationdb --features foundationdb --test
   foundationdb -- --nocapture` after the server is healthy.
4. Run the ordinary workspace checks without `--all-features`. An
   `--all-features` or provider test run is valid only on a job that installed
   the matching native client; it must not be part of a client-free matrix.

The repository-owned `scripts/test-foundationdb.sh` can run the real gate in
isolated Docker containers. It pulls the pinned official
`foundationdb/foundationdb:7.4.7` platform manifest for the detected Linux
architecture, starts a disposable server, copies its cluster file and client
library into a disposable Rust test image, and runs the feature-gated
integration test against that server. The server and client are derived from
the same pinned image digest; the script prints their image IDs before the
test. Set `MOUNT_RS_FOUNDATIONDB_IMAGE` only when an explicitly selected image
is required. The script removes only its explicitly named
container/network/temp paths on exit; it installs no host client and does not
modify Docker configuration. Set `MOUNT_RS_FOUNDATIONDB_KEEP=1` when
diagnosing a failed run. The feature gate remains separate from the client-free
workspace gate.

When `R2_ENDPOINT` is set for the composed FoundationDB + RustFS lane, the
script runs the composition client, restarts the owned FoundationDB container,
republishes the authority sample in a separate client process, and then runs a
fresh-client reopen/fencing check before reporting success. External FoundationDB
mode is intentionally rejected for this lane because it cannot provide
service-restart evidence owned by the harness.

Set `MOUNT_RS_FOUNDATIONDB_NAPI=1` to add the live Node chunked-factory gate.
The script builds the feature-enabled N-API artifact in the same pinned Rust
client image that supplies `libfdb_c`, then runs the Node 24 test container on
the FoundationDB network. The temporary artifact is written only beneath the
script-owned run directory and is removed with the other test state.

Set `MOUNT_RS_FOUNDATIONDB_NATIVE_CLI=1` as well to run the ignored
config-driven Linux CLI lifecycle test in that client container. The caller
must provide the actual `/dev/fuse`; the script adds only `SYS_ADMIN` and the
FUSE device to the disposable container, installs `fuse3` there, and runs the
feature-enabled CLI through a real FUSE mount with RustFS blocks. This option
requires the composed RustFS lane and is intentionally not inferred from a
client-only provider run.

The normal script invocation owns its cluster file. If an existing cluster is
supplied with `MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE`, the script refuses it unless
`MOUNT_RS_FOUNDATIONDB_ALLOW_EXTERNAL_CLUSTER=1` is also set. That explicit
mode additionally requires `MOUNT_RS_FOUNDATIONDB_NETWORK`,
`MOUNT_RS_FOUNDATIONDB_SERVER_CONTAINER`, and a per-run
`MOUNT_RS_FOUNDATIONDB_TEST_PREFIX` (or the RustFS combo prefix); it verifies the
server is attached to that network and that the server image ID matches the
pinned client image. It never removes the external server or network.

The dependency and limit choices follow the [`foundationdb-rs` client
documentation](https://docs.rs/foundationdb/latest/foundationdb/),
[FoundationDB's known limits](https://apple.github.io/foundationdb/known-limitations.html),
and the [C-client installation/API guidance](https://apple.github.io/foundationdb/api-c.html).
