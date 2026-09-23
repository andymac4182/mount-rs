# Real RustFS integration gate

This directory contains the local, real-S3 integration package used by
scripts/test-rustfs.sh. It exercises mount-rs-rustfs::RustFsBlockStore against an
actual RustFS container. The tests refuse non-loopback endpoints, so a local
RustFS run cannot silently become live Cloudflare R2 evidence. The separate
live-R2 gates remain required.

## Pinned service

The harness uses the official RustFS release image:

    rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff

That is the multi-architecture manifest digest. Docker selects the official
linux/amd64 or linux/arm64 child image for the host. The release and image
are published by RustFS:

* https://github.com/RustFS/RustFS/releases
* https://hub.docker.com/r/rustfs/rustfs/tags

The script checks Docker availability but never installs Docker or starts a
global daemon. It creates only a test-owned loopback container, temporary data
directory, bucket, and object prefix. Cleanup removes those exact resources by
default, including after a failed test. MOUNT_RS_RUSTFS_KEEP=1 is an explicit
debug/benchmark handoff option and prints the retained endpoint and data path.

RustFS is started with non-default, test-only RUSTFS_ACCESS_KEY and
RUSTFS_SECRET_KEY values. The authenticated bucket bootstrap uses the Node
standard library to sign one CreateBucket request; no AWS SDK, mc, or
additional image is required.

## Run

From the repository root:

    ./scripts/test-rustfs.sh

The script waits for the RustFS /health endpoint, creates the isolated
bucket, runs the real block contract, restarts the same container over the
same test-owned data directory, waits for readiness again, and runs the fresh
client reopen assertion. The contract covers immutable publication, stale
conditional reads/writes, full and range reads, missing-object errors,
concurrent puts, exact-ID cleanup, and persistence across service restart.
It also opens and reopens the public Rust SDK split-store factory with SQLite
metadata and RustFS blocks.

For two independent writable macOS NFS mounts backed by the same disposable
FoundationDB volume and RustFS prefix, supply matching arm64 FoundationDB 7.4
server, CLI, and client library paths. The RustFS harness provides a fresh
container; the combo runner provides a fresh loopback FoundationDB server and
checks exact mount cleanup:

```sh
MOUNT_RS_NATIVE_FDB_SERVER=/path/to/fdbserver \
MOUNT_RS_NATIVE_FDB_CLI=/path/to/fdbcli \
MOUNT_RS_NATIVE_FDB_CLIENT_LIB_DIR=/path/to/lib \
RUSTFS_COMBO_NAME=native-two-process-fdb-rustfs \
RUSTFS_COMBO_TIMEOUT_SECONDS=900 \
RUSTFS_COMBO_GROUP_STOP_SECONDS=45 \
RUSTFS_COMBO_COMMAND='python3 ./scripts/test-native-fdb-rustfs-two-process.py' \
./scripts/test-rustfs.sh
```

The native case runs two separate CLI processes through two NFS mounts,
synchronizes 20 unique 4 KiB files per writer, reads all 40 from both mounted
views, merges disjoint same-file writes, checks rename and removal, and reads
every acknowledged file after a third CLI reopens. It emits
`NATIVE_FDB_RUSTFS_LOAD_PASS acknowledgements=40 elapsed_ms=...` and
`NATIVE_FDB_RUSTFS_TWO_PROCESS_PASS` only after these checks pass. This covers
two processes on one Mac; it does not measure separate hosts or production
durability.

The native runner uses a private 1 GiB sparse APFS image for its FoundationDB
data directory. FoundationDB can reject writes on a large, nearly full host
volume because of its 5% free-space margin, even when several GiB remain.
Readiness requires a committed test-owned key and a fresh read of that value;
`status minimal` alone can say the database is available while writes fail.
The image is attached without opening Finder and is detached by its exact
owned device after the CLI, NFS, and server processes stop. A failed detach
preserves the private run directory and image for diagnosis.

Before SQLite, PGlite, or FoundationDB metadata enters concurrent revision-CAS
mode, a RustFS block store built from a signed `RustFsConfig` conditionally
creates `<prefix>/_mount-rs-concurrent-probe-v1` and reads its exact bytes back
through a separate signed client. This write/read checks the bucket, keys, and
prefix before the persistent mode conversion. Direct `RustFsBlockStore::new`
wrappers over arbitrary object stores cannot opt in. The immutable probe is
one small retained object per concurrent prefix; it is outside normal block
reconciliation and is removed only by explicit prefix or bucket cleanup. The
real-service preflight test verifies valid reuse, wrong credentials, a missing
bucket, and a colliding object before SQLite mode conversion.

`RUSTFS_COMBO_GROUP_STOP_SECONDS=45` allows the native runner to detach both
run-owned NFS mountpoints after an interrupted test. The no-service dry cleanup
probe is `python3 scripts/test-native-fdb-rustfs-runner-cleanup.py`; it also
checks image detach failure and that an unrelated Finder image is untouched.

The RustFS container is single-node/single-disk test storage. It is not a
durability or power-loss claim, and it does not replace live Cloudflare R2
verification. Storage benchmark workloads may consume an explicitly retained
endpoint later; this package does not fabricate or publish benchmark results.
