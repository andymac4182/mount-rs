# W01 progress ledger

This file is the working-time ledger for W01, **Core and mountx behavioral
parity**. It is intentionally separate from `WORK_TRACKER.md`: the tracker
records milestone acceptance, while this file records the remaining work in a
form that can be updated after every implementation or evidence run.

Last refreshed: **2026-09-22** (Australia/Brisbane)

## How to read the numbers

The percentages below are provisional planning percentages, not release
claims. They measure the scoped work item against its listed closure evidence;
they do not promote a component pass into native, hosted, or live-provider
acceptance.

- `Done` means the listed local implementation and evidence are complete.
- `In progress` means meaningful evidence exists, but a required boundary is
  still open.
- `External gate` means the remaining result depends on credentials,
  privileges, another operating system, hosted CI, or a provider service.
- `Open` means implementation or focused evidence is still required.

For time, record actual engineering time in the `Actual h` column. The
remaining ranges are planning estimates only. External-gate time is kept
separate because it cannot be predicted from local coding effort.

## W01 roll-up

| ID | Work item | Status | Completion | Remaining engineering time | External dependency |
| --- | --- | --- | ---: | ---: | --- |
| W01.1 | Applicable public exports and behavior ledger | In progress | 77% | 16–32 h | Hosted/native transport qualification |
| W01.2 | Upstream skip classification and required coverage | In progress | 65% | 8–16 h | Linux uid-0/hosted lanes, provider credentials |
| W01.3 | Seeded cross-engine traces | In progress | 75% | 4–12 h | Live R2 credentials |
| W01.4 | Supported macOS/Linux errors, data, links, times, handles, concurrency, lifecycle | In progress | 35% | 16–32 h | Linux FUSE, hosted Windows/Linux, native/FSKit prerequisites |
| W01.5 | Bounded deterministic concurrency packet | Done | 100% | 0 h | Follow-up races remain classified under W01.4 |
| **W01** | **Weighted planning view** | **Open** | **66%** | **44–92 h** | **Several gates are environment-dependent** |

The W01 percentage is a planning indicator calculated from the five work-item
percentages, weighted 25% / 20% / 20% / 25% / 10% for W01.1–W01.5. It must not
be read as “65% of release acceptance”; W01 remains open until every required
gate below is either passed or explicitly accepted as outside the supported
scope.

## Transport-owned W01 tracks

W01 remains one production gate, while implementation and evidence are split
into transport-owned tracks so a local pass in one transport cannot mask a
blocker in another. Each track keeps its own focused tests and updates this
ledger in the same commit as an implementation/evidence chunk.

| Track | Scope | Current boundary | Production-ready gate |
| --- | --- | --- | --- |
| W01-FUSE | FUSE protocol, mount-free session, native mount, callbacks and lifecycle | Focused Rust/N-API protocol/session evidence exists; native Linux, callback events, remaining session parity and lifecycle races remain | Hosted Linux native mount/read/write/unmount, callback-event and lifecycle evidence, plus the supported macOS/FSKit decision |
| W01-9P | 9P protocol, session, connection, attach and mount lifecycle | Rust/N-API attached Node Duplex with typed per-attachment peer/ownership/frame/in-flight bounds and source-specific peer absence (`undefined` for an attached stream, `null` for a native listener connection), direct session, scalar server/session options, `userFor`, ownership, duplicate-attach, bounded backpressure, write-fault, server-teardown, broadcast-shutdown, active-connection close-race, shutdown-aware permit waits, bounded task-reaping, and session-destroy/Tflush-wakeup evidence passes; the dedicated [Native 9P run `35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344) at `431affd` passed kernel probing plus all four ignored Linux lifecycle tests; native Tokio listener connections intentionally expose no Node stream and use the supported `attach` seam | Upstream driver/fid/lock/assertion/debug, property-shaped `clients`, and mount/barrel parity remain open; crash/reset/half-close recovery is explicitly supervisor-owned rather than a library claim, and the overall W01/release decision remains NO-GO |
| W01-NFS | NFSv3/v4 router, sessions, handles, native mount and lifecycle | v3/v4 direct routing, shared server state, BigInt handle snapshots, active connection objects/count, close/wait lifecycle, v4 view, process-lifetime session continuity across an orderly TCP reconnect, rootless pipelined v3 dispatch, pinned 266-case upstream conformance, codec differential, bounded `maxHandles`/NFSv4 pinning, bounded v4 channel/state knobs, and `maxLocksPerFile` enforcement for existing lock state are evidenced; full stateful matrix and native/hosted qualification remain | Shared v3/v4 handle/state proof, native macOS/Linux lifecycle, v4 behavior matrix, and crash/close evidence |
| W01-S3 | S3 protocol, session, streaming, providers and lifecycle | Local protocol and structural-driver evidence exists; live provider and complete member parity remain | Applicable API ledger, live AWS/R2, fault/restart and concurrency evidence |
| W01-WebDAV | WebDAV protocol, locks, session, streaming and lifecycle | Local protocol/session evidence exists; provider/native lifecycle remains | Applicable API ledger, auth/lock durability and restart evidence |
| W01-Auto/CLI | Auto selection, mount facade and SDK-backed consumers | Focused option/consumer paths exist; cross-transport native lifecycle remains | Per-transport options/callback ownership and signal/async-dispose evidence |
| W01-Provider/Native | Providers, hosted CI, FSKit, Windows, crash and concurrency | Local capability-limited packets exist; external lanes remain | Fresh live-provider and hosted/native results with no prerequisite-gated acceptance rows |

The canonical detailed FUSE ledger is
[docs/W01_FUSE_PROGRESS.md](./W01_FUSE_PROGRESS.md). Its 2026-09-22
session-controls chunk adds public construction options and lifecycle/error
observability while preserving the established durable `FLUSH` sync default.
The follow-up N-API chunk adds the Rust-backed `FuseSession` class and public
`./fuse` facade with generated declarations, typed options/defaults, negotiated
state, inode views, request/reply/error counters, callbacks, notification
encoders, destroy-state readback, and raw INIT/LOOKUP/READLINK coverage. The
focused package/build/typecheck evidence passes. The native transport follow-up
adds owned `FuseTransportError` kinds, `FuseMountHooks`, `mount_with_hooks`,
exactly-once terminal reporting, callback-panic isolation, and a mount-free
Unix-stream protocol-failure harness; native `/dev/fuse` callback delivery is
still an external gate. macOS/FSKit activation, cancellation, concurrency,
crash/restart and durability remain open.
The latest FUSE follow-up registers positional read workers by request unique:
`FUSE_INTERRUPT` aborts a known in-flight read, unknown targets retain the
existing `EAGAIN` boundary, and serial stateful requests drain read workers
before mutation or release. A Linux-gated Unix-stream regression proves that
interrupting a blocked read leaves the session open and that orderly stop stays
callback-silent. Host all-target FUSE tests, strict Clippy, formatting/diff,
and Linux-target strict Clippy pass; hosted `/dev/fuse` interrupt behavior,
native mutation/write concurrency, close/crash/restart, callback events, locks
and durability remain external, so W01 stays NO-GO.

The detailed 9P ledger is [docs/W01_9P_PROGRESS.md](./W01_9P_PROGRESS.md).
Its 2026-09-22 packet adds the N-API `attach(stream, options)` boundary,
direct `P9Session.handleCall`/`destroy`, attached connection stream/peer/closed
state, scalar server/session options and `P9Session.userFor`, shared byte-range
lock state, ownership and duplicate-attach handling,
bounded frame dispatch/backpressure, write-failure reporting, and server-close
teardown. Session destruction now wakes and drains in-flight `Tflush` waiters
with the existing destroyed-session error boundary. It also adds ignored
Linux-native harnesses for eight concurrent
mounted file write/read/rename/read workers followed by bounded unmount. The
native packet also closes the server side, waits for the kernel connection to
close, and unmounts within the same bound. The
focused Rust/N-API gates and pinned 44-case 9P codec differential pass. The
dedicated hosted [Native 9P run `35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344)
passed the Linux kernel-client module checks and all four ignored mounted
lifecycle tests at exact SHA `431affd`. Native listener connections expose
`stream: undefined` by deliberate supported-scope decision because their Tokio
stream is not transferable to a Node `Duplex`; crash/reset/half-close recovery
remains supervisor-owned rather than a library claim.

