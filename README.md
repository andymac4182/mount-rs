# mount-rs

**Delivery progress:** [Workstream and task tracker](WORK_TRACKER.md) — landed
work, active implementation, remaining acceptance tests and blockers.

Rust port of the filesystem driver contract behind
[pithings/mountx](https://github.com/pithings/mountx).

The project is being built around one observable contract: operations issued
through the loopback harness must have the same results regardless of whether
the bytes live in memfs, SQLite, Cloudflare R2, or PGlite. The first milestone
provides the async driver API, POSIX path and error behavior, a full in-memory
driver, and persisted drivers that use the same filesystem model.

For crate responsibilities and extension points, see the
[code architecture guide](docs/code-architecture.md). The
[storage and durability guide](ARCHITECTURE.md) records publication ordering
and safety boundaries; the [documentation index](docs/README.md) links to
acceptance and operations records.
For a native NFS mount and its exact Finder path on macOS, see the
[macOS usage guide](docs/macos-usage.md).

## Backends

- `ChunkedFs` — composes independently selected metadata and immutable block
  providers with persisted fixed-size chunking, coordinated writers and ordered
  durability barriers. Memory, SQLite and PGlite implement both provider roles;
  R2 and AWS S3 provide block storage. Each provider and filesystem has its own
  crate under `providers/` or `filesystems/`.
- `MemoryFs` (`mount-rs-memfs`) — memfs-style in-memory filesystem with handles,
  links, symlinks, rename, timestamps, and special-node metadata.
- `SqliteFs` (`mount-rs-sqlite-fs`) — persisted state in SQLite, including an
  in-memory SQLite mode for fast tests.
- `R2Fs` (`mount-rs-r2-fs`) — persisted state in any S3-compatible object store.
  The `R2Config` builder targets Cloudflare R2 endpoints and credentials.
- `PgliteFs` (`mount-rs-pglite-fs`) — persisted state through PostgreSQL wire protocol. PGlite's
  official socket server makes it usable from Rust without embedding a JS
  runtime.

The workspace includes separate FUSE, 9P, NFS, WebDAV, S3, HTTP, native
auto-mount, and CLI crates. Behavioral and native-platform acceptance remain in progress.
The full upstream surface is tracked explicitly in
[`PORTING_STATUS.md`](PORTING_STATUS.md) and remains part of the active porting
goal.

Additional delivery requirements are tracked in [`REQUIREMENTS.md`](REQUIREMENTS.md).
Safe hosting of SQLite database files on mounts is required. Linux FUSE tests
have passed actual DELETE/WAL transactions, competing processes, killed SQLite
writers, reopen, mount-service crashes and injected backend failures on split
SQLite stores; other platform/backend combinations remain acceptance work. See the revision-specific
evidence in `PORTING_STATUS.md`, not a blanket production-safety claim.
Native macOS NFS tests also cover single-host DELETE-journal SQLite transactions
and reopen. SQLite selects DELETE when WAL is requested on that mount: this is
reported as unsupported WAL, not a passing WAL test or distributed-locking proof.
Copy-on-write and additional chunking algorithms are future work. The older
`SqliteFs`, `R2Fs` and `PgliteFs` factories retain transitional snapshot storage.

## Node split-store API

After building the local napi-rs package:

```js
const { createChunkedDriver } = require('./bindings/mount-rs-napi');
const fs = await createChunkedDriver({
  metadata: { kind: 'sqlite', uri: './metadata.sqlite' },
  blocks: { kind: 'sqlite', uri: './blocks.sqlite' },
  chunkSize: 65536,
});
try {
  await fs.writeFile('/hello', Buffer.from('hello'));
  console.log(Buffer.from(await fs.readFile('/hello')).toString());
} finally {
  await fs.shutdown(); // release the writer lease explicitly
}
```

PGlite providers require `uri` and a volume `key`; durability defaults to false
unless the caller explicitly asserts a persistent server configuration. R2
blocks require an isolated prefix `key`, `endpoint`, `bucket`, `accessKeyId`,
and `secretAccessKey`. Keep credentials outside source control.

The Node package additionally exposes memory/host/Unstorage driver factories,
native mounting and probes, and NFS/9P/S3/WebDAV server factories. The Unstorage
adapter forwards JavaScript storage callbacks to the Rust KV driver; it is not
a JavaScript filesystem reimplementation. Always await `shutdown()` when done:
it releases provider connections, writer leases, or retained JavaScript callback
references as appropriate. A retained Unstorage driver intentionally keeps its
callbacks alive until shutdown, so omitting shutdown can keep Node running.
Close server instances before shutting down the filesystem they serve.

For single-host SQLite over NFS, opt in with
`mount(fs, path, { transport: "nfs", nfsSqliteSingleHost: true })`.
This selects hard mounts and local-only locks; general mount defaults remain
unchanged. The profile is NFSv3-only and does not promise WAL, cross-host
locking, or power-loss durability. Native macOS acceptance currently covers
DELETE journaling; unsupported modes remain acceptance gaps.

Architectural inspiration and benchmark references are listed in
[`REFERENCES.md`](REFERENCES.md). The dependency-light
[`storage benchmark runner`](benchmarks/storage/README.md) measures
ComputeSDK-aligned write/full-read/delete workloads with payload validation,
raw samples, and explicit backend/cleanup status. Run
`MOUNTX_SOURCE=/path/to/pinned/mountx node benchmarks/storage/runner.mjs --smoke`.
Remote, mounted-path, and small-operation dispatch performance acceptance
remains tracked in [`REQUIREMENTS.md`](REQUIREMENTS.md).

The Rust CLI is a thin consumer of `mount-rs-sdk`: its mount-free SDK
contract can be exercised with `cargo run -p mount-rs-cli -- sdk-self-test`
or a durable config with `--reopen`. The matching Node CLI example is
`node examples/node-cli/index.mjs --sdk-self-test`; both are covered by the
provider/consumer integration matrix before native transport tests are run.

## Development

Use the repository wrapper for normal Cargo commands so all mount-rs
worktrees share one per-user target directory instead of rebuilding into
separate `target/` trees:

```sh
./scripts/cargo-shared test --workspace --all-targets --locked
./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings
```

The default is `~/Library/Caches/mount-rs/cargo-target` on macOS and
`$XDG_CACHE_HOME/mount-rs/cargo-target` (or `~/.cache/mount-rs/cargo-target`)
elsewhere. Set `MOUNT_RS_CARGO_TARGET_DIR` to choose a different shared
location. An explicit `CARGO_TARGET_DIR` still wins for gates that need an
isolated target.

```sh
./scripts/cargo-shared fmt --all -- --check
./scripts/cargo-shared test --workspace --all-targets

# Differential tests against a checkout of pithings/mountx.
MOUNTX_SOURCE=/path/to/mountx node scripts/check-parity.mjs
MOUNTX_SOURCE=/path/to/mountx node scripts/check-edge-parity.mjs

# Build and smoke-test the Node.js package.
pnpm --dir bindings/mount-rs-napi install --frozen-lockfile
pnpm --dir bindings/mount-rs-napi build
pnpm --dir bindings/mount-rs-napi test

# Run the complete local acceptance gate.
MOUNTX_SOURCE=/path/to/mountx ./scripts/test-all.sh
```

The TypeScript oracle used by the differential tests is the upstream `mountx`
checkout. `scripts/test-all.sh` also starts the official PGlite socket server,
runs the live PGlite integration test, and runs the live R2 test when all four
R2 credential variables are present. Without R2 credentials it reports that
gate as skipped rather than silently treating the in-memory object-store test
as Cloudflare verification.

With R2 credentials configured, the gate also runs all seeded TypeScript
oracle traces against the live bucket. These use unique objects beneath
`mount-rs-tests/trace-*`, never `R2_STATE_KEY`, and delete only their own
snapshot after execution. Use a dedicated test bucket. Live credentials are
still required; local object-store results are not live R2 evidence.

Live R2 and PGlite checks are opt-in because they require credentials or a
running PGlite socket server:

```sh
R2_ENDPOINT=... R2_ACCESS_KEY_ID=... R2_SECRET_ACCESS_KEY=... \
  R2_BUCKET=... cargo test --test backend_parity cloudflare_r2_matches -- --ignored --nocapture

PGLITE_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test --test backend_parity pglite_matches -- --ignored --nocapture
```

Live checks are explicitly ignored in ordinary Cargo runs, never counted as
passing without executing. `sh scripts/test-acceptance.sh` requires R2
configuration up front and runs the full local backend gate.

To record credential-safe live service evidence for a review, run
`sh scripts/r2-service-evidence.sh` with the four `R2_*` variables supplied by
the caller's environment or local Keychain-backed wrapper. It performs only a
read-only bucket probe and prints the R2 endpoint authority, bucket, repository
revision, and dirty-entry count; it never prints credential values.
