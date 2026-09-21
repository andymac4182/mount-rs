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
| Session and connection objects | Local PASS for exposed scope | `P9Session.handleCall`/`destroy`, stats/lifecycle, clients, peer, `closed`, attached stream exposure, and identity tests |
| Attached-stream contract | Local PASS | Node `attach(stream, options)` with typed peer/ownership/frame/in-flight bounds, ownership, duplicate attach, direct session calls, non-socket duplex, backpressure, write failure, and server-close tests |
| Native-listener stream boundary | Explicit supported-scope decision | Native Tokio-accepted connections expose `stream: undefined`; callers requiring a Node `Duplex` use `server.attach` |
| Linux native 9P | Hosted prior lifecycle PASS; current packet exposed a server-close cleanup failure; fix rerun pending | CI run `35616832528`, job `106389895603`, at `e168315c246061926a36e795f7332831cb1ab62f` passed the prior privileged lifecycle; current run `35625437327`, native-9p job `106418844564`, passed module probing and 3/4 ignored native tests, while `native_linux_server_close_releases_kernel_connection` found a live mountpoint after `P9Mount::unmount()` returned success; corrective run `35626340158` at `c2290b269882c768ba64965e65cbe6626a971610` was canceled before jobs materialized |
| Errors, cancellation, concurrency, crash and cleanup | Local deterministic PASS; native fault/crash evidence open | Focused Rust/N-API lifecycle and fault tests now cover broadcast shutdown, accept-loop close races, shutdown-aware in-flight permit waits, bounded task reaping, transport faults, and session destruction that wakes and drains `Tflush` waiters; ignored native harnesses cover eight concurrent mounted file write/read/rename/read workers plus server-close, kernel-connection-close, and bounded-unmount cleanup, while hosted/native reset, half-close, and process-crash evidence remains |

## Current queue

- Rerun and retain the hosted Linux `native-9p` job on the current packet,
  including kernel `9p`/`9pnet_fd` module checks, the privileged lifecycle
  test, the bounded concurrent file-I/O/unmount test, and the server-close
  kernel-connection/unmount test.
- Qualify cross-process mount I/O, cancellation/close, restart, reset,
  half-close, concurrency, and crash behavior on the supported Linux runtime;
  the local `Tflush` teardown regression and the new server-close harness do
  not close these remaining native gates, and the prior hosted lifecycle result
  does not close them either.
- Keep unsupported platform/client results explicit and separate from passes.

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

## Completion rule

The owning task may mark W01-9P production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
