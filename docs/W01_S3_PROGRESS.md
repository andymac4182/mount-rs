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
| Public S3 exports and protocol behavior | In progress (Node scope explicit) | Pinned-oracle runtime scope audit, Rust codec/API differentials, and generated declarations |
| Session, streaming, bucket-map and connection objects | In progress | Buffered/streamed request/response, options, assertions, buckets, connections, and peer-fault evidence |
| Structural driver and local lifecycle | In progress | Oracle-backed source/bucket construction, mixed structural bucket maps, isolation, session replacement, close sweep, terminal-race handling, and local filesystem behavior |
| Live AWS/R2 provider | External gate | Fresh authenticated service result; credentials remain externally injected |
| Faults, cancellation, concurrency and durability | In progress | Durable-driver `syncfs` barriers now gate successful mutations; crash/power-loss restart, ordering, and provider failure evidence remain open; local multipart Complete/Abort terminal races, failed-Complete retry release, injected assembly-read failure recovery, cancellation-safe staged PUT/part replacement, bounded concurrent multipart publication, and native-filesystem process-restart handoff are covered |

## Current queue

- Keep the supported Node `./s3` server/session boundary pinned by the exact
  oracle runtime scope audit; keep the 153 oracle-only codec/helper members
  explicitly Rust-owned rather than implying JavaScript parity.
- Qualify live AWS/R2 behavior with fresh credentials and record any service
  limitations separately from local structural-driver evidence.
