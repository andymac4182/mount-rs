# P0 provider/consumer matrix

This directory is an isolated, bounded consumer harness. It is not part of the
root Cargo workspace and does not change source, site, benchmark, tracker, or
history files.

The simple-first operation is the same in every executable: create one
\`/provider-matrix/value\` file, write a fixed binary payload, read it back,
check its size, and close the consumer. The Rust chunked rows also validate the
published namespace and flush path. The live R2 Rust rows track and remove only
objects created below their run-owned prefix.

| Provider path | Rust SDK | Node SDK | CLI |
| --- | --- | --- | --- |
| direct memfs | \`memfs\` | \`memfs\` factory | memory config/runtime |
| memory metadata + memory blocks | \`memory/memory\` | \`chunked-memory/memory\` | memory config/runtime |
| SQLite metadata + SQLite blocks | \`sqlite/sqlite\` using \`:memory:\` | \`chunked-sqlite/sqlite\` using \`:memory:\` plus the SQLite factory | SQLite config/runtime |
| PGlite metadata + PGlite blocks | gated by \`PGLITE_DATABASE_URL\` or \`MOUNT_RS_PGLITE_URL\` | gated by \`PGLITE_DATABASE_URL\` | config validation only |
| memory/SQLite metadata + Cloudflare R2 blocks | gated by all four \`R2_*\` variables | \`Filesystem.r2\` factory gated by all four \`R2_*\` variables | config validation only |
| PGlite metadata + Cloudflare R2 blocks | gated by both PGlite and R2 | not duplicated here | config validation only |

The PGlite rows use the repository's existing PostgreSQL-wire fixture contract;
the harness does not start a server or invent a connection string. The R2
rows use the repository's existing environment contract and never print
endpoint, bucket, access key, secret, or connection-string values. A missing
gate is a visible \`SKIP\`, not a pass.

## Focused commands

Run from the repository root:

\`\`\`sh
cargo fmt --manifest-path tests/provider_matrix/Cargo.toml -- --check
cargo run --manifest-path tests/provider_matrix/Cargo.toml --offline --locked
node tests/provider_matrix/node-sdk.mjs
node tests/provider_matrix/cli.mjs
\`\`\`

The CLI matrix also runs the portable Rust-backed Node CLI SDK self-test. The
Rust CLI runtime rows construct filesystems through `mount-rs-sdk`; native
mount self-tests remain explicit platform gates because they require a usable
FUSE/NFS transport.

The Node SDK command expects the checked-out native addon at
\`integrations/mount-rs-napi/mount-rs.darwin-arm64.node\` (or the corresponding
platform build). The CLI command builds/runs only the focused CLI package
commands it needs and captures child output so provider values are not echoed.

For the existing local PGlite fixture, start
\`tests/pglite/server.mjs\` using the repository's pinned \`tests/pglite\`
dependencies, set \`PGLITE_DATABASE_URL\` to that socket/TCP URL in the test
shell, and rerun the Rust and Node commands. The fixture setup remains
explicit and outside this matrix.

For live Cloudflare R2 coverage, export the repository's existing
\`R2_ENDPOINT\`, \`R2_BUCKET\`, \`R2_ACCESS_KEY_ID\`, and
\`R2_SECRET_ACCESS_KEY\` variables in the test shell and rerun the Rust and
Node commands. Use a dedicated test bucket. The R2 Rust rows are the
block-provider coverage; the Node row is the public legacy \`Filesystem.r2\`
factory with exact-key cleanup. Local \`object_store::memory::InMemory\`
behavior is intentionally not counted as Cloudflare evidence.

## Result interpretation

Each runner emits \`PASS\`, \`SKIP\`, or \`FAIL\` lines and exits nonzero on a
failure. \`validate-config\` is deliberately static: the CLI PGlite/R2 config
row proves schema and provider selection only, and does not prove service or
credential access. Native mounting and full live CLI/R2 acceptance remain the
repository's existing opt-in acceptance gates.
