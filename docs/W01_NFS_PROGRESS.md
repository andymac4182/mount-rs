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
| NFSv4.1 router/session behavior | In progress | Version routing, COMPOUND/state matrix, shared handle/counter proof, direct N-API view, and bounded channel/state knobs |
| Connection and server lifecycle | In progress | Active connection objects/counts, malformed/EOF/reset handling, close/wait, async disposal, and restart evidence |
| macOS/Linux native NFS | External gate | Actual supported-client mount/read/write/unmount and cleanup results |
| Handles, concurrency, crash and durability | In progress | Cross-version handle lifetime, bounded LRU/pinning, cancellation/close, crash/restart, ordering, and persistence evidence |

## Current queue

- Reconcile the remaining upstream NFS connection-object surface and direct
  session/member parity; the local live-client object/close boundary, bounded
  `maxHandles`/NFSv4 pinning policy, bounded v4 channel/state knobs, and
  deterministic static NFSv4 owner map, lease expiry enforcement, and
  request-level `onError` callback now pass. Dynamic owner callbacks and the
  N-API `now` bridge now also pass their Rust and live N-API evidence.
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
| 2026-09-22 | v3/v4 shared-state foundation | N-API v3/v4 direct routing and `Nfs4Session` view pass; the Rust server shares the v3/v4 handle table, path lock, and counters; NFS crate and host-enabled package regressions pass | Direct shared-handle inspection, remaining upstream connection/session member parity, native/hosted lifecycle, and crash/durability gates |
| 2026-09-22 | active connections and shared handle inspection | Rust `NfsServer::connections()` now reports active accepted socket tasks and close awaits their teardown; N-API `NfsServer.connections`, `NfsSession.handles`, and `Nfs4Session.handles` are generated and exercised with BigInt handle identity. Rust tests pass 31 unit, rootless wire 1, transport errors 4, v4 barrier 1, and v4 wire 2; release N-API build, generated typecheck, and live server integration pass | Full v3/v4 stateful matrix, remaining upstream connection/session member parity, native/hosted lifecycle, and crash/durability gates |
| 2026-09-22 | live NFS connection objects | Rust `NfsServer::clients()` now exposes stable-id/peer connection objects with shared v3/v4 sessions and abort-safe `close`/`wait_closed`; N-API `NfsConnection` and the `./nfs` subpath export are generated and exercised against a live loopback client | Full v3/v4 stateful matrix, hosted lifecycle, Linux NFSv4.1, crash/durability, and any remaining upstream member differences |
| 2026-09-22 | transport reconnect and pipelined concurrency | Rootless NFSv4.1 proves a session can continue across an orderly TCP transport reconnect while the server process remains alive; a rootless NFSv3 connection completes eight pipelined MOUNT NULL calls with `max_in_flight=4`. The complete NFS target passes 31 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 3 | This is userspace/process-lifetime evidence only; automatic reconnect, crash/restart and durable lease/reply recovery, native Linux v4.1, hosted lifecycle, and the full stateful/member matrix remain open |
| 2026-09-22 | restart-boundary state classification | A rootless restart-boundary test reuses the backend across two server instances, proves `NfsServer::close()` destroys the first v4 session, and verifies the old session is rejected with `NFS4ERR_BADSESSION` by the replacement server. The v4 wire target now passes 4/4 | This proves process-local session/lease/replay state is not crash-durable; backend data durability, crash injection, native Linux v4.1, hosted lifecycle, and full upstream state/member parity remain open |
| 2026-09-22 | macOS native NFSv3 loopback | The refreshed opt-in `native_loopback_mount_round_trip` gate passed 1/1 in 0.11s on the exact pushed tip, including the temporary native mount, filesystem round trips, and bounded cleanup | Privileged Linux NFSv4.1, hosted lifecycle, full stateful matrix, remaining upstream member differences, and crash/durability gates |
| 2026-09-22 | pinned upstream and bounded-handle parity | The pinned oracle (`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`) passed 266 NFSv3/MOUNT and NFSv4.1 TCP conformance cases with 18 capability/root skips and zero mismatches; the N-API NFS codec differential passed; `maxHandles` now has shared LRU eviction, root/current protection, and NFSv4 open-state pins, covered by two focused table tests and the N-API server option path | The 18 conformance skips are explicit capability boundaries, not passes for unsupported rows; upstream `onError`, callback ID maps, N-API clock injection, native Linux v4.1, hosted lifecycle, and crash/durability remain open |
| 2026-09-22 | NFSv4 channel/state limit parity | Rust `Nfs4StateOptions` and nested N-API `Nfs4StateKnobs` now cover lease seconds, per-client sessions, fore slots, COMPOUND operations, request/replay-cache ceilings, per-file opens/locks, and reclaim policy; the v4 wire suite passes 5/5, including `maxLocksPerFile` enforcement for an existing lock state, `NFS4ERR_TOOSMALL`/`NFS4ERR_NOSPC` channel-cap statuses, and replay of a refused `CREATE_SESSION` followed by a next-sequence retry | Upstream dynamic ID-map callbacks, N-API clock injection, and session `onError` remain explicit parity gaps; native Linux/hosted lifecycle and crash/durability remain external gates |
| 2026-09-22 | NFSv4 reclaim ordering | With `requireReclaimComplete` enabled, `OPEN` and `LOCK` now return `NFS4ERR_GRACE` until the client completes `RECLAIM_COMPLETE`; the affected rootless v4.1 round-trip and same-owner/cross-client share tests pass | Full state/member parity, callback ID maps, N-API clock injection, session `onError`, native Linux/hosted lifecycle, and crash/durability remain external gates |
| 2026-09-22 | NFSv4 static ID-map parity | Rust `Nfs4IdMap` and nested N-API `Nfs4IdMap` now qualify mapped user/group names, preserve numeric fallback, reject other domains with `NFS4ERR_BADOWNER`, and keep user/group namespaces separate; the full locked NFS target passes 35 unit tests, rootless wire 1, pipelined concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 5; release N-API build, generated typecheck, live server integration, and strict affected Clippy pass | Upstream callback-based maps, N-API clock injection, session `onError`, native Linux v4.1, hosted lifecycle, and crash/durability remain external gates |
| 2026-09-22 | NFSv4 seeded identity parity | `nfs4.seed` now folds a configured uint32 into the client-id high half and session-id prefix; the rootless v4 wire test asserts both identities and N-API parsing/type coverage passes | N-API clock injection, callback-based maps, session `onError`, native Linux v4.1, hosted lifecycle, and crash/durability remain open |
| 2026-09-22 | NFSv4 lease expiry enforcement | `Nfs4Clock` enables deterministic Rust monotonic-time tests; expired clients are swept automatically before COMPOUND dispatch or explicitly through `Nfs4Session::sweep_expired`, releasing sessions, locks, open states, and pinned backend handles; the v4 wire test covers both paths and passes 6/6 | N-API clock injection, callback-based maps, session `onError`, native Linux v4.1, hosted lifecycle, and crash/durability remain open |
| 2026-09-22 | NFS request error callback parity | Rust `NfsSessionHooks` and N-API `NfsServerOptions.onError` now report ordinary status failures and decoded XDR/dispatch failures; decoded calls, panic isolation, generated typing, and live N-API delivery are covered; complete NFS target passes 37 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 6, with release build/typecheck and live server integration green | Dynamic ID-map callbacks, N-API clock injection, native Linux v4.1, hosted lifecycle, and crash/durability remain external gates |
| 2026-09-22 | NFSv4 dynamic owner and clock callbacks | Rust `Nfs4IdMap` now supports panic-isolated synchronous name/id callbacks, and the N-API `nfs4.idmap.nameOf`/`idOf` plus `nfs4.now` callbacks are retained and released with the server; a live N-API v4.1 `EXCHANGE_ID`/`CREATE_SESSION`/`GETATTR`/`SETATTR` sequence observed owner and group callback arguments, translated names, reverse translations, and injected clock calls. The complete locked NFS target passes 38 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 6; release build, generated typecheck, live server integration, pinned NFS codec differential, and strict affected Clippy pass | Full upstream state/member matrix, native Linux v4.1, hosted lifecycle, and cross-process crash/cancellation/concurrency/durability remain external gates |

