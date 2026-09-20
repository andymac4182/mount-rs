# mountx Rust port status

The active porting goal is intentionally acceptance-driven: a milestone is not
complete until the Rust result matches the TypeScript oracle and every enabled
backend passes the same integration scenario.

## Implemented, with partial verification

This list records implementation present in the worktree, not a claim of full
upstream parity or production readiness. Differential cases cover only the
behaviors explicitly exercised; all remaining upstream operations and edge
cases still require audit.

- Core async `FsDriver` and `FileHandle` contracts.
- POSIX paths, error codes, open modes, metadata, timestamps, links,
  symlinks, rename, special nodes, handles, and `statfs`.
- Memfs implementation with snapshot/restore support.
- Separate persistence and backend crates for SQLite, S3-compatible/R2, and
  PGlite-over-PostgreSQL-wire.
- Separate napi-rs crate and Node package exposing memory, SQLite, PGlite, and
  R2 factories.
- Differential oracle scenarios covering ordinary operations, append/truncate,
  metadata, special nodes, symlink loops, and `statfs`.
- Reproducible local gate: `scripts/test-all.sh`.
- FUSE framing, inode mapping, and driver-backed lookup, attributes, file I/O,
  create, sync, statfs, metadata changes, directory/link mutation requests,
  READDIRPLUS and lookup-reference accounting. Session integration tests run
  without a kernel mount; native lifecycle work is in progress.
- Workspace-integrated 9P2000.L, NFSv3, WebDAV, and S3 gateway crates with
  userspace protocol and loopback network tests.
- Seeded differential traces: five seeds, each with 621 operations against the TypeScript memory
  oracle for Rust memory, SQLite, local object-store, and real local PGlite.
  This caught and fixed PGlite named prepared-statement collisions across
  reconnecting clients; the adapter now uses typed unnamed statements.
  Expanded seeds also caught a symlink-resolved directory rename error mismatch;
  descendant checks now use resolved paths. Snapshot restore rejects inconsistent
  inode graphs before making them available to filesystem operations.

## Still required before the overall porting goal is complete

- Keep revision-matched macOS and Linux CI green for Rust, Node addon builds,
  and backend/transport integration tests. The initial checkpoint `877afb6`
  passed all four hosted jobs in [CI run 35479549553](https://github.com/andymac4182/mount-rs/actions/runs/35479549553).
  This covers userspace transport tests, not real kernel mounts; subsequent
  changes require their own hosted evidence.
- Run the live Cloudflare R2 gate with the target bucket and credentials.
- Port and test the upstream transport/server layers: FUSE, 9P, NFS,
  WebDAV, and S3 gateway behavior.
- Add transport-specific differential/conformance tests once each transport is
  implemented.
- Complete the remaining upstream surface, including NFSv4.1, native mount
  wrappers and probes, the auto-mount facade, CLI, and driver adapters absent
  from the current Rust workspace. Existing transport success is not evidence
  for these missing components.
- Verify platform packaging and Node API/export completeness against upstream;
  building a local `.node` binary alone is not distribution acceptance.

The goal remains active until those gates are closed. The in-memory object-store
test validates the R2 adapter without making a claim about a live Cloudflare
account. Live backend tests are explicitly ignored in ordinary Cargo runs and
fail without configuration when invoked with `--ignored`. The strict
`scripts/test-acceptance.sh` gate requires R2 configuration before starting.
