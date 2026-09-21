# W01-NFS progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md)

Owner: delegated W01-NFS task (thread `01a0c456-a28e-7cb3-9b48-a3d23e7ec8c0`)

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns NFSv3/MOUNT, NFSv4.1, the shared session/router boundary,
connection lifecycle, native mounts, and NFS-specific durability and handle
semantics.

| Gate | State | Required evidence |
| --- | --- | --- |
| NFSv3/MOUNT public API and wire behavior | In progress | Codec, rootless wire, session, error, generated N-API, and shared-handle-view evidence |
| NFSv4.1 router/session behavior | In progress | Version routing, COMPOUND/state matrix, shared handle/counter proof, and direct N-API view |
| Connection and server lifecycle | In progress | Active connections, malformed/EOF/reset handling, close, async disposal, and restart evidence |
| macOS/Linux native NFS | External gate | Actual supported-client mount/read/write/unmount and cleanup results |
| Handles, concurrency, crash and durability | Open | Cross-version handle lifetime, cancellation/close, crash/restart, ordering, and persistence evidence |

## Current queue

- Complete the remaining upstream NFS connection-object surface and direct
  session/member parity.
- Exercise the full v3/v4 behavior matrix, including stateful v4 operations,
  malformed records, reconnects, and version negotiation.
- Keep the macOS v3 native result current; execute the separate privileged
  Linux v4.1 native lane and keep both platform results classified as external
  gates.
- Qualify cross-process crash, cancellation/close, concurrency, and durability
  behavior.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | v3/v4 shared-state foundation | N-API v3/v4 direct routing and `Nfs4Session` view pass; the Rust server shares the v3/v4 handle table, path lock, and counters; NFS crate and host-enabled package regressions pass | Direct shared-handle inspection, complete connection-object parity, native/hosted lifecycle, and crash/durability gates |
| 2026-09-22 | active connections and shared handle inspection | Rust `NfsServer::connections()` now reports active accepted socket tasks and close awaits their teardown; N-API `NfsServer.connections`, `NfsSession.handles`, and `Nfs4Session.handles` are generated and exercised with BigInt handle identity. Rust tests pass 31 unit, rootless wire 1, transport errors 4, v4 barrier 1, and v4 wire 2; release N-API build, generated typecheck, and live server integration pass | Full v3/v4 stateful matrix, upstream connection-object parity, native/hosted lifecycle, and crash/durability gates |
| 2026-09-22 | macOS native NFSv3 loopback | The opt-in `native_loopback_mount_round_trip` gate passed 1/1 in 0.09s, including the temporary native mount, filesystem round trips, and bounded cleanup | Privileged Linux NFSv4.1, hosted lifecycle, full stateful matrix, connection-object parity, and crash/durability gates |

## Exact commands and gate boundaries

- `./scripts/cargo-shared test -p mount-rs-nfs --all-targets --locked` — PASS:
  31 unit tests, rootless wire 1, transport errors 4, v4 commit barrier 1,
  and v4 wire 2; the native mount target remains 1 explicitly ignored test.
- `(cd integrations/mount-rs-napi && pnpm build)` — PASS: release addon and
  generated declarations rebuilt.
- `(cd integrations/mount-rs-napi && node test/typecheck.mjs)` — PASS.
- `(cd integrations/mount-rs-napi && node test/servers.mjs)` — PASS: live NFS
  v3/v4 routing, shared handle snapshots, active connection count, malformed
  record reporting, close, and destroyed-state cleanup.
- `(cd integrations/mount-rs-napi && node test/nfs-codec.mjs)` — SKIP because
  `MOUNTX_SOURCE` was not configured; this is not acceptance evidence.
- `MOUNT_RS_NFS_NATIVE_TEST=1 ./scripts/cargo-shared test -p mount-rs-nfs --test native_mount -- --ignored --exact native_loopback_mount_round_trip --nocapture` — PASS: macOS native NFSv3 loopback mount, filesystem round trips, unmount, and bounded cleanup; 1 passed, 0 failed, 0.09s.
- Linux v4.1 native qualification remains an external gate and was not run on
  this macOS host; it requires the separate
  `MOUNT_RS_NFS_NATIVE_V4_TEST=1` lane plus a privileged Linux NFS client.

## Completion rule

The owning task may mark W01-NFS production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
