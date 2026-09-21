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
| Public WebDAV barrel and pure protocol behavior | Local PASS; pinned pure oracle differential PASS; broader session/server parity OPEN | `./webdav` barrel, generated declarations, pinned constants/path/header/XML/lock/document differential; the pinned pure differential passed with `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921` at oracle `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` |
| Session, lock, auth and streaming objects | Buffered/session, active-lock-owner, direct-method, and direct-streaming slices PASS; member parity OPEN | N-API session view, driver/options/lock policy, scalar and recursive owner lock records, class 1/2/3 methods, auth challenge/acceptance, streamed bodies, cancellation, and typed errors; the supported N-API scope currently does not claim the oracle's injectable `now`, `onError`, `onAssertion`, live `DavLockTable`, or `Map`-shaped method counters |
| Connection and server lifecycle | Rust and local host-enabled N-API listener/fault slices PASS; restart/native/hosted lifecycle OPEN | Rust HTTP lifecycle and connection-fault tests; local N-API listen/close and peer-reset evidence; restart and native/hosted lifecycle evidence remain required |
| Live/provider/native qualification | External gate | Fresh supported backend and hosted/native results where applicable |
| Faults, concurrency and durability | Local peer-fault/lock-conflict/expiry/cancellation/close and same-session independent-request concurrency slices PASS; broader restart/durability/concurrency OPEN | Peer-fault callback, lock conflicts/expiry, cancellation/close, independent direct-session requests, crash/restart, and durability matrices |

## Current queue

- Bind the remaining applicable WebDAV session/server members, including
  complete direct method/member parity; first resolve or explicitly accept the
  current N-API boundary for `now`, `onError`, `onAssertion`, the live
  `DavLockTable`, and `stats.methods` representation.
- Extend concurrency evidence beyond the eight independent direct-session
  PUT/GET requests, including network-client, native, hosted, and
  crash/power-loss boundaries where applicable.
- Exercise cancellation, close, and crash/power-loss restart behavior through
  the N-API boundary.
- Keep the pinned pure oracle differential green and add the broader
  session/server differential when that harness is available.
- Qualify supported provider/native/hosted paths; keep unavailable rows
  explicitly classified.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | WebDAV pure barrel and buffered N-API session | `./scripts/cargo-shared test -p mount-rs-webdav --locked`: 13 passed, native mount probe explicitly ignored; isolated locked N-API check passed; release N-API build and generated declarations passed; direct buffered session/options/driver/auth check passed | Broader session/server parity, loopback N-API listener in this sandbox (`Operation not permitted` without host execution), streaming, peer-fault, restart/durability, provider, and native/hosted gates remain open |
