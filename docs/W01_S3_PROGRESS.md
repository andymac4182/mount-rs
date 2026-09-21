# W01-S3 progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md)

Owner: delegated W01-S3 task (thread `01a0c456-dd32-7803-8592-58a469846929`)

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns S3 protocol, SigV4/XML/chunked behavior, session and
streaming APIs, bucket-map construction, connection faults, providers, and
restart/concurrency boundaries.

| Gate | State | Required evidence |
| --- | --- | --- |
| Public S3 exports and protocol behavior | In progress | Pinned-oracle codec/API differentials and generated declarations |
| Session, streaming, bucket-map and connection objects | In progress | Buffered/streamed request/response, options, assertions, buckets, connections, and peer-fault evidence |
| Structural driver and local lifecycle | In progress | Mixed structural bucket maps, isolation, close, restart, and local filesystem behavior |
| Live AWS/R2 provider | External gate | Fresh authenticated service result; credentials remain externally injected |
| Faults, cancellation, concurrency and durability | Open | Reset/close, iterator cancellation, crash/restart, ordering, and provider failure evidence |

## Current queue

- Close any remaining oracle-specific S3 session/server/member parity and keep
  unsupported members explicitly scoped.
- Qualify live AWS/R2 behavior with fresh credentials and record any service
  limitations separately from local structural-driver evidence.
- Run fault, cancellation, concurrency, restart, and durability matrices.
- Preserve explicit S3 protocol limitations and provider prerequisites.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Reconciled peer-fault and connection packet | Remote S3 loopback-only hardening preserved; Rust S3 gateway now exposes bounded drain timeout, live accepted-connection tracking, peer-aware `Connection`/`Server` transport hooks, and reset-on-close coverage; `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-s3-target ./scripts/cargo-shared test -p mount-rs-s3 --all-targets --locked --offline` passed 4 unit, 6 chunked, 17 gateway, and 5 public-API tests with loopback host permission | Complete member parity, streaming N-API binding, direct JavaScript peer-fault evidence, live AWS/R2, and broader fault/durability/concurrency/native gates |
| 2026-09-22 | Exposed streaming S3 session through N-API | `S3Session.handleRequestStream` now accepts async iterables or Web `ReadableStream` request bodies, returns an async-iterable response body with cancellation, exposes coherent `stats()` snapshots, and preserves `S3Server.session`; generated release `pnpm build`, package typecheck, focused release binding load, and host-enabled `node test/servers.mjs` passed streamed PUT/GET, multi-chunk backpressure, response cancellation, generator failure mapping, bucket isolation, and metric deltas; S3 Rust tests passed 4 unit, 6 chunked, 17 gateway, and 5 public-API cases; N-API `cargo check`/Clippy, formatting, and the strict TypeScript fixture check passed | Direct JavaScript peer-fault evidence was still pending for this earlier chunk; complete oracle parity, live AWS/R2, and broader fault/durability/concurrency/native gates remained open |
| 2026-09-22 | Closed the applicable S3 N-API member and peer-fault packet | Added `S3Server.connections`, `onTransportError`, debug-gated assertion accounting, non-secret effective session options, session-owned bucket wrappers, and generated declarations. The Rust gateway target passed 4 unit, 6 chunked, 18 gateway, and 5 public-API tests; N-API library tests passed 16/16; release `pnpm build`, package typecheck, strict Clippy, formatting/diff checks, and host-enabled `node test/servers.mjs` passed idle connection cleanup, bucket/session views, streamed traffic, cancellation, and one typed peer-aware callback after a direct Node socket reset | Live AWS/R2, complete oracle-specific codec/member parity, restart/durability/concurrency matrices, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |

## Explicit boundaries

- Local Rust and loopback structural-driver tests are not live AWS or
  Cloudflare R2 acceptance.
- A missing or expired provider credential is a blocked external prerequisite,
  not a passing service result.
- Native Linux/Windows/macOS mount and hosted lanes remain separate from this
  mount-free transport evidence.

## Completion rule

The owning task may mark W01-S3 production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
