# mount-rs

Rust port of the filesystem driver contract behind
[pithings/mountx](https://github.com/pithings/mountx).

The project is being built around one observable contract: operations issued
through the loopback harness must have the same results regardless of whether
the bytes live in memfs, SQLite, Cloudflare R2, or PGlite. The first milestone
provides the async driver API, POSIX path and error behavior, a full in-memory
driver, and persisted drivers that use the same filesystem model.

## Backends

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