## Detailed work items

### W01.1 — Applicable public exports and behavior ledger

Current estimate: **77%**. The mount-free core contract is strong; the
remaining work is concentrated in the public transport and native lifecycle
surfaces.

| Sub-item | State | Evidence / remaining action | Completion | Actual h |
| --- | --- | --- | ---: | ---: |
| Core Rust, Node, memory, and oracle contract | Done | 101-step pinned trace: 79 successful results, 22 expected errors, zero mismatches/skips | 100% | — |
| Local persistence and consumer paths | Done locally | Rust/Node SDK, CLI, SQLite, object-store, and chunked local paths pass; live providers remain separate | 90% | — |
| FUSE wire codecs and whole-message framing | Done focused | Typed body table, raw/unknown framing, protocol-minor differentials, malformed/trailing checks | 95% | — |
| Rust-backed mount-free FUSE session | In progress | INIT, options, cache, negative lookup, flush, callbacks, counters, lifecycle readback, inode view, generated N-API facade, native transport-error hooks, and plain RENAME2 pass; remaining operation/native-session parity is open | 84% | — |
| Structural driver adapter and server factories | In progress | Focused oracle tests and macOS NFS structural mount pass; hosted Linux/Windows and full factory lifecycle remain | 75% | — |
| Auto/mount option and lifecycle surface | In progress | Shared `useDriverIno`, focused native `fuse`/`9p`/`nfs` option bags, configured FUSE `Mounted.source` mapping, package-level `signals` teardown, positive NFS `Mounted.port` readback, `Mounted[Symbol.asyncDispose]()` disposal, shared NFS plus already-listened 9P server handles, transport-specific automatic transport-error callbacks for FUSE/9P/NFS, and the root auto `onTransportError` adapter now pass through the N-API auto facade; FUSE request callbacks, runtime callback-event qualification, remaining option/session members, mount object details, and full lifecycle parity remain | 64% | — |
| NFS and 9P complete session/server contracts | Partial | NFS now exposes a read-only N-API session view with v3/v4-aware direct request routing, shared v3/v4 counters and sorted BigInt handle snapshots, a v4 session view, mounts, destroyed state, live connection objects/count, and close/wait lifecycle; 9P now exposes direct raw-frame handling, the bounded Node attached-stream contract, and the native-listener stream scope decision alongside its session/connection view; the full upstream object/session/handle/attach surface and native qualification remain | 75% | — |
| S3 and WebDAV public Node surfaces | Partial | Native server facades exist; S3/WebDAV `drainTimeout` and `onTransportError` option shapes now map, both expose buffered direct `handleRequest`, WebDAV malformed-connection, direct peer-reset, lock-conflict, expiry, malformed-request, same-driver recreation, and same-session parallel-request evidence passes, both server objects expose shared session/statistics views, S3/WebDAV expose live connection views, S3 peer-aware connection-error reporting passes at the Rust transport boundary, and the WebDAV subpath now exposes pinned constants/status tables, protocol/XML/lock helpers, active lock-record and recursive owner views, and streamed request/response body bindings with direct class 1/2/3 method, LOCK/UNLOCK, cancellation, and error coverage; its pure pinned oracle differential also passes; S3 also exposes its incremental stream boundary; complete option/member parity and external lifecycle gates remain | 72% | — |
| CLI parity and native consumer behavior | Partial | SDK-backed Rust/Node CLI and macOS NFS self-tests pass; exact oracle/native/hosted coverage remains | 65% | — |

