# W01-NFS progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md)

Owner: delegated W01-NFS task (thread id recorded in the parent tracker after
dispatch)

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns NFSv3/MOUNT, NFSv4.1, the shared session/router boundary,
connection lifecycle, native mounts, and NFS-specific durability and handle
semantics.

| Gate | State | Required evidence |
| --- | --- | --- |
| NFSv3/MOUNT public API and wire behavior | In progress | Codec, rootless wire, session, error, and generated N-API evidence |
| NFSv4.1 router/session behavior | In progress | Version routing, COMPOUND/state matrix, shared handle/counter proof, and direct N-API view |
| Connection and server lifecycle | In progress | Live connections, malformed/EOF/reset handling, close, async disposal, and restart evidence |
| macOS/Linux native NFS | External gate | Actual supported-client mount/read/write/unmount and cleanup results |
| Handles, concurrency, crash and durability | Open | Cross-version handle lifetime, cancellation/close, crash/restart, ordering, and persistence evidence |

## Current queue

- Complete direct N-API shared-handle/session inspection and the remaining
  upstream NFS connection-object surface.
- Exercise the full v3/v4 behavior matrix, including stateful v4 operations,
  malformed records, reconnects, and version negotiation.
- Refresh supported macOS/Linux native mount evidence and keep ignored native
  tests classified as external gates.
- Qualify cross-process crash, cancellation/close, concurrency, and durability
  behavior.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | v3/v4 shared-state foundation | N-API v3/v4 direct routing and `Nfs4Session` view pass; the Rust server shares the v3/v4 handle table, path lock, and counters; NFS crate and host-enabled package regressions pass | Direct shared-handle inspection, complete connection-object parity, native/hosted lifecycle, and crash/durability gates |

## Completion rule

The owning task may mark W01-NFS production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
