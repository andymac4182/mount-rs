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
  without a kernel mount; separate Linux kernel tests also exercise mounts.
- Workspace-integrated 9P2000.L, NFSv3, WebDAV, and S3 gateway crates with
  userspace protocol and loopback network tests.
- NFSv4.1 sessions, filesystem operations, stateids and byte-range locks have
  userspace wire coverage at `0ff3ff8`; actual Linux NFS client CI is newly
  enabled and remains an acceptance gate. Unsupported protocol operations
  explicitly fail; this is not a claim of complete RFC coverage.
- Seeded differential traces: five seeds, each with 621 operations against the TypeScript memory
  oracle for Rust memory, SQLite, local object-store, and real local PGlite.
  This caught and fixed PGlite named prepared-statement collisions across
  reconnecting clients; the adapter now uses typed unnamed statements.
  Expanded seeds also caught a symlink-resolved directory rename error mismatch;
  descendant checks now use resolved paths. Snapshot restore rejects inconsistent
  inode graphs before making them available to filesystem operations.
- Core independent metadata/block contracts and fixed-size chunker with persisted
  algorithm/version/configuration. Separate volatile memory and SQLite providers
  now implement fenced writer leases, revision CAS and immutable blocks. The
  composed chunked driver now passes seven mixed-store/fault/sparse integration
  tests and all five 621-operation TypeScript oracle seeds across memory,
  SQLite and local object-store block combinations. Actual SQLite hosting on
  the split stores is newly wired into Linux CI, not yet verified there.
  Legacy Node factories retain transitional snapshot persistence; the new
  `createChunkedDriver` factory independently selects providers and has local
  Node integration coverage including real PGlite.
- PGlite split providers have isolated-server fencing/CAS/immutable-block tests,
  three mixed-store composition cases and five 621-operation differential
  seeds. Volatile is the default durability policy; persistent configurations
  require an explicit caller assertion.
- A separate CLI crate provides help/probe/mount, driver/transport selection,
  request logging and signal-triggered unmount, with 19 passing local tests.
  Native CLI lifecycle and remaining upstream presentation/cleanup behavior
  still require acceptance.
- Automatic native transport selection and explicit overrides are implemented
  in a separate crate with five passing selection/options tests. An actual
  Linux FUSE facade test is wired into CI.

## Revision-specific verification checkpoints