- Run fault, cancellation, concurrency, restart, and durability matrices.
- Preserve explicit S3 protocol limitations and provider prerequisites.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Wired provider workflow trigger coverage | Live AWS and Cloudflare R2 workflow path filters now include `transports/mount-rs-s3/**`; the repository regression script passes for both workflows, so future S3 transport changes cannot silently skip provider qualification. This changes triggering only, not service acceptance | Fresh authenticated AWS/R2 result, provider failure diagnosis, crash/power-loss durability, broader ordering/concurrency, complete oracle member parity, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |
| 2026-09-22 | Added durable mutation barriers | Every successful S3 mutation now awaits `FsDriver::syncfs` when the selected driver advertises `durable_writes`; a barrier failure is returned instead of acknowledged, while volatile drivers remain unchanged. `./scripts/cargo-shared test -p mount-rs-s3 --all-targets --locked --offline` passed the durable barrier regression alongside the existing gateway packet | Physical power-loss/torn-write evidence, provider-backed durability, broader upload/complete ordering, live AWS/R2, complete oracle-specific codec/member parity, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |
| 2026-09-22 | Reconciled peer-fault and connection packet | Remote S3 loopback-only hardening preserved; Rust S3 gateway now exposes bounded drain timeout, live accepted-connection tracking, peer-aware `Connection`/`Server` transport hooks, and reset-on-close coverage; `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-s3-target ./scripts/cargo-shared test -p mount-rs-s3 --all-targets --locked --offline` passed 4 unit, 6 chunked, 17 gateway, and 5 public-API tests with loopback host permission | Complete member parity, streaming N-API binding, direct JavaScript peer-fault evidence, live AWS/R2, and broader fault/durability/concurrency/native gates |
| 2026-09-22 | Exposed streaming S3 session through N-API | `S3Session.handleRequestStream` now accepts async iterables or Web `ReadableStream` request bodies, returns an async-iterable response body with cancellation, exposes coherent `stats()` snapshots, and preserves `S3Server.session`; generated release `pnpm build`, package typecheck, focused release binding load, and host-enabled `node test/servers.mjs` passed streamed PUT/GET, multi-chunk backpressure, response cancellation, generator failure mapping, bucket isolation, and metric deltas; S3 Rust tests passed 4 unit, 6 chunked, 17 gateway, and 5 public-API cases; N-API `cargo check`/Clippy, formatting, and the strict TypeScript fixture check passed | Direct JavaScript peer-fault evidence was still pending for this earlier chunk; complete oracle parity, live AWS/R2, and broader fault/durability/concurrency/native gates remained open |
| 2026-09-22 | Closed the applicable S3 N-API member and peer-fault packet | Added `S3Server.connections`, `onTransportError`, debug-gated assertion accounting, non-secret effective session options, session-owned bucket wrappers, and generated declarations. The Rust gateway target passed 4 unit, 6 chunked, 18 gateway, and 5 public-API tests; N-API library tests passed 16/16; release `pnpm build`, package typecheck, strict Clippy, formatting/diff checks, and host-enabled `node test/servers.mjs` passed idle connection cleanup, bucket/session views, streamed traffic, cancellation, and one typed peer-aware callback after a direct Node socket reset | Live AWS/R2, complete oracle-specific codec/member parity, restart/durability/concurrency matrices, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |
| 2026-09-22 | Closed the applicable S3 structural-factory construction slice | The pinned-oracle `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/structural-factories.mjs` differential now covers empty maps, mixed structural-driver maps, single-bucket options, invalid sources, seven invalid bucket-name cases, and a non-ASCII UTF-16 boundary; invalid names preserve the oracle's `TypeError` class/message through the N-API facade. `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-s3-target ./scripts/cargo-shared check -p mount-rs-s3 -p mount-rs-napi --locked --offline`, warning-denied Clippy, release build, generated typecheck, and distribution/export checks passed | Live AWS/R2, complete oracle-specific codec/member parity, close/restart/durability/concurrency matrices, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |
| 2026-09-22 | Closed the local multipart restart/terminal-race slice | `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-s3-target ./scripts/cargo-shared test -p mount-rs-s3 --test gateway --locked --offline` passed 22/22 with loopback access. New coverage proves a replacement `S3Session` lists and completes staged parts from the shared filesystem, `S3Session::close` sweeps every bucket and remains idempotent while the session answers new requests, and concurrent Complete/Abort has exactly one terminal winner with no staging debris. Exclusive `.finalizing` claims return `NoSuchUpload` to the loser and are released on pre-publication failure; late part writes are refused once finalization is visible | Crash/power-loss recovery, provider-backed durability, broader upload/complete ordering, live AWS/R2, complete oracle-specific codec/member parity, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |
| 2026-09-22 | Added failed-Complete retry regression | The focused `invalid_multipart_complete_releases_finalization_claim_for_retry` gateway test passed: a deliberately invalid ETag returns `InvalidPart`, removes the filesystem-visible `.finalizing` claim, and permits a subsequent correct Complete and GET; full provider/native/hosted and crash/power-loss boundaries remain separate | Crash/power-loss recovery, provider-backed durability, broader upload/complete ordering, live AWS/R2, complete oracle-specific codec/member parity, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |
| 2026-09-22 | Added cancellation-safe staging and multipart ordering coverage | Streaming PUT and multipart part replacement now use unique private staging paths with atomic rename; cancellation removes abandoned staging, preserves an existing part, and releases the debug request ticket. A bounded concurrent two-part upload publishes and completes in numeric order. The loopback-enabled S3 target passed 4 unit, 6 chunked, 26 gateway, and 5 public-API tests; release N-API build, generated typecheck/distribution, host-enabled `node test/servers.mjs`, process-restart, and exact Node scope checks also passed | This closes local cancellation and bounded in-process multipart ordering only; power-loss/torn-write durability, provider-backed failure, live AWS/R2, complete oracle-specific codec/member parity, and native/hosted lifecycle acceptance remain open; production decision stays NO-GO |
| 2026-09-22 | Exercised the N-API multipart replacement-session boundary | Regenerated the release addon/declarations with `CI=true CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-s3-target pnpm build`; host-enabled `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/servers.mjs` passed the direct N-API replacement-session create/part/list/complete/GET flow alongside streamed traffic, cancellation, bucket isolation, connection cleanup, and one typed peer-fault callback; `node test/typecheck.mjs` and `node test/distribution.mjs` passed | This is same-process N-API evidence over a shared native Filesystem, not native mount, hosted CI, crash/power-loss durability, live AWS/R2, or complete oracle codec/member parity; production decision stays NO-GO |
| 2026-09-22 | Exercised native-filesystem process-restart multipart recovery | `node test/s3-restart.mjs` passed: a child process intentionally exited without `S3Server.close()` after CreateMultipartUpload/UploadPart, and a fresh `createNodeFsDriver` plus S3 server listed, completed, and read the staged object; this is process-restart evidence over the local filesystem, not a power-loss or hosted-native result | Crash/power-loss recovery, provider-backed durability, broader upload/complete ordering, live AWS/R2, complete oracle-specific codec/member parity, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |
| 2026-09-22 | Pinned the Node `./s3` supported-scope boundary | `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/s3-barrel-scope.mjs` passed with 155 oracle runtime exports, 255 package exports, and the exact 153 oracle-only codec/helper names scoped out; supported N-API S3 server/session/streaming classes and factory identity remain present | This explicitly scopes the pure codec/helper barrel to Rust; live AWS/R2, crash/power-loss recovery, broader ordering/concurrency, and native/hosted lifecycle evidence remain open; production decision stays NO-GO |

## Explicit boundaries

- Local Rust and loopback structural-driver tests are not live AWS or
  Cloudflare R2 acceptance.
- A missing or expired provider credential is a blocked external prerequisite,
  not a passing service result.
- Native Linux/Windows/macOS mount and hosted lanes remain separate from this
  mount-free transport evidence.
- The Node `./s3` package entry intentionally supports the N-API
  server/session facade; the oracle-only pure codec/helper barrel is Rust-owned
  and is not counted as Node parity.

## Completion rule

The owning task may mark W01-S3 production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