Latest W01.1 action: the shared `useDriverIno` option was added to the public
auto options and threaded through the native FUSE, 9P, and NFS mount paths.
The auto option mapping tests (16 N-API unit tests), locked FUSE/9P/NFS tests,
strict FUSE/auto Clippy, release build, generated typecheck, authorized macOS
NFS native mount/read/write/unmount test with positive `Mounted.port` readback
and `Mounted[Symbol.asyncDispose]()` disposal, and opt-in child-process signal
teardown test passed with the nested NFS bag and `useDriverIno: false`. The
configured FUSE `Mounted.source` mapping is compiled and exposed, and the
`x86_64-unknown-linux-gnu` FUSE/auto transport check passed; a hosted Linux
native-FUSE runtime result is still required. The same authorized macOS NFS
lane now adopts an already-created NFS server through the auto option bag and
confirms the mount port identity and teardown. Already-listened 9P server
handles are mapped through the same bag, while a hosted Linux 9P runtime is
still required. The auto FUSE, 9P, and NFS bags now own their
transport-specific `onTransportError` hooks through the mount/session or
mount-created server lifecycle; locked crate/N-API tests, release build, and
generated typecheck cover callback construction and ownership. The authorized
macOS lane exercises the NFS callback construction/teardown, while the direct
NFS/9P fault packets remain the event-delivery evidence; a Linux FUSE runtime
event and a root auto callback event remain unverified. This is a focused
option/lifecycle result; FUSE request callbacks, the complete option/session
surface, and hosted/native lifecycle gates stay open. The same lane now
configures both root and focused NFS callbacks, exits cleanly with status 0,
and confirms that adopting a shared NFS server does not retain an unused
mount-level callback in the live mount object. The next server-object slice
adds N-API `S3Session` and `WebdavSession` views behind the existing server
objects: bucket/method counters, error/reply totals, WebDAV lock count, and
assertion readback are exercised by the real loopback PUT/GET/404 tests. The
S3 session assertion list is currently an explicit empty native boundary
because the Rust S3 session does not retain assertion messages; this is not
full S3 session parity. The NFS server-object follow-up adds N-API `NfsSession`
behind the NFS server: synchronized request/reply/error/drop/procedure stats,
mount records, and destroyed-state readback are exercised by real loopback
NULL/MOUNT/GETATTR traffic. The server's live `connections` count is checked
while the socket is open and after close. The malformed-record callback and
cleanup path also pass. The same view exposes direct `handleCall` for raw
unframed NFSv3 RPC records, with a direct NULL reply checked independently of
the socket. NFS library tests (30/30), NFS integration targets
(rootless wire 1, transport errors 4, v4 barrier 1, v4 wire 2), N-API library
tests (16/16), the N-API server integration, release addon build, generated
typecheck, repository formatting, and focused cross-transport Clippy pass; this
is still a read-only view with process-local connection close state rather than
complete NFS unified session/member or crash/durability parity. The `./nfs` package
subpath now re-exports the same
`NfsServer`, `NfsSession`, and `NfsConnection` constructors/classes as the root facade, and the
pinned NFS codec differential verifies that identity without mutating root
exports. The analogous `P9Session.handleCall` binding is exercised on a live
connection session with a direct `Rversion` reply, while the Rust/N-API 9P
integration, pinned 44-case differential, generated typecheck, release build,
and combined 9P/NFS/N-API Clippy remain green. These direct methods still do
not establish the oracle's v3/v4 NFS router or 9P stream/attach parity. The S3
server-object follow-up now exposes typed buffered `S3Session.handleRequest`
and incremental `S3Session.handleRequestStream` request/response bodies with
header mapping, cancellation, and an async metrics snapshot; direct streamed
PUT/GET, response cancellation, generator failure mapping, and the existing
loopback PUT/GET/404 lane pass. S3 assertion retention remains unqualified;
the S3 server's live
`connections` count now follows accepted TCP socket lifetimes and disconnect
cleanup in both Rust and N-API loopback tests. Its tracked TCP boundary also
reports a peer-aware `Connection` transport event on reset-on-close, verified
by the Rust gateway test; the N-API bridge and callback ownership pass the
rebuilt package integration, while direct JavaScript peer-fault injection
remains unqualified. The
WebDAV server-object follow-up now exposes typed buffered
`WebdavSession.handleRequest` and pull-based `handleRequestStream` methods. A
direct N-API probe covers three-chunk PUT, multi-chunk GET, early response
iterator return, and deliberate request-body failure mapping; the stream facade
accepts async iterables and Web ReadableStreams. The `@mount-rs/core/webdav`
subpath also exposes the WebDAV constants/status barrel and protocol/XML/lock
helpers. The session exposes read-only active lock records with expiry cleanup;
the direct method packet covers class 1/2/3 methods plus LOCK/UNLOCK, and the
direct LOCK/UNLOCK check observes one record and then zero. A host-enabled
Node socket reset during a large WebDAV response delivers one typed peer-aware
transport callback, and an isolated malformed request delivers one typed
peer-aware callback. An unsubmitted-token write returns `423` before a
one-second lock expires and is removed. The active lock view now also
preserves the recursive namespaced owner XML tree, including predefined entity
text. Eight parallel unique-file PUTs and
GETs through one direct WebDAV session also pass with byte-for-byte readback;
this is in-process same-driver concurrency evidence only. Complete
session/member parity, direct listener lifecycle, network/native/hosted
concurrency, and the external provider/native/restart gates remain open.
Same-driver server recreation preserves file bytes but resets session locks;
crash/power-loss and provider durability remain unqualified. The pinned pure
WebDAV barrel/protocol differential now passes with
`MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921` at oracle
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`; broader session/server member
differential remains open.

Next W01.1 action: close the next smallest remaining mount-free export or
behavior gap, then rerun the pinned oracle and generated type/build checks
before touching broader native acceptance. Production readiness remains **NO-GO**
until every applicable W01 gate below is green or explicitly accepted outside
the supported scope; local component passes do not override hosted, native,
provider, or cross-platform blockers.

### W01.2 — Upstream skip classification and required coverage

Current estimate: **65%**. The skip inventory is documented and several
capability-limited rows now have exact oracle-backed assertions, but the
inventory is not yet closed as a reviewed, executable boundary for every row.

| Skip/gate family | State | Current evidence | Remaining action |
| --- | --- | --- | --- |
| No-PGlite upstream baseline | In progress | 990 passed, 79 skipped on the pinned oracle suite | Keep the count current while each skip is executed or explicitly covered |
| PGlite rows | In progress | Refreshed PGlite-enabled upstream stage passed 1,200 with 82 skips; the bounded provider/CLI packet also passed Rust SDK 6/6, Node SDK 5/5, and CLI 11/11 while R2 rows remained explicit skips | Keep root-only, live-R2, and hosted rows classified; rerun when those prerequisites exist |
| Unstorage hardlinks | Done as boundary | Four exact `ENOSYS` rows, zero skips/mismatches | Preserve the capability declaration until alias semantics exist |
| Unstorage remaining capability inventory | Done as focused boundary | Fresh 25-row remaining-skip, 14-row next-capability, 4-row hardlink, 13-row aggregate-capability, and 11-row edge packets all matched the pinned oracle with zero skipped rows or mismatches | Reconcile the focused packets with the latest upstream inventory and retain the explicit unsupported capability declarations |
| Symlink, link-timestamp, statfs, and special-node skips | In progress | Capability reasons and focused first-operation assertions exist | Decide per row between implementation and an explicitly reviewed unsupported contract |
| NFS handle skips | Open/external | Classified as a handle/session capability gap | Add/refresh focused NFS handle boundary evidence or implement the missing state semantics |
| Root-only permission/lchown rows | External gate | Current macOS runner is non-root; ordinary ownership tests are not substitutes | Run unchanged tests in a controlled Linux uid-0 lane |
| Native/provider opt-ins | External gate | macOS NFS passed; Linux FUSE, live R2, hosted platform lanes remain | Execute with prerequisites and record failures as failures, not skips |

### W01.3 — Seeded cross-engine traces

Current estimate: **75%**. All currently enabled local backends pass the fixed
seed matrix at the pinned oracle revision.

| Backend lane | State | Evidence / remaining action |
| --- | --- | --- |
| Memory | Done | Five seeds, 621 operations per seed |
| SQLite | Done | Five seeds, 621 operations per seed |
| Object-store | Done locally | Five seeds, 621 operations per seed |
| Chunked memory | Done locally | Five seeds, 621 operations per seed |
| Chunked SQLite | Done locally | Five seeds, 621 operations per seed |
| Chunked object-store | Done locally | Five seeds, 621 operations per seed |
| PGlite and chunked-PGlite | Done locally | Five seeds, 621 operations per seed for each backend; current revision-matched trace refresh passed |
| Live R2 | Blocked by prerequisite | Requires dedicated credentials and bucket-scoped cleanup/verification |

Recorded local result: **40 combinations, 621 operations each**, oracle
revision `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`, oracle SHA
`85361a8212ff9b9ff8e69f62fa8993ef2c2ec51e8`.

### W01.4 — Supported macOS/Linux behavior and lifecycle

Current estimate: **35%**. This is the largest remaining acceptance item because
it combines behavior verification with host and kernel prerequisites.

| Boundary | State | Evidence / remaining action |
| --- | --- | --- |
| macOS NFS structural N-API mount | Done | Actual mount, write/read rewrite, positive NFS `Mounted.port` readback, adoption of an existing NFS server with matching port identity, unmount, `unmountAll`, live-mount cleanup, nested NFS option bag, shared `useDriverIno: false` option propagation, child-process signal teardown, and `Mounted[Symbol.asyncDispose]()` passed |
| Rust CLI macOS NFS | Done | Independent Rust and Node clients passed mounted I/O and persistence |
| Node SDK CLI macOS NFS | Done | Native self-test passed mount/read/write/unmount/persistence |
| Linux FUSE native mount | External gate | Structural job and CI wiring exist; hosted `/dev/fuse` result is still required |
| Linux 9P and native NFS | External gate | The revision-matched hosted Linux 9P job `35616832528` / `native-9p` job `106389895603` passed kernel-module probing and privileged native mount/read/write/unmount on the prior packet, and the macOS native NFSv3 loopback package gate passes; the current 9P shutdown/reaping/concurrent-I/O packet and Linux NFSv4.1 mount/read/write/unmount still need fresh hosted evidence, with native fault/race/crash evidence open |
| macOS FSKit/macFUSE boundary | Open/external | Current implementation does not claim FSKit or macFUSE FUSE-protocol parity |
| Hosted Windows runtime | External gate | Windows-target checks exist; hosted runtime evidence remains required |
| Errors, paths, bytes, links, timestamps | In progress | Strong mount-free and macOS NFS evidence; cross-platform/native coverage remains |
| Handles and lifecycle | In progress | Mount-free inode/handle readback, NFS `Mounted.port`, signal teardown, async disposal, and NFS evidence exist; NFS handle parity and native cleanup races remain |
| Cross-process/crash, cancellation/close, exact append ordering, durability/restart | Open/classified | Package-level NFS signal teardown now passes in a child process; 9P session destruction now wakes and drains direct `Tflush` waiters and an ignored native harness covers server-close/kernel-connection-close/unmount, but process-crash, native reset/half-close races, exact append ordering, and failure durability remain |
| Transport/native concurrency | Open/classified | An opt-in Linux-native 9P harness now exercises eight concurrent mounted file round trips and bounded unmount; hosted execution and broader native Linux/macOS evidence are still required rather than local userspace traces |

### W01.5 — Bounded deterministic concurrency packet

Status: **100% complete for the defined packet**. Six scenarios, five explicit
unsupported classifications, zero oracle mismatches, and five consecutive
pinned-oracle runs passed. The following are deliberately not silently counted
as complete: cross-process/crash behavior, exact append ordering,
cancellation/close races, durability/restart under failure, and native
transport concurrency. Those remain listed under W01.4.

## Time tracking

Add one row after each work session. Use wall-clock engineering time, not time
spent waiting for a hosted job or credential approval.

| Date | Work item | Change/evidence | Actual h | New completion | Notes/blockers |
| --- | --- | --- | ---: | ---: | --- |
| 2026-09-22 | W01-FUSE | Added public Rust `FuseSessionOptions`/`FuseFlushMechanism`, configured inode identity, INIT preferences, cache/timeout policy, error readback, handle counts and destroy-state observability; the complete locked FUSE target (14 unit, 6 INIT, 6 notify/record, 11 protocol, 18 session, 3 sync-barrier tests), strict scoped Clippy, formatting and diff checks passed | — | 66% planning view | Native Linux/FSKit, callbacks, hosted platform, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Added the Rust-backed N-API `FuseSession` and public `./fuse` facade with typed options/defaults, negotiated state, inode views, request/reply/error counters, assertion/error callbacks, notification encoders, destroy-state readback, generated declarations, and raw INIT/LOOKUP/READLINK coverage; locked N-API check/Clippy, debug addon build, focused session/codec/typecheck tests, FUSE tests, formatting and diff checks passed | — | 68% planning view | `MOUNTX_SOURCE`-backed full package suite, native Linux FUSE/callback events, FSKit, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Added Rust-native `FuseMountHooks`/`FuseTransportError` reporting through `mount_with_hooks`, exactly-once terminal callback delivery, callback-panic isolation, and a mount-free Unix-stream protocol-failure harness; locked FUSE tests, formatting and diff checks passed on macOS | — | 70% planning view | Linux-only hook harness and hosted `/dev/fuse` callback delivery, FSKit, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Threaded the FUSE hook through `mount-rs-auto` and the N-API root `mount(..., { onTransportError })`; converted the JavaScript callback to an owned TSFN before `Env::spawn_future`, regenerated declarations, and passed locked auto/N-API checks and Clippy, 16 N-API unit tests, debug build, generated typecheck, default facade, and focused FUSE tests | — | 72% planning view | At this packet's publication, `MOUNTX_SOURCE` codec differentials were an explicit prerequisite; the later focused oracle refresh is recorded below. Hosted Linux callback-event delivery, native mount/lifecycle, FSKit, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Added mount-free session-scoped `GETLK`/`SETLK` range conflict handling, owner replacement/unlock, `RELEASE`/`DESTROY` cleanup, strict 48-byte lock validation, and explicit serialized-session `SETLKW` `EAGAIN`; the focused session target passed 19/19, the complete FUSE target and strict Clippy passed | — | 74% planning view | Native POSIX/flock lock advertisement remains disabled pending a truly blocking/concurrent implementation and hosted Linux lock evidence; native mount/lifecycle, callback events, FSKit, cancellation, concurrency, crash/restart and durability remain open |
| 2026-09-22 | W01-FUSE | Threaded the owned N-API `onTransportError` callback through automatic FUSE/9P/NFS mounts, added NFS hook-preserving native entrypoint, generated declaration coverage and teardown release; scoped auto/NFS/N-API Rust tests, debug addon build, native facade skip lane and typecheck passed | — | 70% planning view | Hosted native fault-event delivery, FSKit, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Hardened the Linux native session against backend and cleanup panics: the request loop now converts an unwind into one owned `Task` transport error, still runs async session teardown, marks the mount inactive/closed, and wakes lifecycle waiters; added a Linux-gated panic regression harness | — | 76% planning view | macOS all-target FUSE tests, strict Clippy, formatting and Linux-target test type-check passed; hosted Linux native fault/crash/restart, callback-event, concurrency, lock and durability evidence, plus FSKit, remain open |
| 2026-09-22 | W01-FUSE | Made native shutdown cancellation-aware while a backend request is in flight: `request_stop()` now cancels the request future, runs session cleanup, and closes lifecycle state; added a Linux-gated blocking-backend regression harness | — | 78% planning view | macOS all-target FUSE tests, strict Clippy, formatting and Linux-target test type-check passed; hosted Linux close races, concurrent request behavior, crash/restart, callback-event, lock and durability evidence, plus FSKit, remain open |
| 2026-09-22 | W01-FUSE | Corrected the N-API mount-free session's default INIT policy to match serialized native dispatch: `ASYNC_DIO`, `PARALLEL_DIROPS`, and `SETXATTR_EXT` are no longer advertised by default, with explicit flag overrides retained and negotiated-flag regression coverage added | — | 79% planning view | Hosted Linux negotiation/callback/lifecycle, FSKit, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Added bounded native positional-read concurrency: up to 16 `FUSE_READ` workers may overlap behind one serialized reply writer, while stateful operations and writes remain serialized; a Linux-gated barrier-driver harness proves two reads overlap | — | 80% planning view | macOS all-target FUSE tests, strict Clippy, formatting and Linux-target test type-check passed; hosted Linux `/dev/fuse`, native write/mutation concurrency, close/crash/restart, callback-event, lock and durability evidence, plus FSKit, remain open |
| 2026-09-22 | W01-FUSE | Refreshed the current pinned FUSE oracle lane: `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/fuse-codec.mjs` passed all listed protocol differential families and `node test/fuse-inodes.mjs` passed inode parity | — | 80% planning view | This closes focused mount-free codec/inode evidence only; the full N-API package matrix, hosted Linux `/dev/fuse`, callback/lifecycle, FSKit, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Hardened the mount-free unsupported-operation boundary: codec-backed `BMAP`, legacy/extended `SETXATTR`, `GETXATTR`, `LISTXATTR`, and `REMOVEXATTR` bodies are validated with negotiated context before valid requests return explicit `ENOSYS`; malformed forms return `EINVAL`; the complete locked FUSE target passed 20 session tests plus the existing unit/INIT/protocol/notify/sync-barrier suites and strict Clippy | — | 80% planning view | This does not advertise or implement native xattrs/BMAP; hosted Linux/native lifecycle, callback events, FSKit, cancellation/concurrency, crash/restart and durability evidence remain open |
| 2026-09-21 | Baseline | Created this ledger from the current W01 tracker and evidence | — | 61% planning view | Hosted/native/live-provider gates remain open |
| 2026-09-21 | W01.1 / W01.4 | Added shared `useDriverIno`, focused native `fuse`/`9p`/`nfs` option bags, configured FUSE `Mounted.source`, package-level signal teardown, `Mounted.port` readback, and `Mounted[Symbol.asyncDispose]()` to the N-API auto facade; 16 N-API unit tests, affected Rust crates, strict Clippy, Linux-target transport check, build/typecheck, authorized macOS NFS lifecycle, and the opt-in child-process signal lane passed | — | 65% planning view | Automatic error/transport callbacks, shared-server handles, remaining option/session members, hosted Linux native lanes, FSKit, PGlite, and live R2 remain open |
| 2026-09-21 | W01.2 / W01.3 | Refreshed the pinned PGlite-enabled upstream suite (4 files, 1,200 passed, 82 skipped), the bounded PGlite provider/CLI packet (Rust SDK 6/6, Node SDK 5/5, CLI 11/11 with R2 skips), and all 40 seeded trace lanes across eight local backends at the pinned oracle revision | — | 65% planning view | Root-only skip rows, live R2, hosted platforms, and native transport acceptance remain open |
| 2026-09-21 | W01.2 | Re-ran the pinned Unstorage boundary packets: remaining-skip (25 rows), next-capability (14), hardlink (4), aggregate capability (13), and edge (11); every packet passed with zero skipped rows or oracle mismatches | — | 65% planning view | The upstream 82 skipped rows remain classified capability/protocol or environment boundaries; root-only and hosted/provider gates remain open |
| 2026-09-21 | W01.1 / W01.4 | Added shared NFS server adoption, already-listened 9P server-handle mapping, transport-specific automatic `onTransportError` hook ownership for FUSE/9P/NFS, and root auto `onTransportError` adaptation to the selected transport; locked FUSE/9P/NFS/auto/N-API tests, strict FUSE/auto Clippy, Linux-target transport check, release build, and generated typecheck passed, with the authorized macOS NFS mount/read/write/port/teardown lane still green | — | 65% planning view | FUSE/root callback event delivery needs hosted Linux/native fault evidence; remaining option/session members, hosted Linux FUSE/9P, FSKit, and other native lifecycle gates remain open |
| 2026-09-21 | W01.1 / W01.2 | Re-ran `MOUNTX_SOURCE=/private/tmp/mountx-source.uWiHfX pnpm test` with host listener permission: the pinned oracle-enabled N-API package harness passed through distribution aggregation, including server callback packets, all FUSE codec/session packets, fresh Unstorage boundaries, differential checks, and restart parity; PGlite/R2/native mount rows remained explicit skips | — | 65% planning view | Live PGlite/R2 credentials, hosted Linux FUSE/9P, FSKit, and root callback runtime event delivery remain open |
| 2026-09-21 | W01.1 / W01.4 | Fixed the automatic NFS shared-server callback lifetime: an ignored mount-level hook is released when an already-created server is adopted; locked NFS/auto/N-API tests (30/5/16), strict FUSE/9P/NFS/auto Clippy, release build, generated typecheck, and the host-enabled pinned package harness passed. The authorized macOS NFS lane passed mounted I/O, matching port, async disposal, cleanup, and exited with status 0 while root and focused callbacks were configured | — | 65% planning view | FUSE/root callback event delivery, hosted Linux FUSE/9P, FSKit, live providers, and the remaining option/session surface remain open |
| 2026-09-21 | W01.1 / W01.4 | Added WebDAV `onTransportError` hooks with a real malformed-HTTP connection test, plus S3 `drainTimeout` and transport-hook ownership through the native server and N-API wrapper; WebDAV/S3 Rust tests, N-API tests, strict S3/WebDAV Clippy, release build, generated typecheck, loopback server integration, and the host-enabled pinned package harness passed | — | 65% planning view | S3 peer-level transport-event delivery, complete S3/WebDAV session/object parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open |
| 2026-09-21 | W01.1 | Added N-API `S3Session` and `WebdavSession` read-only views behind their server objects; S3 bucket names and request/reply/error/operation counters, WebDAV method counters and lock count, assertions, generated declarations, release build, and real loopback PUT/GET/404 integration all passed | — | 65% planning view | S3 assertion retention, S3 connections/peer events, direct session request APIs, complete transport option/member parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open |
| 2026-09-21 | W01.1 / W01.4 | Added N-API `NfsSession` read-only stats/mounts/destroyed views and live `NfsServer.connections`; loopback NULL/MOUNT/GETATTR, live-count/post-close assertions, malformed-record callback and cleanup passed. NFS package targets, N-API library/server integration, release build, generated typecheck, formatting, and focused cross-transport Clippy passed | — | 65% planning view | NFS/9P direct session and connection-object/handle parity, S3/WebDAV direct APIs and peer parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-21 | W01.1 | Closed the NFS subpath export gap: `@mount-rs/core/nfs` now re-exports `NfsServer`, `NfsSession`, and `NfsSessionStats` alongside codecs; the pinned NFS codec differential, root-export immutability check, and generated typecheck passed | — | 65% planning view | The subpath exposes read-only state plus raw-v3 `handleCall`; unified v3/v4 parity and all broader production gates remain open |
| 2026-09-21 | W01.1 / W01.4 | Added direct N-API `NfsSession.handleCall` for raw unframed NFSv3 records; a direct NULL reply and counter update passed independently of socket framing, alongside the rebuilt addon, generated typecheck, NFS differential, NFS/N-API tests, formatting, and Clippy | — | 65% planning view | Unified NFS v3/v4 routing, connection objects/handles, 9P and S3/WebDAV direct APIs, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-21 | W01.1 / W01.4 | Added direct N-API `P9Session.handleCall` for raw 9P frames; a direct Rversion reply on a live connection session passed independently of socket framing. The full 9P Rust integration target, pinned 44-case differential, N-API server/typecheck, release build, and combined 9P/NFS/N-API Clippy passed | — | 65% planning view | NFS v3/v4 unification, 9P stream/attach parity, connection objects/handles, S3/WebDAV direct APIs, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-21 | W01.1 | Added typed buffered N-API `S3Session.handleRequest`; direct PUT with an explicit content-length, real loopback PUT/GET/404 traffic, generated typecheck, N-API tests, S3 gateway 12/12, release build, and S3/N-API Clippy passed | — | 65% planning view | Streaming request bodies, direct WebDAV methods, S3 assertion retention/connections/peer events, complete option/member parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-21 | W01.1 | Added typed buffered N-API `WebdavSession.handleRequest` with normalized headers and materialized response bodies; direct PUT plus loopback PUT/GET, WebDAV integration 12/12, N-API tests/typecheck, release build, and S3/WebDAV/N-API Clippy passed | — | 65% planning view | Streaming bodies, broader direct WebDAV methods, S3 assertion retention/connections/peer events, complete option/member parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-21 | W01.1 | Added the `@mount-rs/core/webdav` constants/status barrel while preserving root server/class identity; the pinned WebDAV constants differential matched literals, limits, status/errno tables and server defaults, the public Rust constants test and WebDAV integration suite passed 13/13, strict WebDAV Clippy passed, and generated typecheck plus package distribution/export checks passed | — | 66% planning view | Protocol/XML/lock helper exports, streaming bodies, S3 assertion retention/connections/peer events, complete option/member parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-21 | W01.1 / W01.4 | Added live S3 TCP connection tracking through the Axum listener, including idle sockets and disconnect cleanup; Rust S3 gateway tests passed 13/13, release N-API build regenerated `S3Server.connections`, the host-enabled N-API server integration passed, generated typecheck/distribution checks passed, and strict S3/N-API Clippy passed | — | 66% planning view | S3 peer-level transport events, streaming direct-session bodies, S3 assertion retention, protocol/XML/lock helper exports, complete option/member parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-21 | W01.1 / W01.4 | Added peer-aware S3 connection transport errors on the tracked TCP boundary: a reset-on-close loopback fault produced one `Connection` event with the accepted peer, and the full Rust gateway target passed 14/14; strict S3/N-API Clippy, release N-API build, host-enabled server integration, generated typecheck, and distribution/export checks also passed | — | 77% W01.1 planning view | Direct JavaScript peer-fault injection remains unqualified; streaming direct-session bodies, S3 assertion retention, protocol/XML/lock helper exports, complete option/member parity, FUSE/root callback events, hosted native lanes, FSKit, and live providers remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Reconciled the current `origin/main` S3 loopback-only hardening with the Rust peer-fault packet; bounded drain timeout, live TCP connection tracking, peer-aware transport hooks, and reset-on-close evidence passed in the locked S3 target (4 unit, 6 chunked, 17 gateway, 5 public-API tests) | — | 70% W01.1 planning view | S3 N-API streaming/member parity, direct JavaScript peer-fault evidence, live AWS/R2, and broader fault/restart/durability/concurrency/native gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added the N-API attached Node Duplex contract, direct session `handleCall`/`destroy`, shared lock-table state, ownership/duplicate-attach/closed semantics, bounded dispatch/backpressure, hostile-write reporting, server-close teardown, generated declarations and explicit native-listener `stream: undefined` scope; focused 9P/auto Rust tests, strict Clippy, host-enabled server phases, typecheck, and the pinned oracle package gate passed | — | 65% W01.1 planning view | PGlite/R2/native-mount opt-ins remain explicit skips; fresh hosted Linux 9P kernel-client lifecycle, native fault/race/crash, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Hosted `native-9p` job `106389895603` in run `35616832528` passed the actual Linux `9p`/`9pnet_fd` probe and privileged native mount/read/write/unmount lifecycle. Added broadcast shutdown with an atomic race guard, active-connection close/accept-loop regression coverage, `Task` transport-failure reporting, and completed request-task reaping; local lifecycle 5/5, transport-error 8/8 and strict 9P Clippy passed | — | 65% W01.1 planning view | The hosted result predates this shutdown/reaping packet and must be rerun at its current commit; native reset/half-close/concurrency/crash and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added an ignored Linux-native harness with eight concurrent mounted file write/read/rename/read workers followed by bounded unmount; focused 9P tests and strict 9P Clippy passed locally | — | 65% W01.1 planning view | A hosted run on the harness revision is required; native reset/half-close/crash and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Session destruction now marks and wakes in-flight requests, drains the pending map, and releases a concurrent `Tflush` waiter; the new regression and focused 9P/strict-Clippy gates pass locally | — | 65% W01.1 planning view | Native cancellation/close, reset, crash, and current hosted execution remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added an ignored Linux-native server-close lifecycle harness that verifies mounted I/O, server-side connection closure, kernel connection closure, and bounded unmount with cleanup refusal on failure; focused 9P tests and strict Clippy passed locally | — | 65% W01.1 planning view | Hosted execution, process-crash, reset/half-close, and broader W01 gates remain open; W01 stays NO-GO |

| 2026-09-22 | W01-FUSE | Added fail-closed validation for caller-supplied native FUSE mount option tokens and transport-owned overrides; focused `mount-rs-fuse` all-target tests and strict Clippy passed on macOS | — | 35% W01.4 planning view | Hosted Linux `/dev/fuse`, callback-event, crash/concurrency/durability, and signed/activated FSKit evidence remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added active NFS socket-task accounting with abort-safe close draining and read-only sorted BigInt shared-handle snapshots on both the v3 and v4 N-API views. Rust NFS tests passed 31 unit, rootless wire 1, transport errors 4, v4 barrier 1, and v4 wire 2; the release addon, generated typecheck, and live N-API server integration passed | — | 72% W01.1 planning view | Full v3/v4 stateful matrix, remaining upstream session/member parity, hosted lifecycle, Linux NFSv4.1 and crash/durability gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added live NFS connection objects with stable id/peer/shared-session views and abort-safe `close`/`waitClosed`; the `./nfs` subpath identity check, release addon, generated typecheck, distribution check, and host-enabled N-API server integration passed | — | 75% W01.1 planning view | Full v3/v4 stateful matrix, hosted lifecycle, Linux NFSv4.1, crash/durability, and any remaining upstream member differences remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added rootless NFSv4.1 session continuity across an orderly TCP reconnect and a rootless eight-request NFSv3 pipelining test with bounded in-flight dispatch; the complete locked NFS target passed 31 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 3, with strict affected Clippy and formatting green | — | 75% W01.1 planning view | Process-lifetime userspace evidence does not qualify automatic reconnect, crash/restart or durable recovery, native Linux NFSv4.1, hosted lifecycle, or the full stateful/member matrix; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added a rootless restart-boundary classification: after `NfsServer::close()`, a replacement server sharing the backend rejects the old NFSv4.1 session with `NFS4ERR_BADSESSION`; the v4 wire target passed 4/4 | — | 75% W01.4 planning view | This explicitly classifies v4 session/lease/replay state as process-local, but backend durability, crash injection, native Linux NFSv4.1, hosted lifecycle, and full upstream state/member parity remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | The refreshed opt-in macOS native NFSv3 loopback mount gate passed 1/1 in 0.11s on the exact pushed tip, including filesystem round trips and bounded cleanup | — | 75% W01.4 planning view | Linux NFSv4.1, Linux 9P, hosted lifecycle, full stateful parity, crash/concurrency/durability, and live-provider gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | The v4 channel/state follow-up aligned `CREATE_SESSION` error and counter-offer boundaries (`NFS4ERR_TOOSMALL`, `NFS4ERR_NOSPC`, and preserved back-channel counts), while the rootless v4 wire suite remained 5/5 and scoped Clippy stayed green | — | 75% W01.1 planning view | ID-map/clock/seed and session `onError` parity, Linux NFSv4.1, hosted lifecycle, full stateful parity, crash/concurrency/durability, and live-provider gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Exposed the Rust S3 streaming request/response boundary through the N-API session: async-iterable/`ReadableStream` request bodies, incremental response iteration, cancellation, generator-failure mapping, `S3Server.session`, and async metrics snapshots; the generated release package build, package typecheck, direct release binding load, and host-enabled `node test/servers.mjs` passed, while N-API Rust check/Clippy, formatting, the strict TypeScript fixture check, and the locked S3 Rust target remained green | — | 72% W01.1 planning view | Direct JavaScript peer-fault evidence, complete S3 member parity, live AWS/R2, and broader fault/restart/durability/concurrency/native gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added pull-based N-API WebDAV request/response bodies with async-iterable and Web ReadableStream input, positional response chunks, cancellation cleanup, and request-body failure mapping. The isolated N-API check, release addon/declaration build, generated typecheck, and direct stream probe passed three-chunk PUT, multi-chunk GET, early iterator return, and deliberate body failure | — | 72% W01.1 planning view | `MOUNTX_SOURCE` oracle differential, loopback N-API listener on this sandbox, complete session/member parity, peer-fault injection, provider/native/hosted lifecycle, restart, concurrency, and durability remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added `WebdavSession.locks` with generated `WebdavLockView` records; the host-enabled `node test/servers.mjs` direct N-API LOCK/UNLOCK check observed token/path/depth/exclusive/timeout state and post-UNLOCK zero records, while `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-lock-test-target ./scripts/cargo-shared test -p mount-rs-webdav --locked` passed 13/13 with the native mount probe explicitly ignored, the affected N-API check, release build, generated typecheck, formatting, and warning-denied Clippy passed | — | 72% W01.1 planning view | `MOUNTX_SOURCE` oracle differential, loopback N-API listener on this sandbox, complete session/member parity, peer-fault injection, provider/native/hosted lifecycle, restart, concurrency, and durability remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The host-enabled `node test/servers.mjs` now covers direct `OPTIONS`/`MKCOL`/`PUT`/`HEAD`/`GET`, `PROPFIND`/`PROPPATCH`, `COPY`/`MOVE`, `LOCK`/`UNLOCK`, `DELETE`, and `PATCH` refusal through `WebdavSession.handleRequest`; the complete applicable class 1/2/3 method slice passed | — | 72% W01.1 planning view | `MOUNTX_SOURCE` oracle differential, loopback N-API listener on this sandbox, complete session/member parity, peer-fault injection, provider/native/hosted lifecycle, restart, concurrency, and durability remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The host-enabled `node test/servers.mjs` WebDAV phase drove a large-response Node socket reset after reply readiness and observed exactly one typed peer-aware callback plus cleanup; local peer-fault evidence passed, while malformed-connection, complete session/member parity, provider/native/hosted lifecycle, restart, concurrency, and durability remain open | — | 72% W01.1 planning view | `MOUNTX_SOURCE` oracle differential and the remaining lock conflict/expiry, provider/native/hosted lifecycle, restart, concurrency, and durability gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The host-enabled `node test/servers.mjs` WebDAV phase observed `423 Locked` for an unsubmitted-token write and expiry cleanup for a `Second-1` lock; local lock conflict/expiry evidence passed, while malformed-connection, provider/native/hosted lifecycle, restart, concurrency, and durability remain open | — | 72% W01.1 planning view | `MOUNTX_SOURCE` oracle differential, complete session/member parity, provider/native/hosted lifecycle, restart, concurrency, and durability gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The host-enabled `node test/servers.mjs` WebDAV phase isolated malformed HTTP on a second server and observed one typed peer-aware callback with clean socket/server teardown; local malformed-connection evidence passed while provider/native/hosted lifecycle, restart, concurrency, and durability remain open | — | 72% W01.1 planning view | `MOUNTX_SOURCE` oracle differential, complete session/member parity, provider/native/hosted lifecycle, restart, concurrency, and durability gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The host-enabled `node test/servers.mjs` WebDAV phase recreated a server over the same driver, preserved a seeded file, and reset the session lock table; this classifies local same-process recreation only, not crash/power-loss or provider durability | — | 72% W01.1 planning view | `MOUNTX_SOURCE` oracle differential, complete session/member parity, provider/native/hosted lifecycle, crash/restart, concurrency, and durability gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The host-enabled `node test/servers.mjs` WebDAV phase created `/concurrent`, completed eight parallel unique-file PUTs and eight parallel GETs through one `WebdavSession.handleRequest`, and verified every body byte-for-byte | — | 72% W01.1 planning view | This is in-process same-driver evidence only; network-client, native, hosted, crash/power-loss restart, provider durability, complete session/member parity, and oracle differential remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/webdav-codec.mjs` passed the complete pure WebDAV constants/path/header/XML/lock/document differential at oracle `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`; source-backed host-enabled `node test/servers.mjs` also passed | — | 72% W01.1 planning view | Full session/server member differential, native/hosted lifecycle, provider qualification, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added recursive N-API `WebdavXmlNode` owner readback to `WebdavLockView`; host-enabled local and pinned structural-driver `node test/servers.mjs` runs accepted namespaced `A&amp;B` owner text, while the Rust 13-test target and strict WebDAV/N-API Clippy passed after bounded predefined/numeric XML reference decoding | — | 72% W01.1 planning view | Complete session/member parity beyond lock-owner readback, native/hosted lifecycle, provider qualification, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |

