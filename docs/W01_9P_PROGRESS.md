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
| Attached-stream contract | Local PASS | Node `attach(stream, options)`, ownership, duplicate attach, direct session calls, non-socket duplex, backpressure, write failure, and server-close tests |
| Native-listener stream boundary | Explicit supported-scope decision | Native Tokio-accepted connections expose `stream: undefined`; callers requiring a Node `Duplex` use `server.attach` |
| Linux native 9P | Hosted lifecycle PASS; current packet rerun pending | CI run `35616832528`, job `106389895603`, at `e168315c246061926a36e795f7332831cb1ab62f` passed `modprobe 9p`, `modprobe 9pnet_fd`, and both privileged ignored native mount/read/write/unmount lifecycle tests; the shutdown/reaping packet below still needs a fresh revision-matched run |
| Errors, cancellation, concurrency, crash and cleanup | Local deterministic PASS; native fault/crash evidence open | Focused Rust/N-API lifecycle and fault tests now cover broadcast shutdown, accept-loop close races, bounded task reaping and transport faults; hosted/native reset, half-close, concurrency and crash evidence remains |

## Current queue

- Rerun and retain the hosted Linux `native-9p` job on the shutdown/reaping
  packet revision, including kernel `9p`/`9pnet_fd` module checks and the
  privileged ignored test.
- Qualify cross-process mount I/O, cancellation/close, restart, reset,
  half-close, concurrency, and crash behavior on the supported Linux runtime;
  the prior hosted lifecycle result does not close these additional gates.
- Keep unsupported platform/client results explicit and separate from passes.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Attached Node Duplex and session contract | `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked` passed 24 focused tests; `./scripts/cargo-shared check -p mount-rs-napi --locked` passed; `./scripts/cargo-shared clippy -p mount-rs-9p -p mount-rs-napi --all-targets --locked -- -D warnings` passed; `pnpm build:debug`, `node test/typecheck.mjs`, and host-enabled `node test/servers.mjs` passed; `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 pnpm test` passed, including the pinned 44-case 9P codec differential and artifact aggregation | PGlite/R2/native-mount opt-ins are explicit skips; current-revision hosted Linux native 9P, native mount fault/race/crash, and broader W01 acceptance remain open |
| 2026-09-22 | Hosted Linux native lifecycle and shutdown/reaping packet | Prior revision-matched run `35616832528` / `native-9p` job `106389895603` passed kernel-module probing plus privileged native mount/read/write/unmount; the new packet adds broadcast shutdown, active-connection close-race coverage, task-failure reporting and completed-task reaping, with `transport_lifecycle` 5/5, `transport_errors` 8/8, strict 9P Clippy and formatting passing locally | A fresh hosted run for the new packet is required; native reset/half-close/concurrency/crash evidence and broader W01 acceptance remain open |

## Completion rule

The owning task may mark W01-9P production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