| 2026-09-22 | WebDAV pinned pure oracle differential | `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/webdav-codec.mjs` passed the complete pure constants/path/header/XML/lock/document differential at oracle `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`; the same source-backed host-enabled `node test/servers.mjs` also passed | Full session/server member differential, native/hosted lifecycle, provider qualification, crash/power-loss durability, and broader concurrency remain open |
| 2026-09-22 | WebDAV N-API streamed request/response bodies | Isolated N-API check and release addon build passed; generated declarations expose `handleRequestStream`; direct N-API probe passed three-chunk PUT, multi-chunk GET, early response-iterator return, and deliberate request-body failure mapping; the WebDAV stream facade accepts async iterables and Web ReadableStreams | Complete session/member parity, direct listener lifecycle on a host that permits loopback binds, peer-fault injection, oracle differential, restart/durability, provider, and native/hosted gates remain open |
| 2026-09-22 | WebDAV N-API active lock records | Added generated `WebdavLockView` records and `WebdavSession.locks`; `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-lock-test-target ./scripts/cargo-shared test -p mount-rs-webdav --locked` passed 13/13 with the native mount probe explicitly ignored, `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-lock-check-target ./scripts/cargo-shared check -p mount-rs-napi --locked`, `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-lock-napi-target pnpm build`, `node test/servers.mjs`, and `node test/typecheck.mjs` passed; direct LOCK exposed token/path/depth/exclusive/timeout state and UNLOCK reduced the session view to zero | Complete session/member parity, direct listener lifecycle on a host that permits loopback binds, peer-fault injection, oracle differential, restart/durability, provider, and native/hosted gates remain open |
| 2026-09-22 | WebDAV N-API lock-owner and XML entity parity | The rebuilt N-API view now preserves recursive `WebdavXmlNode` owner trees; host-enabled local and pinned structural-driver `node test/servers.mjs` phases accepted a namespaced owner containing `A&amp;B` and returned the expected owner tree. `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-xml-entities-test-target ./scripts/cargo-shared test -p mount-rs-webdav --locked` passed 13/13 with predefined/numeric entity tests; `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-lock-owner-clippy-target ./scripts/cargo-shared clippy -p mount-rs-webdav -p mount-rs-napi --lib --locked -- -D warnings`, `node test/typecheck.mjs`, and the release addon build passed | Complete session/member parity beyond the owner view, listener/native/hosted lifecycle, provider qualification, crash/power-loss durability, and broader concurrency remain open |
| 2026-09-22 | WebDAV direct N-API method matrix | Host-enabled `node test/servers.mjs` passed direct `OPTIONS`/`MKCOL`/`PUT`/`HEAD`/`GET`, `PROPFIND`/`PROPPATCH`, `COPY`/`MOVE`, `LOCK`/`UNLOCK`, `DELETE`, and explicit `PATCH` refusal at the session boundary | Complete session/member parity, direct listener lifecycle on a host that permits loopback binds, peer-fault injection, oracle differential, restart/durability, provider, and native/hosted gates remain open |
| 2026-09-22 | WebDAV N-API peer-fault callback | Host-enabled `node test/servers.mjs` drove a large-response Node socket reset after reply readiness; exactly one typed transport callback carried the accepted `127.0.0.1:<port>` peer and server cleanup completed | Malformed-connection, lock conflict/expiry, restart/durability, complete session/member parity, oracle differential, provider, and native/hosted gates remain open |
| 2026-09-22 | WebDAV N-API malformed connection | Host-enabled `node test/servers.mjs` isolated a malformed HTTP request on a second WebDAV server and received one typed transport callback with the accepted `127.0.0.1:<port>` peer; the malformed socket and server then closed cleanly | Lock conflict/expiry, restart/durability, complete session/member parity, oracle differential, provider, and native/hosted gates remain open |
| 2026-09-22 | WebDAV same-driver restart classification | Host-enabled `node test/servers.mjs` wrote a file and an active lock, closed the seed server, recreated the server over the same driver, read the file successfully, and observed zero replacement-session locks; this is local same-process recreation evidence only | Crash/power-loss restart, durable lock policy, provider/native/hosted lifecycle, complete session/member parity, oracle differential, concurrency, and durability remain open |
| 2026-09-22 | WebDAV direct-session concurrency classification | Host-enabled `node test/servers.mjs` created `/concurrent` and completed eight parallel unique-file `PUT` requests followed by eight parallel `GET` requests through one `WebdavSession.handleRequest`; every response succeeded and every body matched | This is in-process same-driver evidence only; network-client, native, hosted, crash/power-loss restart, durable-provider behavior, complete session/member parity, and oracle differential remain open |
| 2026-09-22 | WebDAV N-API lock conflict and expiry | Host-enabled `node test/servers.mjs` created a direct exclusive lock, observed an unsubmitted-token `PUT` return `423`, then verified a `Second-1` lock expired and `WebdavSession.locks` returned to zero | Malformed-connection, restart/durability, complete session/member parity, oracle differential, provider, and native/hosted gates remain open |
| 2026-09-22 | WebDAV N-API supported-scope classification | The local N-API surface exposes `driver`, scalar `options`, `stats`, `assertions`, `lockCount`, and a snapshot `locks: WebdavLockView[]`; the pinned oracle's `WebdavSession` also exposes injectable `options.now`, `onError`, `onAssertion`, a live `DavLockTable`, and `stats.methods: Map<string, number>`. The N-API declarations and source-backed tests do not claim those missing callback/clock/table/Map semantics | These are explicit public-parity OPEN items, not accepted as outside the supported production scope; native/hosted lifecycle, provider, crash/power-loss durability, and broader concurrency remain OPEN |
| 2026-09-22 | Published-tip hosted workflow status | A read-only status check for published commit `9e8e4592cd8d4fe5b42c2734621ac1cd1bce02b5` found [CI run 35631845088](https://github.com/andymac4182/mount-rs/actions/runs/35631845088) and [fault-injection run 35631845044](https://github.com/andymac4182/mount-rs/actions/runs/35631845044) cancelled, while [Live Cloudflare R2 run 35631845090](https://github.com/andymac4182/mount-rs/actions/runs/35631845090) failed; the unrelated [W04 production-policy run 35631845034](https://github.com/andymac4182/mount-rs/actions/runs/35631845034) succeeded | No hosted WebDAV PASS is claimable from this tip; hosted/native lifecycle, provider, crash/power-loss durability, and broader concurrency remain OPEN, so local and source-backed runs do not change the production NO-GO decision |
| 2026-09-22 | Package integration boundaries | `node test/webdav-codec.mjs`: explicit SKIP because `MOUNTX_SOURCE` is unset; package-wide `node test/servers.mjs` reached the unrelated NFS lane first and hit its host-permission prerequisite | Do not promote the package-wide NFS failure or the skipped oracle row into WebDAV PASS evidence |

## Completion rule

The owning task may mark W01-WebDAV production-ready only when every
applicable gate above is PASS or explicitly accepted outside supported scope,
exact commands and prerequisites are recorded here, and this file is committed
with each implementation/test chunk. Until then the decision remains
**NO-GO**.