## Definition of W01 complete

W01 can move to complete only when each of these is true:

1. W01.1’s applicable exports and behaviors are either implemented and
   oracle-verified or explicitly removed from the supported scope.
2. W01.2’s skipped rows are executed or have a reviewed, focused capability or
   protocol boundary; a raw skip count alone is not completion.
3. W01.3 has revision-matched seeded results for every backend still claimed as
   supported, including separately gated PGlite and live R2 where claimed.
4. W01.4 has current macOS/Linux native evidence for every claimed transport,
   plus explicit hosted/unsupported results for the others.
5. W01.5 remains green, and its classified follow-up races have either been
   verified or moved into an explicitly unsupported scope.

Until then, the percentage is a progress aid only and the W01 status remains
**Open**.

## Production-readiness decision

Current decision: **NO-GO**. W01 is being tracked toward production release,
not merely toward local test completion. A production-ready decision requires
all five W01 work items to meet their closure evidence, the applicable public
API ledger to be implemented or explicitly scoped out, and the release/build,
generated API, focused integration, platform/native, hosted, provider, and
durability/concurrency gates to be green for every claimed deployment target.

The following evidence is useful but does not by itself authorize release:
local unit tests, loopback transport tests, generated typechecks, release
builds, component Clippy, an allocated workstream being complete in another
task, or a documented unsupported boundary. Each remaining blocker must be
resolved, or explicitly accepted as outside the supported production scope,
before the decision can change to **GO**.
