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
disposable container. When invoked by `scripts/test-rustfs.sh`, it rewrites the
loopback RustFS endpoint to `host.docker.internal` for the test container; the
RustFS lifecycle and run directory remain owned by that harness.

The repository-level composed lane is:

```shell
RUSTFS_COMBO_NAME=foundationdb-metadata-rustfs-chunks \
RUSTFS_COMBO_TIMEOUT_SECONDS=900 \
RUSTFS_COMBO_COMMAND='./scripts/test-foundationdb.sh' \
./scripts/test-rustfs.sh
```

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