## Exact commands and gate boundaries

- `./scripts/cargo-shared test -p mount-rs-nfs --all-targets --locked` — PASS:
  38 unit tests, rootless wire 1, transport concurrency 1, transport errors 4,
  v4 commit barrier 1, and v4 wire 6; the native mount target remains 1
  explicitly ignored test. The reconnect and pipelining rows are rootless
  userspace evidence, not native or hosted-client acceptance.
- `CI=true CARGO_TARGET_DIR=/Users/andrewmcclenaghan/Library/Caches/mount-rs/cargo-target MOUNTX_SOURCE=/private/tmp/mountx-w01-nfs-oracle pnpm --dir tests/upstream exec vitest run nfs-conformance.test.mjs --config vitest.config.mjs` — PASS: 266 pinned-oracle NFSv3/MOUNT and NFSv4.1 TCP cases, 18 explicit capability/root skips, 0 mismatches.
- `(cd integrations/mount-rs-napi && pnpm build)` — PASS: release addon and
  generated declarations rebuilt.
- `(cd integrations/mount-rs-napi && node test/typecheck.mjs)` — PASS.
- `(cd integrations/mount-rs-napi && node test/servers.mjs)` — PASS: live NFS
  v3/v4 routing, callback-backed v4.1 `EXCHANGE_ID`/`CREATE_SESSION`, owner
  `GETATTR`/`SETATTR`, injected clock calls, shared handle snapshots, active
  connection count, live client identity/peer/session views, connection
  close/wait, malformed record reporting, request-level NFS `onError` delivery,
  and destroyed-state cleanup.
- `(cd integrations/mount-rs-napi && node test/nfs-codec.mjs)` — PASS for the
  root/`./nfs` export identity checks and the complete pinned byte differential
  against `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`, including
  `NfsConnection`.
- `./scripts/cargo-shared clippy -p mount-rs-nfs -p mount-rs-napi --all-targets --locked -- -D warnings` — PASS.
- `MOUNT_RS_NFS_NATIVE_TEST=1 ./scripts/cargo-shared test -p mount-rs-nfs --test native_mount -- --ignored --exact native_loopback_mount_round_trip --nocapture` — PASS: macOS native NFSv3 loopback mount, filesystem round trips, unmount, and bounded cleanup; 1 passed, 0 failed, 0.11s on the exact pushed tip.
- Linux v4.1 native qualification remains an external gate and was not run on
  this macOS host; it requires the separate
  `MOUNT_RS_NFS_NATIVE_V4_TEST=1` lane plus a privileged Linux NFS client.

## Completion rule

The owning task may mark W01-NFS production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
