# W01-WebDAV progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md) and the current
repository ledger in [`WORK_TRACKER.md`](../WORK_TRACKER.md).

Owner: delegated W01-WebDAV task

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns WebDAV protocol/XML/locks/authentication, session and
streaming APIs, connection faults, lifecycle, and provider/native boundaries.

| Gate | State | Required evidence |
| --- | --- | --- |
| Public WebDAV barrel and pure protocol behavior | Local PASS; oracle differential SKIP | `./webdav` barrel, generated declarations, pinned constants/path/header/XML/lock/document differential; `MOUNTX_SOURCE` is required for the oracle row |
| Session, lock, auth and streaming objects | Buffered/session slice PASS; streaming OPEN | N-API session view, driver/options/lock policy, class 1/2 methods, auth challenge/acceptance, streamed bodies, cancellation, and typed errors |
| Connection and server lifecycle | Rust PASS; N-API listener prerequisite BLOCKED | Rust HTTP lifecycle and connection-fault tests; live N-API listen/close/async-dispose and restart evidence on a host that permits loopback binds |
| Live/provider/native qualification | External gate | Fresh supported backend and hosted/native results where applicable |
| Faults, concurrency and durability | Open | Peer-fault callback, lock conflicts/expiry, cancellation/close, crash/restart, and durability matrices |

## Current queue

- Bind the remaining applicable WebDAV session/server members, including active
  lock records and streamed request/response bodies.
- Exercise peer reset, malformed connection, lock conflict/expiry, cancellation,
  close, and restart behavior through the N-API boundary.
- Run the pinned oracle differential when `MOUNTX_SOURCE` is supplied.
- Qualify supported provider/native/hosted paths; keep unavailable rows
  explicitly classified.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | WebDAV pure barrel and buffered N-API session | `./scripts/cargo-shared test -p mount-rs-webdav --locked`: 13 passed, native mount probe explicitly ignored; isolated locked N-API check passed; release N-API build and generated declarations passed; direct buffered session/options/driver/auth check passed | Oracle differential needs `MOUNTX_SOURCE`; loopback N-API listener is blocked in this sandbox by `Operation not permitted`; streaming, peer-fault, restart/durability, provider, and native/hosted gates remain open |
| 2026-09-22 | Package integration boundaries | `node test/webdav-codec.mjs`: explicit SKIP because `MOUNTX_SOURCE` is unset; package-wide `node test/servers.mjs` reached the unrelated NFS lane first and hit its host-permission prerequisite | Do not promote the package-wide NFS failure or the skipped oracle row into WebDAV PASS evidence |

## Completion rule

The owning task may mark W01-WebDAV production-ready only when every
applicable gate above is PASS or explicitly accepted outside supported scope,
exact commands and prerequisites are recorded here, and this file is committed
with each implementation/test chunk. Until then the decision remains
**NO-GO**.
