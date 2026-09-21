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

- Close remaining applicable S3 session/server/member parity.
- Qualify live AWS/R2 behavior with fresh credentials and record any service
  limitations separately from local structural-driver evidence.
- Run fault, cancellation, concurrency, restart, and durability matrices.
- Preserve explicit S3 protocol limitations and provider prerequisites.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Reconciled peer-fault and connection packet | Remote S3 loopback-only hardening preserved; Rust S3 gateway now exposes bounded drain timeout, live accepted-connection tracking, peer-aware `Connection`/`Server` transport hooks, and reset-on-close coverage; `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-s3-target ./scripts/cargo-shared test -p mount-rs-s3 --all-targets --locked --offline` passed 4 unit, 6 chunked, 17 gateway, and 5 public-API tests with loopback host permission | Complete member parity, streaming N-API binding, direct JavaScript peer-fault evidence, live AWS/R2, and broader fault/durability/concurrency/native gates |

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
