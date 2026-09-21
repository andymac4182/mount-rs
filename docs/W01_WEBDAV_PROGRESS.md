# W01-WebDAV progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md)

Owner: delegated W01-WebDAV task (thread id recorded in the parent tracker
after dispatch)

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns WebDAV protocol/XML/locks/authentication, session and
streaming APIs, connection faults, lifecycle, and provider/native boundaries.

| Gate | State | Required evidence |
| --- | --- | --- |
| Public WebDAV barrel and pure protocol behavior | In progress | Pinned constants/path/header/XML/lock/document differential and generated declarations |
| Session, lock, auth and streaming objects | In progress | Class 1/2/3 methods, lock lifecycle, auth challenge/acceptance, streamed bodies, cancellation, and errors |
| Connection and server lifecycle | In progress | Live connections, malformed/reset handling, close, async disposal, and restart evidence |
| Live/provider/native qualification | External gate | Fresh supported backend and hosted/native results where applicable |
| Faults, concurrency and durability | Open | Connection faults, lock conflicts/expiry, cancellation/close, crash/restart, and durability evidence |

## Current queue

- Close remaining applicable WebDAV session/server/member parity.
- Exercise lock/auth behavior across restart, conflicts, expiry, and failure.
- Qualify supported native/hosted/provider paths and keep unavailable rows
  explicitly classified.
- Run concurrency, cancellation/close, and crash/durability matrices.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Inherited W01 baseline | Pure protocol packet, lock/auth/session options, buffered/streamed methods, peer faults, connection cleanup, and package regression pass | Complete member parity, supported hosted/provider/native lifecycle, and broader fault/durability gates |

## Completion rule

The owning task may mark W01-WebDAV production-ready only when every
applicable gate above is PASS or explicitly accepted outside supported scope,
exact commands and prerequisites are recorded here, and this file is committed
with the implementation/test chunk. Until then the decision remains **NO-GO**.
