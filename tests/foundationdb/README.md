# FoundationDB + RustFS composition gate

This standalone test crate is the real split-provider acceptance lane. It
opens `ChunkedFs` with `mount-rs-foundationdb` as the fenced metadata store and
`mount-rs-r2::R2BlockStore` against the RustFS S3 endpoint as the immutable
block store. The test performs multi-chunk binary writes, partial overwrites,
truncate/extend operations, fresh provider reopen, metadata revision CAS, and
stale-fence rejection. The real composition path publishes a protected shared
FoundationDB authority and opens consumer storage through its read-only oracle;
the deterministic expiry subtest uses an injected clock only to keep the
boundary reproducible. Ordinary provider construction remains fail closed until
a protected shared `LeaseOracle` is supplied.

The focused command is:

```shell
cargo test --manifest-path tests/foundationdb/Cargo.toml \
  --lib foundationdb_rustfs_chunked_composition -- --exact --nocapture
```

It requires `MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE`, `R2_ENDPOINT`, `R2_BUCKET`,
`R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, and `RUSTFS_COMBO_PREFIX`. The
repository `scripts/test-foundationdb.sh` supplies the FDB server/client in a
disposable container. When invoked by `scripts/test-rustfs.sh`, it attaches the
RustFS container to the FoundationDB client network as `mount-rs-rustfs` and
uses the service's internal port 9000; this keeps the composed path working on
Linux runners where a host-published loopback port is not reachable from a
client container. The RustFS lifecycle and run directory remain owned by that
harness.

The repository-level composed lane is:

```shell
RUSTFS_COMBO_NAME=foundationdb-metadata-rustfs-chunks \
RUSTFS_COMBO_TIMEOUT_SECONDS=900 \
RUSTFS_COMBO_COMMAND='./scripts/test-foundationdb.sh' \
./scripts/test-rustfs.sh
```

For a bounded repeated-composition qualification run, set
`MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS` to a value from 1 through 100. Each round
uses a unique FoundationDB/RustFS prefix, exercises the real multi-chunk
composition, and cleans its own block prefix:

```shell
MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS=5 \
RUSTFS_COMBO_NAME=foundationdb-metadata-rustfs-chunks \
RUSTFS_COMBO_TIMEOUT_SECONDS=1800 \
RUSTFS_COMBO_COMMAND='./scripts/test-foundationdb.sh' \
MOUNT_RS_FOUNDATIONDB_TOPOLOGY=durable \
./scripts/test-rustfs.sh
```

The harness emits `FOUNDATIONDB_SOAK_PASS` and includes the round count in
`FOUNDATIONDB_TEST_PASS`. This is repeated provider/composition qualification,
not a production capacity, cost, or multi-day soak claim; the production
workload and duration must still be defined and accepted in
`docs/foundationdb-production-rollout.md`.

Set `MOUNT_RS_FOUNDATIONDB_TOPOLOGY=durable` for the three-node disposable
cluster used by the hosted acceptance lane. It uses three pinned FoundationDB
server containers, `double` redundancy, separate persistent Docker volumes,
and restarts one replicated node while the other two coordinators remain
available. The default `single` topology is still useful for a fast local
provider smoke test and is not replicated-durability evidence.

The FoundationDB script owns its isolated server, cluster file, client library,
network, and cleanup for that run. In the composed lane it runs the first client
against the live providers, restarts the owned FoundationDB container,
republishes authority time in a separate client process, and then runs
`foundationdb_rustfs_chunked_restart_reopen` in a fresh client process. That
second process reopens the persisted namespace, reads the RustFS-backed
multi-chunk file, reruns metadata CAS/fencing checks, and performs exact
owned-prefix cleanup. The lane therefore requires an owned disposable server;
an externally supplied FoundationDB cluster is rejected instead of being
reported as service-restart evidence.

The standalone provider-only command owns a disposable FoundationDB server and
does not claim RustFS or service-restart coverage:

```shell
./scripts/test-foundationdb.sh
```
