# mount-rs

Rust port of the filesystem driver contract behind
[pithings/mountx](https://github.com/pithings/mountx).

The project is being built around one observable contract: operations issued
through the loopback harness must have the same results regardless of whether
the bytes live in memfs, SQLite, Cloudflare R2, or PGlite. The first milestone
provides the async driver API, POSIX path and error behavior, a full in-memory
driver, and persisted drivers that use the same filesystem model.

## Backends

- `ChunkedFs` — composes independently selected metadata and immutable block
  providers with persisted fixed-size chunking, fenced writers and ordered
  durability barriers. Memory, SQLite and PGlite implement both provider roles;
  R2 provides block storage. Each integration is a separate crate.
- `MemoryFs` — memfs-style in-memory filesystem with handles, links, symlinks,
  rename, timestamps, and special-node metadata.
- `SqliteFs` — persisted state in SQLite, including an in-memory SQLite mode for
  fast tests.
- `R2Fs` — persisted state in any S3-compatible object store. The `R2Config`
  builder targets Cloudflare R2 endpoints and credentials.
- `PgliteFs` — persisted state through PostgreSQL wire protocol. PGlite's
  official socket server makes it usable from Rust without embedding a JS
  runtime.

The storage layer is deliberately correctness-first while the port is being
validated. The public API is small and backend-neutral so transport layers can
be added after the driver behavior is locked down by differential tests.

The current milestone is the driver, persistence, backend, and N-API layer.
The full upstream transport surface is tracked explicitly in
[`PORTING_STATUS.md`](PORTING_STATUS.md) and remains part of the active porting
goal.

Additional delivery requirements are tracked in [`REQUIREMENTS.md`](REQUIREMENTS.md).
Safe hosting of SQLite database files on mounts is required. Linux FUSE tests
have passed actual DELETE/WAL transactions, competing processes, killed SQLite
writers, reopen, mount-service crashes and injected backend failures on split
SQLite stores; other platform/backend combinations remain acceptance work. See the revision-specific
evidence in `PORTING_STATUS.md`, not a blanket production-safety claim.
Copy-on-write and additional chunking algorithms are future work. The older
`SqliteFs`, `R2Fs` and `PgliteFs` factories retain transitional snapshot storage.

## Node split-store API

After building the local napi-rs package:

```js
const { createChunkedDriver } = require('./integrations/mount-rs-napi');
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

The CLI can be run with `cargo run -p mount-rs-cli -- --help` or `-- probe`.

## Development

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets

# Differential tests against a checkout of pithings/mountx.
MOUNTX_SOURCE=/path/to/mountx node scripts/check-parity.mjs
MOUNTX_SOURCE=/path/to/mountx node scripts/check-edge-parity.mjs

# Build and smoke-test the Node.js package.
pnpm --dir integrations/mount-rs-napi install --frozen-lockfile
pnpm --dir integrations/mount-rs-napi build
pnpm --dir integrations/mount-rs-napi test

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
