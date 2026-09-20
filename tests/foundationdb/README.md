# FoundationDB + RustFS composition gate

This standalone test crate is the real split-provider acceptance lane. It
opens `ChunkedFs` with `mount-rs-foundationdb` as the fenced metadata store and
`mount-rs-r2::R2BlockStore` against the RustFS S3 endpoint as the immutable
block store. The test performs multi-chunk binary writes, partial overwrites,
truncate/extend operations, fresh provider reopen, metadata revision CAS, and
stale-fence rejection.

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
network, and cleanup for that run. An externally supplied cluster is an
explicit diagnostic mode only and must pass the opt-in and identity checks
documented by the provider README; the composition still uses a unique
`RUSTFS_COMBO_PREFIX` namespace.
