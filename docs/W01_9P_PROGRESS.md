# W01-9P progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md)

Owner: delegated W01-9P task

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns 9P protocol, session, connection, attach, mount, and
transport-lifecycle parity. Codec and raw-frame results do not prove the
upstream stream/attach contract or hosted native mount behavior.

| Gate | State | Required evidence |
| --- | --- | --- |
| Public 9P exports and protocol behavior | Local PASS for the implemented codec, constants, qid, cursor, fid-table, and session-backed fid surface; parity remains partial | Pinned 9P differential, generated declarations, malformed/trailing coverage, deterministic fid/qid/cursor lifecycle tests, and public-session behavior |
| Session and connection objects | Local PASS for current exposed members and the bounded N-API mount-helper facade; parity remains partial | `P9Session.handleCall`/`destroy`, scalar `options`, live `driver`, `userFor`, debug-gated assertions, request-error/assertion callbacks, live `locks` and `fids`, stats/lifecycle, live property-shaped `clients`, peer, `closed`, attached stream exposure, identity/handle tests, injected shared lock-table option coverage, and direct `./9p` probe/refusal/option/mount-helper checks; richer mount options and hosted N-API native-mount evidence remain open |
| Attached-stream contract | Local PASS | Node `attach(stream, options)` with typed peer/ownership/frame/in-flight bounds, ownership, duplicate attach, direct session calls, non-socket duplex, backpressure, write failure, and server-close tests |
| Native-listener stream boundary | Explicit supported-scope decision | Native Tokio-accepted connections expose `stream: undefined`; callers requiring a Node `Duplex` use `server.attach` |
| Linux native 9P | Hosted PASS for the supported Linux lifecycle scope; crash/reset/half-close recovery is outside the library guarantee | Dedicated [Native 9P run `35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344), job `106427627397`, at `431affd660391a0b8ed99815e389ffe12ad229c2` passed `9p`/`9pnet_fd` probing and all four ignored native tests: concurrent file I/O/unmount, server-close/kernel-connection release, ordinary mount/unmount, and external umount. Earlier failure/cancellation records remain below as history |
| Errors, cancellation, concurrency, crash and cleanup | Local deterministic PASS; supported hosted lifecycle PASS; crash/reset/half-close are supervisor-owned | Focused Rust/N-API lifecycle and fault tests cover broadcast shutdown, accept-loop close races, shutdown-aware in-flight permit waits, bounded task reaping, transport faults, and session destruction that wakes and drains `Tflush` waiters; the hosted native harness now passes eight concurrent mounted file write/read/rename/read workers plus server-close, kernel-connection-close, external umount, and bounded-unmount cleanup. Automatic recovery after process crash or arbitrary kernel reset/half-close remains explicitly outside the library contract |

## Current queue

- Retain the dedicated hosted Linux `Native 9P` workflow as the stable
  regression gate for kernel `9p`/`9pnet_fd` prerequisites, mounted I/O,
  concurrent file operations, server-close/kernel-connection teardown,
  external umount, and bounded unmount. It supports manual dispatch when a
  future 9P implementation packet needs an exact revision rerun.
- If a deployment claims recovery after process crash, arbitrary kernel reset,
  or half-close, qualify that behavior in a supervisor-level test and cleanup
  policy. Those recovery properties are not automatic guarantees of this
  library and are not promoted from the hosted lifecycle pass.
- Keep unsupported platform/client results explicit and separate from passes.
- The direct `./9p` mount-helper facade currently supports Linux-client probing,
  Unix/TCP refusal and option-string helpers, strict named 9P delegation through
  `mount9p`, 9P-only live-mount filtering/cleanup, and mounted `trans`/server/
  connection/`closed` views. Its supported option bag is deliberately bounded
  to transport, host/port/path, msize, access/cache/uname/aname, read-only,
  driver inode, mount options, unmount timeout, and transport-error callback.
  Shared-server injection, signals, server-policy fields, session callbacks,
  lock-table injection, and the remaining oracle mount controls are not claimed
  by this packet. A hosted N-API native mount run is still required before these
  views are treated as native runtime-qualified.

## Supported-scope decisions

- Native kernel mounts are supported and qualified only on Linux with the
  host `9p`/`9pnet_fd` prerequisites and mount privilege. macOS support for
  this crate is the rootless wire/TCP server; no native macOS 9P client is
  claimed.
- Native Tokio listener connections deliberately expose no transferable Node
  `Duplex`; callers needing a Node stream use `P9Server.attach`. The N-API
  low-level session now exposes scalar `options`, its live `driver`,
  `userFor(fid)`, debug-gated assertion retention/statistics, request-error and
  assertion callbacks, and a live `locks` client in addition to direct
  call/destroy, stats, and lifecycle. The standalone `P9LockTable`/`P9LockClient`
  surface is transport-backed and tested for conflict, ownership, rename, and
  release. `P9Server.clients` is now a live property-shaped array combining
  native and attached connections. `P9ServerOptions.locks` now accepts a
  `P9LockTable` and shares it across native and attached sessions, with live
  server/session option handles. The bounded `./9p` mount-helper facade is
  locally evidenced, while richer oracle mount options and hosted N-API native
  mount lifecycle remain applicable parity work. The `./9p` barrel now exposes
  the authoritative Rust-backed `FidTable` alias, live `P9Session.fids`, qid
  synthesis helpers, cursor/resume state, detached clunk views, and retained
  open-handle enumeration; the focused runtime test covers hardlink identity,
  large inode values, path remapping/release, mutable fid views, and a live
  opened session fid. Direct table mutation is a low-level inspection/testing
  seam: orderly production teardown remains protocol `Tclunk` or
  `P9Session.destroy`, not an arbitrary `clear()` on a live session. The
  constants/message-name part of the `./9p` barrel is complete and
  differentially checked across all 124 upstream exports. These remaining gaps
  are not silently accepted out of scope.
- Graceful server close, external unmount, and retryable unmount are in scope;
  the dedicated hosted run above verifies those Linux lifecycle paths.
  Automatic recovery after process crash or arbitrary kernel reset/half-close
  is not a library guarantee; a deployment claiming those properties needs a
  supervisor-level test and cleanup policy.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Attached Node Duplex and session contract | `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked` passed 24 focused tests; `./scripts/cargo-shared check -p mount-rs-napi --locked` passed; `./scripts/cargo-shared clippy -p mount-rs-9p -p mount-rs-napi --all-targets --locked -- -D warnings` passed; `pnpm build:debug`, `node test/typecheck.mjs`, and host-enabled `node test/servers.mjs` passed; `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 pnpm test` passed, including the pinned 44-case 9P codec differential and artifact aggregation | PGlite/R2/native-mount opt-ins are explicit skips; current-revision hosted Linux native 9P, native mount fault/race/crash, and broader W01 acceptance remain open |
| 2026-09-22 | Hosted Linux native lifecycle and shutdown/reaping packet | Prior revision-matched run `35616832528` / `native-9p` job `106389895603` passed kernel-module probing plus privileged native mount/read/write/unmount; the new packet adds broadcast shutdown, active-connection close-race coverage, task-failure reporting and completed-task reaping, with `transport_lifecycle` 5/5, `transport_errors` 8/8, strict 9P Clippy and formatting passing locally | A fresh hosted run for the new packet is required; native reset/half-close/concurrency/crash evidence and broader W01 acceptance remain open |
| 2026-09-22 | Native concurrent mounted I/O harness | Added an ignored Linux-native test that launches eight bounded blocking workers for independent mounted write/read/rename/read round trips, then performs bounded unmount and refuses recursive cleanup after a failed lifecycle; the focused 9P target and strict 9P Clippy pass locally | Hosted execution on a revision containing this harness is required; native reset/half-close/crash evidence and broader W01 acceptance remain open |
| 2026-09-22 | Session teardown and `Tflush` cancellation | Session destruction now marks and wakes every in-flight request, drains the in-flight map, and releases a waiting `Tflush` with the existing destroyed-session error boundary; the regression test passes in the focused 9P target and strict Clippy | Native cancellation/close, reset, crash, and current hosted execution remain open |
| 2026-09-22 | Native server-close lifecycle harness | Added an ignored Linux-native test that performs mounted file I/O, closes the server side, waits for the kernel connection to close, and performs bounded unmount; failed paths refuse recursive mountpoint cleanup, and the focused target plus strict Clippy pass locally | Hosted execution is required; process-crash, reset/half-close, and broader W01 acceptance remain open |
| 2026-09-22 | Attached-stream option declaration parity | The generated `attach()` declaration now exposes the already-supported per-attachment `maxFrame` and `maxInFlight` bounds through `P9AttachOptions`; the TypeScript test compiles the peer/ownership/bounds shape, the attached-stream integration covers the oracle's default `peer: undefined` and per-attachment in-flight/frame limits, and native listener connections retain their N-API `null` representation | Runtime/native lifecycle and broader upstream stream/attach parity remain open |
| 2026-09-22 | Shutdown-aware in-flight permit wait | The Rust connection loop now selects server/connection shutdown while waiting for a `maxInFlight` permit; a blocked-driver regression proves bounded server close instead of waiting behind a slow request, with `transport_lifecycle` 6/6 and the formatter passing locally | Fresh hosted native execution, native reset/half-close/process-crash evidence, and broader W01 acceptance remain open |
| 2026-09-22 | Current hosted rerun attempt | CI run `35624213028` at published commit `201b7724dced89122f2e310ff5145acb123a8ab7` was canceled before jobs materialized; it provides no current-revision native-9P result | Hosted native execution remains required; do not promote the prior lifecycle checkpoint to this packet |
| 2026-09-22 | Current hosted rerun attempt after attached-stream limits packet | CI run `35624869535` at published commit `e5f4dda0f2e79899f3631e6fbf7caa260497ad21` was canceled before jobs materialized (`jobs: []`); it provides no current-revision native-9P result | Hosted native execution remains required; do not promote the prior lifecycle checkpoint to this packet |
| 2026-09-22 | Current hosted native execution and unmount-race fix | Run `35625437327`, native-9p job `106418844564`, loaded `9p`/`9pnet_fd` and passed 3/4 ignored tests; `native_linux_server_close_releases_kernel_connection` failed because the connection-close monitor marked resource teardown complete before `P9Mount::unmount()` issued `umount(8)`. The local fix gives kernel unmount its own coordination state so server-close/EOF cannot suppress unmount; focused 9P tests 28/28, strict Clippy and formatting pass locally | Fresh hosted rerun must verify the fix, plus native reset/half-close/process-crash evidence and broader W01 acceptance remain open |
| 2026-09-22 | Corrective hosted rerun attempt | CI run `35626340158` at `c2290b269882c768ba64965e65cbe6626a971610` was canceled before jobs materialized (`jobs: []`); the follow-up also hardens unmount serialization against outer-task cancellation with a Tokio mutex, and the focused 9P tests 28/28, strict Clippy and formatting pass locally | A hosted run containing the unmount fix is still required, plus native reset/half-close/process-crash evidence and broader W01 acceptance |
| 2026-09-22 | Cancellation-safe unmount and supported-scope boundary | `P9Mount::unmount()` now has cancellation-safe mutex serialization and remains callable after `wait_closed()` resource teardown; the README and tracker define Linux-native, Node-attach, graceful-close, and supervisor-owned crash/reset boundaries. Focused 9P tests 28/28, strict Clippy and formatting pass locally | Hosted verification of the unmount fix remains required; unsupported crash/reset/half-close recovery is not a production claim |
| 2026-09-22 | Stable hosted Linux 9P qualification | Dedicated [Native 9P run `35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344), job `106427627397`, at exact SHA `431affd660391a0b8ed99815e389ffe12ad229c2`, passed kernel-module probing and the four ignored native tests; the log reports `4 passed; 0 failed`, including `native_linux_server_close_releases_kernel_connection` | Public parity remained partial at that revision, and the overall W01/release decision remains NO-GO; crash/reset/half-close recovery is supervisor-owned rather than a library claim |
| 2026-09-22 | N-API P9 session metadata parity | `pnpm --dir integrations/mount-rs-napi build:debug`, `node test/typecheck.mjs`, `node test/p9-session-metadata.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 typed cases), `./scripts/cargo-shared fmt --all -- --check`, `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked` (28 ordinary tests), and strict `./scripts/cargo-shared clippy -p mount-rs-napi -p mount-rs-9p --all-targets --locked -- -D warnings` passed. `node test/servers.mjs` could not reach its P9 phase because this host's unrelated NFS listener cleanup/relisten returned `Operation not permitted` | `P9Server.options`, `P9Session.options`, and `P9Session.userFor` are now evidenced; upstream driver/fid/lock/assertion/debug/mount/barrel surfaces and the property-shaped `clients` contract remain open; production NO-GO |
| 2026-09-22 | N-API P9 lock surface parity | `pnpm --dir integrations/mount-rs-napi build:debug`, `node test/typecheck.mjs`, `node test/p9-locks.mjs`, `node test/p9-session-metadata.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 typed cases), `./scripts/cargo-shared fmt --all -- --check`, `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked`, and isolated strict `CARGO_TARGET_DIR=/private/tmp/mount-rs-clippy-w01-20260922 ./scripts/cargo-shared clippy -p mount-rs-napi -p mount-rs-9p --all-targets --locked -- -D warnings` passed. The lock test proved grant/conflict/rename/release/inspection, and the session test proved its lock client shares authoritative transport state | `P9LockTable`/`P9LockClient` are now transport-backed; upstream driver/fid/assertion/debug/full fid graph, lock-table option injection, property-shaped `clients`, and 9P mount/barrel surfaces remain open; production NO-GO |
| 2026-09-22 | N-API P9 constants/barrel parity | `node test/typecheck.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/p9-constants.mjs`, `node --check p9.cjs`, and `git diff --check` passed. The test differentially checks all 124 upstream `./9p` constants and `messageName` results, including message numbers, 64-bit masks, qid bits, wire limits, versions, and Linux open flags; direct CommonJS assignments preserve ESM named-export discovery | Constants/message-name parity is closed; upstream driver/fid/assertion/debug/full fid graph, lock-table option injection, property-shaped `clients`, and 9P mount helpers remain open; production NO-GO |
| 2026-09-22 | N-API P9 fid-table/session parity | `pnpm --dir integrations/mount-rs-napi build:debug`, `node test/typecheck.mjs`, `node test/p9-fids.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-locks.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/p9-constants.mjs` (124 exports), `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 cases), `node --check integrations/mount-rs-napi/p9.cjs`, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (30 passed, 0 failed), and isolated warning-denied Clippy passed. Coverage includes live session fids, mutable path/open/iounit/cursor state, qid/cursor helpers, hardlink/release identity, large inode values, deterministic creation order, clunk snapshots, and real retained open-handle enumeration when the driver supplies one | Remaining upstream driver/assertion/debug, lock-table option injection, property-shaped `clients`, and 9P mount-helper parity remain open; hosted native lifecycle evidence is revision-scoped to the published SHA, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 driver and observability parity | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-observability.mjs`, `node test/p9-fids.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-locks.mjs`, pinned `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/p9-constants.mjs` (124 exports), pinned `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 typed cases), root/`./9p` factory identity, `node --check` for the server and 9P facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (31 passed, 0 failed), and isolated warning-denied Clippy passed. Coverage includes the live `P9Session.driver`, debug-gated assertion readback/counters, duplicate-tag assertion/error reporting, malformed-frame `EPROTO` reporting without a header, negotiated unknown-message `ENOTSUP` reporting with its typed header, and Node error revival | Lock-table option injection, property-shaped `clients`, and 9P mount-helper parity remain open; hosted native evidence remains revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 property-shaped clients parity | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-observability.mjs`, `node test/p9-fids.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-locks.mjs`, host-enabled `node test/servers.mjs`, `node --check` for the server and 9P facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (31 passed, 0 failed), and isolated warning-denied Clippy passed. `P9Server.clients` is now a live property-shaped array combining native and attached connections, with generated declaration and runtime identity/teardown evidence; the host-enabled server integration passed. The earlier sandbox-only NFS relisten `Operation not permitted` was not promoted to a product failure | Lock-table option injection and 9P mount-helper parity remain open; hosted native evidence remains revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 lock-table option injection parity | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-observability.mjs`, `node test/p9-fids.mjs`, `node test/p9-locks.mjs`, host-enabled `node test/servers.mjs`, `node --check` for the server and 9P facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (31 passed, 0 failed), and isolated warning-denied Clippy passed. `P9ServerOptions.locks` now accepts a `P9LockTable`; injected ranges are visible through the server and connection-session option handles and are shared with protocol lock state, with generated declaration and type/runtime evidence | 9P mount-helper parity remains open; hosted native evidence remains revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 bounded mount-helper facade | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-mount-helpers.mjs`, the focused P9 session/observability/fid/lock regressions, host-enabled `node test/servers.mjs`, `node --check` for the P9/server facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, `mount-rs-napi --lib` (17 passed, 0 failed), focused `mount-rs-9p` tests (35 passed, 0 failed), and warning-denied Clippy for `mount-rs-napi`, `mount-rs-auto`, and `mount-rs-9p` passed. The direct `./9p` facade now exposes probe/refusal/option helpers, strict named `mount9p` delegation, 9P live-mount filtering/cleanup, and mounted transport/server/connection/closed views; the nested auto option bag carries the same bounded native 9P fields | Oracle shared-server/signal/extended server-policy mount options and hosted N-API native-mount lifecycle evidence remain open; prior hosted Linux evidence is revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |

## Completion rule

The owning task may mark W01-9P production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
