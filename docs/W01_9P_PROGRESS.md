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
| Linux native 9P | External gate | Fresh revision-matched hosted Linux 9P kernel-client mount/read/write/unmount and transport-fault result |
| Errors, cancellation, concurrency, crash and cleanup | Local deterministic PASS; platform evidence open | Focused Rust/N-API lifecycle and fault tests plus hosted/native race, reset, half-close, and crash evidence |

## Current queue

- Run and retain the hosted Linux `native-9p` job on this revision, including
  kernel `9p`/`9pnet_fd` module checks and the privileged ignored test.
- Qualify cross-process mount I/O, cancellation/close, restart, reset,
  half-close, concurrency, and crash behavior on the supported Linux runtime.
- Keep unsupported platform/client results explicit and separate from passes.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Attached Node Duplex and session contract | `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked` passed 24 focused tests; `./scripts/cargo-shared check -p mount-rs-napi --locked` passed; `./scripts/cargo-shared clippy -p mount-rs-9p -p mount-rs-napi --all-targets --locked -- -D warnings` passed; `pnpm build:debug`, `node test/typecheck.mjs`, and host-enabled `node test/servers.mjs` passed; `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 pnpm test` passed, including the pinned 44-case 9P codec differential and artifact aggregation | PGlite/R2/native-mount opt-ins are explicit skips; current-revision hosted Linux native 9P, native mount fault/race/crash, and broader W01 acceptance remain open |

## Completion rule

The owning task may mark W01-9P production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
