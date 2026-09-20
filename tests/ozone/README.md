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
service.

## Contract coverage

The real gateway tests cover:

- immutable block publication with full and range reads;
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
`MOUNT_RS_OZONE_TEST_TIMEOUT_SECONDS` (600 seconds).

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

The tests use seven-byte chunks and exercise multi-chunk writes, partial
writes, shrink and extend truncation, range reads, provider CAS conflicts,
expired-writer fencing, fresh `ChunkedFs` reopen, and deletion of every test
block object. The composition harness also removes its PGlite data directory
and SQLite file. It never replaces Ozone or PGlite with an in-memory or mock
service.

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

TiDB and FoundationDB are intentionally not added to this Ozone packet: their
existing real composition harnesses are RustFS-specific and no Ozone-aware
real-service composition was available here. Their separate gates remain the
evidence for those providers.
