# Real RustFS integration gate

This directory contains the local, real-S3 integration package used by
scripts/test-rustfs.sh. It exercises mount-rs-r2::R2BlockStore against an
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

The RustFS container is single-node/single-disk test storage. It is not a
durability or power-loss claim, and it does not replace live Cloudflare R2
verification. Storage benchmark workloads may consume an explicitly retained
endpoint later; this package does not fabricate or publish benchmark results.
