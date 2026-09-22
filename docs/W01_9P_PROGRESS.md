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
| Public 9P exports and protocol behavior | Local PASS for the implemented codec, all 124 pinned constants, and all 274 upstream runtime `./9p` barrel exports; broader protocol/session parity remains partial | Pinned 9P differential, generated declarations, malformed/trailing coverage, deterministic fid/qid/cursor lifecycle tests, the six public default values, every upstream `./9p` barrel export, `P9DirentPacker.maxSize`, and public-session behavior |
| Session and connection objects | Local PASS for current exposed members and the bounded N-API mount-helper facade; hosted Linux N-API lifecycle PASS for the supported surface; parity remains partial | `P9Session.handleCall`/`destroy`, scalar `options`, live `driver`, `userFor`, debug-gated assertions, request-error/assertion callbacks, live `locks` and `fids`, stats/lifecycle, live property-shaped `clients`, peer, `closed`, attached stream exposure, identity/handle tests, injected shared lock-table option coverage, direct `./9p` probe/refusal/option/mount-helper/signal checks, configured `P9Server` reuse through the native mount option, direct mount-created scalar server-policy/session-callback mapping, and exact-SHA hosted automatic/direct/structural native 9P mounted I/O and cleanup; the direct `MountP9Options` audit found no additional unrepresented fields, while automatic cross-transport signal ownership remains an explicit scope boundary |
| Attached-stream contract | Local PASS | Node `attach(stream, options)` with typed peer/ownership/frame/in-flight bounds, ownership, duplicate attach, direct session calls, non-socket duplex, backpressure, write failure, and server-close tests |
| Native-listener stream boundary | Explicit supported-scope decision | Native Tokio-accepted connections expose `stream: undefined`; their peer is the transport source string when available (Unix socket path or TCP `address:port`) and is `null` only when absent. Callers requiring a Node `Duplex` use `server.attach`, whose attached connection retains the supplied stream and peer fallback |
| Linux native 9P | Hosted PASS for the supported Rust and N-API Linux lifecycle scope; crash/reset/half-close recovery is outside the library guarantee | Dedicated [Native 9P run `35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344), job `106427627397`, at `431affd660391a0b8ed99815e389ffe12ad229c2`, passed `9p`/`9pnet_fd` probing and all four ignored Rust native tests: concurrent file I/O/unmount, server-close/kernel-connection release, ordinary mount/unmount, and external umount. Exact SHA `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` then passed [Native 9P run `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703): N-API job `106558367429` passed automatic, direct `./9p` (including non-empty string `source`), and structural-driver mounted I/O/cleanup, while Rust job `106558367006` passed all four ignored native tests. The latest exact SHA `0d520a1d0a9af44e08e65c5f0638a640bb3c08db` also passed [Native 9P run `35671509538`](https://github.com/andymac4182/mount-rs/actions/runs/35671509538): N-API job `106569412372` passed automatic/direct/structural mounted I/O and cleanup plus direct native session/lock optional-shape assertions, and Rust job `106569412047` passed all four ignored native tests. Earlier runs remain below as history |
| Errors, cancellation, concurrency, crash and cleanup | Local deterministic PASS; supported hosted lifecycle PASS; crash/reset/half-close are supervisor-owned | Focused Rust/N-API lifecycle and fault tests cover broadcast shutdown, accept-loop close races, shutdown-aware in-flight permit waits, bounded task reaping, transport faults, and session destruction that wakes and drains `Tflush` waiters; the hosted native harness now passes eight concurrent mounted file write/read/rename/read workers plus server-close, kernel-connection-close, external umount, and bounded-unmount cleanup. Automatic recovery after process crash or arbitrary kernel reset/half-close remains explicitly outside the library contract |

## Current queue

- Retain the dedicated hosted Linux `Native 9P` workflow as the stable
  regression gate for kernel `9p`/`9pnet_fd` prerequisites, mounted I/O,
  concurrent file operations, server-close/kernel-connection teardown,
  external umount, and bounded unmount. It supports manual dispatch when a
  future 9P implementation packet needs an exact revision rerun.
- If a deployment claims recovery after process crash, arbitrary kernel reset,
  or half-close, qualify that behavior in a supervisor-level test and cleanup
  policy. Those recovery properties are not automatic guarantees of this
  library and are not promoted from the hosted lifecycle pass.
- Keep unsupported platform/client results explicit and separate from passes.
- The direct `./9p` mount-helper facade currently supports Linux-client probing,
  Unix/TCP refusal and option-string helpers, strict named 9P delegation through
  `mount9p`, 9P-only live-mount filtering/cleanup, and mounted `trans`/server/
  connection/`closed` views. It also accepts a configured native `P9Server` and
  adopts that listener, server policy, lock table, callbacks, and client set
  instead of creating a second listener. Its supported option bag is deliberately bounded
  to transport, host/port/path, msize, access/cache/uname/aname, read-only,
  driver inode, mount options, unmount timeout, transport-error callback, and
  shared-server injection, and the scalar server policy used when a mount
  creates its own listener: remote-peer admission, socket mode and directory
  policy, frame/in-flight bounds, negotiated `msize`, inode/read-only policy,
  ownership claims, debug mode, lock-table injection, and direct session
  `onError`/`onAssertion` callbacks. These callbacks apply to a listener created
  by the mount; an injected shared server retains its own callbacks. The direct
  `./9p` signal teardown is now supported. The pinned oracle audit found no
  additional unrepresented direct `MountP9Options` fields, and `P9Mount.source`
  is now type- and runtime-qualified as a non-empty string. Automatic
  cross-transport signal ownership remains an explicit scope boundary rather
  than a root automatic-mount claim. Exact SHA
  `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` in [Native 9P run
  `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703),
  N-API job `106558367429`, qualifies the supported Linux automatic, direct,
  and structural-driver native lifecycle views; Rust job `106558367006` also
  passed all four ignored native tests.
- The direct `./9p` probe helpers accept the oracle's optional platform override:
  `p9ClientProbe(platform?)` reports deterministic non-Linux/Linux simulation
  facts without attempting a mount, while `p9Platform(platform?)` maps the
  requested platform to `"linux" | undefined`. The host-only zero-argument
  probe still comes from the native binding. Exact SHA
  `9da45327a9e09a9f827a9630869d1a32119674e3` also passed [Native 9P run
  `35672845113`](https://github.com/andymac4182/mount-rs/actions/runs/35672845113):
  N-API job `106573050491` passed automatic/direct/structural mounted I/O and
  cleanup, while Rust job `106573049500` passed all four ignored native tests.
  Those hosted mount steps qualify the surrounding native facade; the
  synthetic override branches remain covered by the local host-independent
  helper regression.

## Supported-scope decisions

- Native kernel mounts are supported and qualified only on Linux with the
  host `9p`/`9pnet_fd` prerequisites and mount privilege. macOS support for
  this crate is the rootless wire/TCP server; no native macOS 9P client is
  claimed.
- Native Tokio listener connections deliberately expose no transferable Node
  `Duplex`; callers needing a Node stream use `P9Server.attach`. Native
  accepted connections retain a source-specific peer string (Unix socket path
  or TCP `address:port`) when available, with `null` only for an absent peer;
  the N-API
  low-level session now exposes scalar `options`, its live `driver`,
  `userFor(fid)`, debug-gated assertion retention/statistics, request-error and
  assertion callbacks, and a live `locks` client in addition to direct
  call/destroy, stats, and lifecycle. The standalone `P9LockTable`/`P9LockClient`
  surface is transport-backed and tested for conflict, ownership, rename, and
  release. `P9Server.clients` is now a live property-shaped array combining
  native and attached connections. `P9ServerOptions.locks` now accepts a
  `P9LockTable` and shares it across native and attached sessions, with live
  server/session option handles. The bounded `./9p` mount-helper facade is
  locally evidenced, including reuse of a configured native `P9Server`, while
  scalar server-policy fields and direct session `onError`/`onAssertion`
  callbacks are now mapped for mount-created listeners; an injected shared
  server keeps its own hooks. The direct `./9p` facade also installs one
  process-wide `SIGINT`/`SIGTERM` teardown pair for opted-in mounts and removes
  it after the last such mount closes; automatic cross-transport signal
  ownership remains an explicit scope boundary; the direct mount-option audit found
  no additional unrepresented fields. The
  `./9p` barrel now exposes
  the authoritative Rust-backed `FidTable` alias, live `P9Session.fids`, qid
  synthesis helpers, cursor/resume state, detached clunk views, and retained
  open-handle enumeration; the focused runtime test covers hardlink identity,
  large inode values, path remapping/release, mutable fid views, and a live
  opened session fid. Direct table mutation is a low-level inspection/testing
  seam: orderly production teardown remains protocol `Tclunk` or
  `P9Session.destroy`, not an arbitrary `clear()` on a live session. The
  constants/message-name/default part of the `./9p` barrel is complete and
  differentially checked across all 124 pinned constants and all 274 upstream
  runtime barrel exports, including the six public defaults. These remaining
  gaps are not silently accepted out of scope.
- N-API option snapshots intentionally contain only serializable effective
  policy. `onError` and `onAssertion` remain effective transport/session hooks,
  but their live JavaScript functions are not returned by `server.options` or
  `session.options`; the attached-stream metadata regression asserts that
  boundary while also checking the supplied `Duplex`, peer, and pre-listen
  `address()`/`path` representations. Corrected exact SHA
  `81cc6596c2c9562c3405df50126239a7bcb44f63` passed [Native 9P run
  `35670279904`](https://github.com/andymac4182/mount-rs/actions/runs/35670279904),
  including the direct native `stream: undefined` and transport-source
  peer-string assertions.
- The N-API facade normalizes the oracle's optional absence shapes: an
  unnegotiated `P9Session` reports `msize`/`version` as `undefined`, an unknown
  `userFor(fid)` is `undefined`, and a conflict-free `getlock()` is
  `undefined` on both the table and session lock client. The native binding's
  internal `null` results are not exposed at this supported JavaScript
  boundary.
- `P9Session.stats.messages` now follows the oracle's `Map<string, number>`
  shape at the JavaScript boundary instead of exposing the native binding's
  object/hash-map representation. The attached-session observability test and
  the direct hosted mount test both assert the `Tversion` count through
  `Map#get`; the generated declaration and typecheck cover the same shape.
- The direct `./9p` fid-view declaration now follows the native N-API
  representation: `DirCursor.offsets` is an array of `{ offset: bigint,
  index: number }` records, while `Fid.iounit` and `Fid.cursor` are writable.
  This is an explicit serializable N-API boundary from the oracle's internal
  offset map, not a claim that the native wire adapter exposes that `Map`.
  `node test/p9-fids.mjs` proves the runtime array and mutation behavior, and
  `node test/typecheck.mjs` proves the declaration; syntax and diff checks are
  also green. Exact SHA `ba20d29d7e8ad00b3c4b5270dc21cf6ab913e4c2` passed the
  N-API job `106578252549` in [Native 9P run `35674581481`](https://github.com/andymac4182/mount-rs/actions/runs/35674581481),
  including the Linux probe and automatic/direct/structural mounted-I/O
  checks; the companion Rust `native-9p` job `106578252700` also passed the
  Linux probe and all four ignored native lifecycle tests.
- `P9DirentPacker.maxSize` now matches the pinned oracle declaration and is
  backed by the native N-API packer's fixed budget. The local codec
  differential covers the getter and its `size + remaining` invariant in 44
  typed cases, while generated typecheck, fid/runtime, syntax, formatting,
  strict Clippy, and the focused Rust tests remain green. Exact SHA
  `b3757fd288e6f52888873838946343e7cd37f953` passed [Native 9P run
  `35675876913`](https://github.com/andymac4182/mount-rs/actions/runs/35675876913):
  N-API job `106582464900` passed the Linux probe, addon build, and automatic,
  direct `./9p`, and structural-driver mounted-I/O/cleanup checks; Rust job
  `106582465059` passed the Linux probe and all four ignored native lifecycle
  tests.
- Graceful server close, external unmount, and retryable unmount are in scope;
  the dedicated hosted run above verifies those Linux lifecycle paths.
  Automatic recovery after process crash or arbitrary kernel reset/half-close
  is not a library guarantee; a deployment claiming those properties needs a
  supervisor-level test and cleanup policy.

The dedicated hosted `Native 9P` workflow now also contains an N-API Linux job
that loads the kernel client, builds the public addon, and runs automatic,
direct `./9p`, and structural-driver mounted-I/O/cleanup checks as root. Exact
SHA `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` passed all three checks in
[run `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703),
N-API job `106558367429`, with the direct native check asserting the mounted
`source` is a non-empty string; the Rust native job `106558367006` passed all
four ignored lifecycle tests. The local pinned oracle check at that same
packet passed `124 constants; 274 barrel exports`. The direct native check is
opt-in outside that job and deliberately preserves its mountpoint and driver
root when teardown is not proven safe.

The current hosted packet at exact SHA
`e8c6043827e6cd0232a28f94b8fc665e25985f76` passed [Native 9P run
`35673543701`](https://github.com/andymac4182/mount-rs/actions/runs/35673543701):
Rust job `106575123716` passed all four ignored native tests, and N-API job
`106575123928` passed the Linux probe, addon build, automatic/direct/structural
mounted-I/O and cleanup checks. The direct test includes the `stats.messages`
`Map` assertion; hosted success qualifies the native facade at this revision,
not the local synthetic platform-override branch.

The latest implementation packet at exact SHA
`ba20d29d7e8ad00b3c4b5270dc21cf6ab913e4c2` passed the N-API job
`106578252549` in [Native 9P run `35674581481`](https://github.com/andymac4182/mount-rs/actions/runs/35674581481),
including the Linux probe, addon build, and automatic/direct/structural
mounted-I/O/cleanup checks. Its companion Rust `native-9p` job
`106578252700` also passed the Linux probe and all four ignored native
lifecycles; the local `p9-fids.mjs` runtime test remains the direct evidence
for the array-shaped fid cursor and writable fid fields.

The current implementation packet at exact SHA
`b3757fd288e6f52888873838946343e7cd37f953` passed [Native 9P run
`35675876913`](https://github.com/andymac4182/mount-rs/actions/runs/35675876913):
N-API job `106582464900` passed the Linux probe, addon build, and
automatic/direct/structural mounted-I/O and cleanup checks, while Rust job
`106582465059` passed the Linux probe and all four ignored native lifecycle
tests. The packet adds the missing `P9DirentPacker.maxSize` declaration and
runtime differential; local codec coverage is 44 typed cases, and production
remains NO-GO for the broader open gates.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | N-API 9P member representation boundaries | The focused session metadata regression now checks effective callback-hook omission from serializable option snapshots, attached `Duplex`/peer/closed state, and pre-listen string/null address/path views; the direct native test asserts the native listener's `stream: undefined` and transport-source peer string. Local typecheck, metadata, mount-helper, syntax, and diff checks passed. Corrected exact SHA `81cc6596c2c9562c3405df50126239a7bcb44f63` passed [Native 9P run `35670279904`](https://github.com/andymac4182/mount-rs/actions/runs/35670279904): N-API job `106565351978` passed automatic/direct/structural mounted I/O and cleanup, and Rust job `106565352174` passed all four ignored native tests | Broader upstream member parity, automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, and W01 acceptance remain open |
| 2026-09-22 | Hosted member-boundary rerun attempt | Exact SHA `03529cf30985c2be6503c2909b94e646565cf6fe` in [Native 9P run `35669536706`](https://github.com/andymac4182/mount-rs/actions/runs/35669536706) passed Rust job `106562666985` and the automatic N-API mount, but direct `./9p` stopped at the new assertion because the test incorrectly expected a native Unix `peer` to be `null`; the actual value was the transport-owned socket path. The assertion and scope wording are corrected to require `stream: undefined` plus a non-empty transport-source peer string | Corrected exact-SHA hosted rerun required; production remains NO-GO |
| 2026-09-22 | Corrected hosted member-boundary rerun | Exact SHA `81cc6596c2c9562c3405df50126239a7bcb44f63` in [Native 9P run `35670279904`](https://github.com/andymac4182/mount-rs/actions/runs/35670279904) passed N-API job `106565351978` (`PASS` automatic mount, direct `./9p` probe/mounted I/O/views/cleanup, and structural-driver callback reachability) and Rust job `106565352174` (`4 passed; 0 failed`), qualifying native `stream: undefined` plus the transport-source Unix peer string | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and W01 acceptance remain open; production remains NO-GO |
| 2026-09-22 | N-API 9P optional absence-shape parity | The JS facade now converts the native binding's `null` absence results to the oracle's `undefined` for pre-version `msize`/`version`, unknown `userFor(fid)`, and conflict-free `P9LockTable`/`P9LockClient.getlock`; generated declarations, metadata/lock runtime checks, syntax, typecheck, and diff checks pass locally. Exact SHA `0d520a1d0a9af44e08e65c5f0638a640bb3c08db` passed [Native 9P run `35671509538`](https://github.com/andymac4182/mount-rs/actions/runs/35671509538): N-API job `106569412372` passed automatic/direct/structural mounted I/O and cleanup plus the direct native session/lock assertions, and Rust job `106569412047` passed all four ignored native tests | Broader upstream member parity, automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, and W01 acceptance remain open; production remains NO-GO |
| 2026-09-22 | N-API 9P platform-probe argument parity | The direct `./9p` facade now accepts optional platform overrides for `p9ClientProbe(platform?)` and `p9Platform(platform?)`; typecheck covers Linux/Darwin override calls and the mount-helper regression checks deterministic simulated platform facts without attempting a mount. The host-only zero-argument probe remains native-backed. Exact SHA `9da45327a9e09a9f827a9630869d1a32119674e3` also passed [Native 9P run `35672845113`](https://github.com/andymac4182/mount-rs/actions/runs/35672845113): N-API job `106573050491` passed automatic/direct/structural mounted I/O and cleanup, and Rust job `106573049500` passed all four ignored native tests; the hosted mount steps do not replace the local synthetic-override assertions | Native Linux mount/lifecycle evidence is now current for this revision; automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and W01 acceptance remain open; production remains NO-GO |
| 2026-09-22 | N-API 9P `P9DirentPacker.maxSize` parity | The pinned oracle's public `maxSize` getter is now represented in the native Rust binding, generated declarations, direct `./9p` types, and codec runtime differential; the local differential passed 44 typed cases, with generated typecheck, fid/runtime, syntax, formatting, strict Clippy, and focused Rust tests green. Exact SHA `b3757fd288e6f52888873838946343e7cd37f953` passed [Native 9P run `35675876913`](https://github.com/andymac4182/mount-rs/actions/runs/35675876913): N-API job `106582464900` passed automatic/direct/structural mounted I/O and cleanup, and Rust job `106582465059` passed all four ignored native tests | Broader upstream member parity, automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, and W01 acceptance remain open; production remains NO-GO |
| 2026-09-22 | N-API 9P public barrel and default parity | Added the six pinned oracle defaults (`DEFAULT_P9_PORT`, `DEFAULT_SOCKET_MODE`, `DEFAULT_MAX_IN_FLIGHT`, `DEFAULT_MSIZE`, `P9_LOCK_EOF_END`, and `DEFAULT_MAX_LOCKS_PER_FILE`) to the direct facade, postlude binding, and generated declarations. The pinned runtime test passed all 124 constants and every one of the 274 upstream 9P barrel exports; local typecheck, syntax, helper, and diff checks passed. Exact SHA `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` passed [Native 9P run `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703): N-API job `106558367429` passed automatic/direct/structural mounted I/O and cleanup, and Rust job `106558367006` passed all four ignored native tests | Automatic cross-transport signal ownership remains an explicit scope boundary; native listener `stream: undefined`, supervisor-owned crash/reset/half-close recovery, and broader W01 acceptance remain open |
| 2026-09-22 | N-API 9P return-shape and direct-option parity | The declaration now narrows `P9Mount.source` to `string`; generated typecheck, `node --check test/p9-native.mjs`, the Darwin-safe native smoke test, the mount-helper regression, and `git diff --check` passed locally. The pinned oracle audit found no additional unrepresented direct `MountP9Options` fields. Exact SHA `3c884bd8c0d0199a17e4c355c36d45f660c7c786` passed [Native 9P run `35665824215`](https://github.com/andymac4182/mount-rs/actions/runs/35665824215): N-API job `106552944097` passed automatic/direct/structural mounted I/O and cleanup, including the non-empty `source` assertion, and Rust job `106552944349` passed all four ignored native tests | Automatic cross-transport signal ownership remains an explicit scope boundary; crash/reset/half-close recovery remains supervisor-owned and broader W01 acceptance remains open |
| 2026-09-22 | Hosted N-API 9P lifecycle gate | Run `35664614270`, N-API job `106547449823`, at exact SHA `1dcf4dee4d01fb5e3807335579659b54efd74351` passed `9p`/`9pnet_fd` probing, addon build, automatic N-API mounted I/O/cleanup (`PASS (9p)`), direct `./9p` mounted I/O/views/cleanup, and structural-driver mounted I/O/cleanup (`PASS (9p; read/write/unmount callback reachability)`); the Rust `native-9p` job `106547449501` also passed | Automatic cross-transport signal ownership and remaining mount controls remain open; crash/reset/half-close recovery remains supervisor-owned and broader W01 acceptance remains open |
| 2026-09-22 | Attached Node Duplex and session contract | `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked` passed 24 focused tests; `./scripts/cargo-shared check -p mount-rs-napi --locked` passed; `./scripts/cargo-shared clippy -p mount-rs-9p -p mount-rs-napi --all-targets --locked -- -D warnings` passed; `pnpm build:debug`, `node test/typecheck.mjs`, and host-enabled `node test/servers.mjs` passed; `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 pnpm test` passed, including the pinned 44-case 9P codec differential and artifact aggregation | PGlite/R2/native-mount opt-ins are explicit skips; current-revision hosted Linux native 9P, native mount fault/race/crash, and broader W01 acceptance remain open |
| 2026-09-22 | Hosted Linux native lifecycle and shutdown/reaping packet | Prior revision-matched run `35616832528` / `native-9p` job `106389895603` passed kernel-module probing plus privileged native mount/read/write/unmount; the new packet adds broadcast shutdown, active-connection close-race coverage, task-failure reporting and completed-task reaping, with `transport_lifecycle` 5/5, `transport_errors` 8/8, strict 9P Clippy and formatting passing locally | A fresh hosted run for the new packet is required; native reset/half-close/concurrency/crash evidence and broader W01 acceptance remain open |
| 2026-09-22 | Native concurrent mounted I/O harness | Added an ignored Linux-native test that launches eight bounded blocking workers for independent mounted write/read/rename/read round trips, then performs bounded unmount and refuses recursive cleanup after a failed lifecycle; the focused 9P target and strict 9P Clippy pass locally | Hosted execution on a revision containing this harness is required; native reset/half-close/crash evidence and broader W01 acceptance remain open |
| 2026-09-22 | Session teardown and `Tflush` cancellation | Session destruction now marks and wakes every in-flight request, drains the in-flight map, and releases a waiting `Tflush` with the existing destroyed-session error boundary; the regression test passes in the focused 9P target and strict Clippy | Native cancellation/close, reset, crash, and current hosted execution remain open |
| 2026-09-22 | Native server-close lifecycle harness | Added an ignored Linux-native test that performs mounted file I/O, closes the server side, waits for the kernel connection to close, and performs bounded unmount; failed paths refuse recursive mountpoint cleanup, and the focused target plus strict Clippy pass locally | Hosted execution is required; process-crash, reset/half-close, and broader W01 acceptance remain open |
| 2026-09-22 | Attached-stream option declaration parity | The generated `attach()` declaration now exposes the already-supported per-attachment `maxFrame` and `maxInFlight` bounds through `P9AttachOptions`; the TypeScript test compiles the peer/ownership/bounds shape, the attached-stream integration covers the oracle's default `peer: undefined` and per-attachment in-flight/frame limits, and native listener connections retain their transport-source peer string representation | Runtime/native lifecycle and broader upstream stream/attach parity remain open |
| 2026-09-22 | Shutdown-aware in-flight permit wait | The Rust connection loop now selects server/connection shutdown while waiting for a `maxInFlight` permit; a blocked-driver regression proves bounded server close instead of waiting behind a slow request, with `transport_lifecycle` 6/6 and the formatter passing locally | Fresh hosted native execution, native reset/half-close/process-crash evidence, and broader W01 acceptance remain open |
| 2026-09-22 | Current hosted rerun attempt | CI run `35624213028` at published commit `201b7724dced89122f2e310ff5145acb123a8ab7` was canceled before jobs materialized; it provides no current-revision native-9P result | Hosted native execution remains required; do not promote the prior lifecycle checkpoint to this packet |
| 2026-09-22 | Current hosted rerun attempt after attached-stream limits packet | CI run `35624869535` at published commit `e5f4dda0f2e79899f3631e6fbf7caa260497ad21` was canceled before jobs materialized (`jobs: []`); it provides no current-revision native-9P result | Hosted native execution remains required; do not promote the prior lifecycle checkpoint to this packet |
| 2026-09-22 | Current hosted native execution and unmount-race fix | Run `35625437327`, native-9p job `106418844564`, loaded `9p`/`9pnet_fd` and passed 3/4 ignored tests; `native_linux_server_close_releases_kernel_connection` failed because the connection-close monitor marked resource teardown complete before `P9Mount::unmount()` issued `umount(8)`. The local fix gives kernel unmount its own coordination state so server-close/EOF cannot suppress unmount; focused 9P tests 28/28, strict Clippy and formatting pass locally | Fresh hosted rerun must verify the fix, plus native reset/half-close/process-crash evidence and broader W01 acceptance remain open |
| 2026-09-22 | Corrective hosted rerun attempt | CI run `35626340158` at `c2290b269882c768ba64965e65cbe6626a971610` was canceled before jobs materialized (`jobs: []`); the follow-up also hardens unmount serialization against outer-task cancellation with a Tokio mutex, and the focused 9P tests 28/28, strict Clippy and formatting pass locally | A hosted run containing the unmount fix is still required, plus native reset/half-close/process-crash evidence and broader W01 acceptance |
| 2026-09-22 | Cancellation-safe unmount and supported-scope boundary | `P9Mount::unmount()` now has cancellation-safe mutex serialization and remains callable after `wait_closed()` resource teardown; the README and tracker define Linux-native, Node-attach, graceful-close, and supervisor-owned crash/reset boundaries. Focused 9P tests 28/28, strict Clippy and formatting pass locally | Hosted verification of the unmount fix remains required; unsupported crash/reset/half-close recovery is not a production claim |
| 2026-09-22 | Stable hosted Linux 9P qualification | Dedicated [Native 9P run `35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344), job `106427627397`, at exact SHA `431affd660391a0b8ed99815e389ffe12ad229c2`, passed kernel-module probing and the four ignored native tests; the log reports `4 passed; 0 failed`, including `native_linux_server_close_releases_kernel_connection` | Public parity remained partial at that revision, and the overall W01/release decision remains NO-GO; crash/reset/half-close recovery is supervisor-owned rather than a library claim |
| 2026-09-22 | N-API P9 session metadata parity | `pnpm --dir integrations/mount-rs-napi build:debug`, `node test/typecheck.mjs`, `node test/p9-session-metadata.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 typed cases), `./scripts/cargo-shared fmt --all -- --check`, `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked` (28 ordinary tests), and strict `./scripts/cargo-shared clippy -p mount-rs-napi -p mount-rs-9p --all-targets --locked -- -D warnings` passed. `node test/servers.mjs` could not reach its P9 phase because this host's unrelated NFS listener cleanup/relisten returned `Operation not permitted` | `P9Server.options`, `P9Session.options`, and `P9Session.userFor` are now evidenced; upstream driver/fid/lock/assertion/debug/mount/barrel surfaces and the property-shaped `clients` contract remain open; production NO-GO |
| 2026-09-22 | N-API P9 lock surface parity | `pnpm --dir integrations/mount-rs-napi build:debug`, `node test/typecheck.mjs`, `node test/p9-locks.mjs`, `node test/p9-session-metadata.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 typed cases), `./scripts/cargo-shared fmt --all -- --check`, `./scripts/cargo-shared test -p mount-rs-9p --all-targets --locked`, and isolated strict `CARGO_TARGET_DIR=/private/tmp/mount-rs-clippy-w01-20260922 ./scripts/cargo-shared clippy -p mount-rs-napi -p mount-rs-9p --all-targets --locked -- -D warnings` passed. The lock test proved grant/conflict/rename/release/inspection, and the session test proved its lock client shares authoritative transport state | `P9LockTable`/`P9LockClient` are now transport-backed; upstream driver/fid/assertion/debug/full fid graph, lock-table option injection, property-shaped `clients`, and 9P mount/barrel surfaces remain open; production NO-GO |
| 2026-09-22 | N-API P9 constants/barrel parity | `node test/typecheck.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/p9-constants.mjs`, `node --check p9.cjs`, and `git diff --check` passed. The test differentially checks all 124 upstream `./9p` constants and `messageName` results, including message numbers, 64-bit masks, qid bits, wire limits, versions, and Linux open flags; direct CommonJS assignments preserve ESM named-export discovery | Constants/message-name parity is closed; upstream driver/fid/assertion/debug/full fid graph, lock-table option injection, property-shaped `clients`, and 9P mount helpers remain open; production NO-GO |
| 2026-09-22 | N-API P9 fid-table/session parity | `pnpm --dir integrations/mount-rs-napi build:debug`, `node test/typecheck.mjs`, `node test/p9-fids.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-locks.mjs`, `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/p9-constants.mjs` (124 exports), `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 cases), `node --check integrations/mount-rs-napi/p9.cjs`, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (30 passed, 0 failed), and isolated warning-denied Clippy passed. Coverage includes live session fids, mutable path/open/iounit/cursor state, qid/cursor helpers, hardlink/release identity, large inode values, deterministic creation order, clunk snapshots, and real retained open-handle enumeration when the driver supplies one | Remaining upstream driver/assertion/debug, lock-table option injection, property-shaped `clients`, and 9P mount-helper parity remain open; hosted native lifecycle evidence is revision-scoped to the published SHA, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 driver and observability parity | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-observability.mjs`, `node test/p9-fids.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-locks.mjs`, pinned `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/p9-constants.mjs` (124 exports), pinned `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/9p-codec.mjs` (44 typed cases), root/`./9p` factory identity, `node --check` for the server and 9P facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (31 passed, 0 failed), and isolated warning-denied Clippy passed. Coverage includes the live `P9Session.driver`, debug-gated assertion readback/counters, duplicate-tag assertion/error reporting, malformed-frame `EPROTO` reporting without a header, negotiated unknown-message `ENOTSUP` reporting with its typed header, and Node error revival | Lock-table option injection, property-shaped `clients`, and 9P mount-helper parity remain open; hosted native evidence remains revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 property-shaped clients parity | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-observability.mjs`, `node test/p9-fids.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-locks.mjs`, host-enabled `node test/servers.mjs`, `node --check` for the server and 9P facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (31 passed, 0 failed), and isolated warning-denied Clippy passed. `P9Server.clients` is now a live property-shaped array combining native and attached connections, with generated declaration and runtime identity/teardown evidence; the host-enabled server integration passed. The earlier sandbox-only NFS relisten `Operation not permitted` was not promoted to a product failure | Lock-table option injection and 9P mount-helper parity remain open; hosted native evidence remains revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 lock-table option injection parity | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-session-metadata.mjs`, `node test/p9-observability.mjs`, `node test/p9-fids.mjs`, `node test/p9-locks.mjs`, host-enabled `node test/servers.mjs`, `node --check` for the server and 9P facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, focused `mount-rs-9p` tests (31 passed, 0 failed), and isolated warning-denied Clippy passed. `P9ServerOptions.locks` now accepts a `P9LockTable`; injected ranges are visible through the server and connection-session option handles and are shared with protocol lock state, with generated declaration and type/runtime evidence | 9P mount-helper parity remains open; hosted native evidence remains revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 bounded mount-helper facade | Release `pnpm --dir integrations/mount-rs-napi build`, generated `node test/typecheck.mjs`, `node test/p9-mount-helpers.mjs`, the focused P9 session/observability/fid/lock regressions, host-enabled `node test/servers.mjs`, `node --check` for the P9/server facades, `git diff --check`, `./scripts/cargo-shared fmt --all -- --check`, `mount-rs-napi --lib` (17 passed, 0 failed), focused `mount-rs-9p` tests (35 passed, 0 failed), and warning-denied Clippy for `mount-rs-napi`, `mount-rs-auto`, and `mount-rs-9p` passed. The direct `./9p` facade now exposes probe/refusal/option helpers, strict named `mount9p` delegation, 9P live-mount filtering/cleanup, and mounted transport/server/connection/closed views; the nested auto option bag carries the same bounded native 9P fields | Oracle shared-server/signal/extended server-policy mount options and hosted N-API native-mount lifecycle evidence remain open; prior hosted Linux evidence is revision-scoped, crash/reset/half-close recovery is supervisor-owned, and production remains NO-GO |
| 2026-09-22 | N-API P9 shared-server mount injection | The native mount option now accepts a configured `P9Server`; `mount9p` starts it only when the Linux client probe is usable, and the Rust adapter passes the exact bound transport through to `mount_9p` for client adoption. Generated declarations, N-API compile/build, typecheck, helper validation, syntax, and diff checks passed | Extended server-policy/signal/session callback controls and hosted N-API native-mount lifecycle evidence remain open; production remains NO-GO |
| 2026-09-22 | N-API P9 mount-created server policy | Direct and automatic 9P mount options now map scalar `P9ServerOptions` policy to a private mount-created listener, including remote admission, socket mode/shared-directory policy, frame/in-flight bounds, negotiated `msize`, inode/read-only/ownership/debug policy, and an injected `P9LockTable`; Rust and N-API mapping tests passed, generated declarations/typecheck, focused P9/N-API tests, host-enabled server integration, formatting, and strict Clippy passed | Direct session `onError`/`onAssertion` callback injection, process signals, and hosted N-API native-mount lifecycle evidence remain open; production remains NO-GO |
| 2026-09-22 | N-API P9 mount-created session callbacks | Direct and automatic 9P mount options now carry `onError` and `onAssertion` into the private listener's existing Rust `P9SessionHooks`; shared-server mounts retain the configured server's hooks. The debug N-API build, generated typecheck, observability/mount-helper/session/fid/lock runtime regressions, host-enabled server integration, N-API/Rust tests, formatting, syntax, diff checks, and strict Clippy passed | Process signals, remaining mount controls, and hosted N-API native-mount lifecycle evidence remain open; production remains NO-GO |
| 2026-09-22 | N-API P9 direct-facade signal teardown | The direct `./9p` mount helper now supports `signals` (default `true`) with one process-wide `SIGINT`/`SIGTERM` pair, unmount-all dispatch, handler removal after opted-in mounts close, and default-signal re-raise when no other listener remains; the signal lifecycle regression, mount-helper regression, syntax and diff checks passed | Automatic cross-transport signal ownership, remaining mount controls, and hosted N-API native-mount lifecycle evidence remain open; the release build was not completed because the isolated target exhausted `/private/tmp`; production remains NO-GO |

| 2026-09-22 | N-API 9P session message-statistics shape parity | The N-API postlude now converts the native `P9Session.stats.messages` object to the oracle's `Map<string, number>` shape, with generated declarations plus local attached-session observability, session-metadata, typecheck, syntax, and diff checks passing. The direct native test also asserts `Map#get("Tversion")`; `node test/p9-native.mjs` is an expected host-gated skip locally. Exact SHA `e8c6043827e6cd0232a28f94b8fc665e25985f76` passed [Native 9P run `35673543701`](https://github.com/andymac4182/mount-rs/actions/runs/35673543701): N-API job `106575123928` passed automatic/direct/structural mounted I/O and cleanup, and Rust job `106575123716` passed all four ignored native tests | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and W01 acceptance remain open; production remains NO-GO |
| 2026-09-22 | N-API 9P fid-view declaration and representation parity | The direct `./9p` declaration now matches the native N-API fid view: cursor offsets are an array of `{ offset: bigint, index: number }` records, and `Fid.iounit`/`Fid.cursor` are writable. `node test/p9-fids.mjs`, `node test/typecheck.mjs`, `node --check p9.cjs`, and `git diff --check` passed locally. Exact SHA `ba20d29d7e8ad00b3c4b5270dc21cf6ab913e4c2` passed N-API job `106578252549` and Rust job `106578252700` in [Native 9P run `35674581481`](https://github.com/andymac4182/mount-rs/actions/runs/35674581481); the N-API job passed the Linux probe and automatic/direct/structural mounted-I/O/cleanup checks, and the Rust job passed the Linux probe plus all four ignored native lifecycle tests | Broader upstream member parity, automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, and W01 acceptance remain open; production remains NO-GO |

## Completion rule

The owning task may mark W01-9P production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
