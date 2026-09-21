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
| Public 9P exports and protocol behavior | Local PASS; parity remains partial | Pinned 9P differential, generated declarations, malformed/trailing coverage, and public-session behavior |
| Session and connection objects | Local PASS for current exposed members; parity remains partial | `P9Session.handleCall`/`destroy`, scalar `options`, `userFor`, live `locks`, stats/lifecycle, clients, peer, `closed`, attached stream exposure, and identity tests; upstream driver/fid/assertion/debug surfaces, full fid graph, lock-table option injection, and property-shaped clients remain open |
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

## Supported-scope decisions

- Native kernel mounts are supported and qualified only on Linux with the
  host `9p`/`9pnet_fd` prerequisites and mount privilege. macOS support for
  this crate is the rootless wire/TCP server; no native macOS 9P client is
  claimed.
- Native Tokio listener connections deliberately expose no transferable Node
  `Duplex`; callers needing a Node stream use `P9Server.attach`. The N-API
  low-level session now exposes scalar `options`, `userFor(fid)`, and a live
  `locks` client in addition to direct call/destroy, stats, and lifecycle. The
  standalone `P9LockTable`/`P9LockClient` surface is transport-backed and
  tested for conflict, ownership, rename, and release. Upstream `driver`,
  `fids`, assertion/debug, full fid graphs, lock-table option injection,
  property-shaped `clients`, and 9P mount/barrel helpers remain applicable
  parity work; they are not silently accepted out of scope.
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

## Completion rule

The owning task may mark W01-9P production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