- `a19f297`: [native macOS NFS job 106006016122](https://github.com/andymac4182/mount-rs/actions/runs/35483700074/job/106006016122)
  passed the expanded real NFSv3 namespace/handle test (one passed, zero
  ignored). The same revision's Linux NFSv4.1 mount timed out before I/O;
  wire tests had not exposed that failure. Native v4.1 remains incomplete.
- Local macOS WebDAV native read/write/unmount passed after canonicalizing the
  test mountpoint (`/var` versus `/private/var`); `5c2e74e` adds macOS/Linux
  WebDAV native CI. Hosted results remain separate evidence.
- The host-driver differential harness covers 112 operations, including flags,
  handle cursors and errors, links and rooted paths. It caught two unwanted
  error `path` fields; after correction the local macOS oracle passes.
- `e15179d`: [native FUSE job 106005427389](https://github.com/andymac4182/mount-rs/actions/runs/35483491213/job/106005427389)
  passed both real mount-service graceful restart and SIGKILL recovery tests
  (two passed, zero ignored, 21.02s). Separate SQLite metadata/block files
  preserved committed DELETE/WAL databases across fresh service processes and
  production lease expiry. This does not simulate host power loss or injected
  backend failures. See [`ARCHITECTURE.md`](ARCHITECTURE.md) for scope.
- `91577ab`: [native FUSE job 106004365456](https://github.com/andymac4182/mount-rs/actions/runs/35483110170/job/106004365456)
  passed actual SQLite DELETE/WAL hosting and reopen on separate metadata and
  block SQLite databases in 4.47s, plus the real auto-facade mount. This
  revision's overall CI failed because two new fault-test initializers were
  staged during an overlapping worker edit; the subsequent fixture correction
  and expanded fault tests require a fresh green run.
- `0ff3ff8`: [native NFS job 106003944143](https://github.com/andymac4182/mount-rs/actions/runs/35482956567/job/106003944143)
  passed one actual Linux NFSv3 mount/read/write/unmount test, zero ignored.
  This is not native v4.1 evidence.
- `6f8ba04`: [CI run 35482470541](https://github.com/andymac4182/mount-rs/actions/runs/35482470541)
  passed the entire platform, Node packaging and then-enabled native suite.
- `3ebf501`: [CI run 35481387472](https://github.com/andymac4182/mount-rs/actions/runs/35481387472)
  passed all eight jobs: Rust on macOS/Linux, Node on both platforms and both
  arm64/x64 architectures, actual Linux FUSE mounts, and aggregation/packaging
  of all four native artifacts. Packaging checks do not publish to npm.
- `81f890b`: [CI run 35480606094](https://github.com/andymac4182/mount-rs/actions/runs/35480606094)
  passed actual kernel-mounted file operations over memory, SQLite persistence,
  local object storage, and real PGlite, including backend connection reopen.
  Local object storage is not live Cloudflare R2 evidence.
- `bac1fcc`: SQLite split-store local tests cover independent database files,
  reopen, stale/expired writer rejection, monotonic fences, revision conflicts,
  immutable bytes and database integrity. `9082196` adds five passing local
  volatile-store tests; neither checkpoint by itself proves composed-driver safety.
- Actual SQLite-process testing was added at `a252361`: DELETE and WAL modes,
  FULL synchronous, competing processes, forced dirty-page spills, killed writers,
  reopen and integrity checks. The host-filesystem control passed. The first
  Linux mounted run failed during contender setup under a correctly held lock;
  `2eda510` moves contender setup before the writer starts. `7006719` bounds
  the dirty-page spill workload to 128 KiB (still exceeding the 32 KiB cache)
  after the transitional snapshot backend exceeded the original deadline.
  [Native job 106001538402](https://github.com/andymac4182/mount-rs/actions/runs/35482069213/job/106001538402)
  then passed actual SQLite 3.45.1 DELETE/WAL process recovery on both mounted
  memfs and SQLite snapshot persistence, with FULL synchronization, in 30.41s.
  Mount-service crashes, backend faults,
  power loss and the final split-store architecture are not covered by this probe.
- `4150b5d`: [native Linux 9P job 106000928554](https://github.com/andymac4182/mount-rs/actions/runs/35481837691/job/106000928554)
  passed both actual mounted I/O/lifecycle and external-unmount tests, zero
  ignored. Ordinary userspace tests do not substitute for that job.

## Still required before the overall porting goal is complete

- Replace the transitional whole-filesystem snapshot persistence architecture
  with independently composable metadata and block stores, and pluggable data
  chunking with fixed-size chunks initially and an extensible algorithm interface.
  Mixed-store correctness and durability
  must be verified; see [`REQUIREMENTS.md`](REQUIREMENTS.md).
- Safely host real SQLite database files inside mounted filesystems, including
  concurrency, locking, journal/WAL behavior, synchronization, and crash recovery.
  The detailed gate is in [`REQUIREMENTS.md`](REQUIREMENTS.md). SQLite-as-backend
  tests do not satisfy this additional requirement. Copy-on-write is tracked in
  that file as future work, not a currently implemented feature.
- Keep revision-matched macOS and Linux CI green for Rust, Node addon builds,
  and backend/transport integration tests. The initial checkpoint `877afb6`
  passed all four hosted jobs in [CI run 35479549553](https://github.com/andymac4182/mount-rs/actions/runs/35479549553).
  This covers userspace transport tests, not real kernel mounts; subsequent
  changes require their own hosted evidence.
- Linux rootless FUSE mounted-file write/read/readdir/unmount passed for
  checkpoint `24af979` in [native FUSE job 105996579367](https://github.com/andymac4182/mount-rs/actions/runs/35480223176/job/105996579367).
  Its log confirms one actual native test passed, zero ignored. This does not
  establish native NFS/9P/WebDAV behavior or mounted persistent-backend coverage.
- Run the live Cloudflare R2 gate with the target bucket and credentials.
- Port and test the upstream transport/server layers: FUSE, 9P, NFS,
  WebDAV, and S3 gateway behavior.
- Add transport-specific differential/conformance tests once each transport is
  implemented.
- Complete the remaining upstream surface, including NFSv4.1 native acceptance, native mount
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
