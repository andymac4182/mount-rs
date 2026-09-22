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
| W01-FUSE | FUSE protocol, mount-free session, native mount, callbacks and lifecycle | Focused Rust/N-API protocol/session evidence exists; macOS FUSE/FSKit is explicitly outside supported scope on the actual Darwin host, while native Linux, callback events, remaining session parity and lifecycle races remain | Hosted Linux native mount/read/write/unmount, callback-event and lifecycle evidence, plus the remaining supported-scope/lifecycle gates |
| W01-NFS | NFSv3/v4 router, sessions, handles, native mount and lifecycle | v3/v4 direct routing, shared server state, BigInt handle snapshots, active connection objects/count, close/wait lifecycle, v4 view, process-lifetime session continuity across an orderly TCP reconnect, rootless pipelined v3 dispatch, rootless two-client v4.1 concurrent distinct-file round trips, pinned 266-case upstream conformance, codec differential, bounded `maxHandles`/NFSv4 pinning, bounded v4 channel/state knobs, seeded v4 identities, dynamic Rust/N-API owner callbacks, injected N-API clock calls, `maxLocksPerFile` enforcement, refused-`CREATE_SESSION` replay/retry, request-level error callbacks, hosted native NFSv3/NFSv4.1 platform jobs, host-backed NFSv3 process-crash recovery with explicit stale-handle rejection, and forced-crash NFSv4.1 fencing of the old session with `NFS4ERR_BADSESSION` and root handle with `NFS4ERR_STALE` are evidenced; multiple server processes sharing one backend are explicitly outside supported scope; full stateful matrix and durable v4 recovery remain | Shared v3/v4 handle/state scope, native-client ordering, NFSv4 lease/replay/file-handle durability, power-loss durability, and production acceptance; hosted run `35658285441` passed both named NFS jobs while unrelated workflow jobs failed |
| W01-9P | 9P protocol, session, connection, attach and mount lifecycle | Rust/N-API attached Node Duplex with typed per-attachment peer/ownership/frame/in-flight bounds and source-specific peer values (`undefined` for an attached stream with no fallback, transport-source strings for native Unix/TCP listeners, and `null` only when N-API has no peer), direct session, scalar server/session options, `userFor`, ownership, duplicate-attach, bounded backpressure, write-fault, server-teardown, broadcast-shutdown, active-connection close-race, shutdown-aware permit waits, bounded task-reaping, session-destroy/Tflush-wakeup evidence, a live property-shaped `P9Server.clients` array covering native and attached connections, injected shared `P9ServerOptions.locks` coverage plus native TCP shared-lock conflict/holder/release coverage, direct mount-created scalar server-policy and session-callback mapping, serializable option-snapshot and attached-stream member-boundary checks, direct `./9p` signal teardown, all 124 pinned constants and all 274 upstream runtime `./9p` barrel exports including six public defaults, native N-API fid views with array-shaped cursor offsets plus writable `iounit`/`cursor`, `P9DirentPacker.maxSize` declaration/runtime parity, the synchronous/asynchronous `framesFrom` helper with caller-owned assembler support, and the bounded direct `./9p` probe/refusal/option/`mount9p` facade with configured shared-server adoption plus mounted transport/server/connection/closed views; the dedicated [Native 9P run `35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344) at `431affd` passed kernel probing plus all four ignored Linux lifecycle tests; exact SHA `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` also passed [Native 9P run `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703), N-API job `106558367429` with automatic, direct, and structural-driver mounted I/O/cleanup, including `P9Mount.source` runtime shape, and Rust job `106558367006` passed all four ignored native tests; corrected exact SHA `81cc6596c2c9562c3405df50126239a7bcb44f63` also passed [Native 9P run `35670279904`](https://github.com/andymac4182/mount-rs/actions/runs/35670279904), N-API job `106565351978` with automatic, direct, and structural-driver mounted I/O/cleanup, including native `stream: undefined` and transport-source peer, and Rust job `106565352174` passed all four ignored native tests; exact SHA `ba20d29d7e8ad00b3c4b5270dc21cf6ab913e4c2` also passed [Native 9P run `35674581481`](https://github.com/andymac4182/mount-rs/actions/runs/35674581481), N-API job `106578252549` with the Linux probe and automatic/direct/structural mounted-I/O/cleanup checks, and Rust job `106578252700` with the Linux probe plus all four ignored native lifecycle tests; exact SHA `b3757fd288e6f52888873838946343e7cd37f953` passed [Native 9P run `35675876913`](https://github.com/andymac4182/mount-rs/actions/runs/35675876913), N-API job `106582464900` and Rust job `106582465059`, with the N-API job passing automatic/direct/structural mounted-I/O/cleanup and the Rust job passing all four ignored native lifecycle tests; exact SHA `412c422e2485a5c7ce2caf55892ec6475faab8d8` passed [Native 9P run `35676832586`](https://github.com/andymac4182/mount-rs/actions/runs/35676832586), N-API job `106585007802` and Rust job `106585007667`, with both passing their Linux probes and the N-API mounted-I/O/cleanup plus Rust native lifecycle gates; native Tokio listener connections intentionally expose no Node stream and use the supported `attach` seam | Automatic cross-transport signal ownership remains an explicit scope boundary; the pinned direct `MountP9Options` audit found no additional unrepresented fields; crash/reset/half-close recovery is explicitly supervisor-owned rather than a library claim, broader upstream member parity and other W01 gates remain open, and the overall W01/release decision remains NO-GO |
| W01-S3 | S3 protocol, session, streaming, providers and lifecycle | Local protocol/structural-driver evidence, oracle-backed source and bucket-name construction parity, streamed N-API bodies, effective session options, Rust `S3SessionHooks` plus N-API `now`/`requestId`/`onError`/`onAssertion` with close-safe callback ownership, bucket wrappers, debug-gated assertions, live connections, direct peer-fault callback delivery, bounded unique-object direct-session concurrency, same-key conditional PUT CAS ordering, multipart session replacement through Rust and N-API, close sweep, Complete/Abort terminal-race handling, failed-Complete retry release, injected assembly-read fault recovery, native-filesystem process-restart handoff, and the exact Node `./s3` supported-scope audit pass; live provider and native/hosted lifecycle remain | Applicable API ledger with the Node codec/helper scope explicit, live AWS/R2, crash/power-loss durability, broader ordering/concurrency, and native/hosted evidence |
| W01-WebDAV | WebDAV protocol, locks, session, streaming and lifecycle | Local protocol/session evidence exists; the full supported N-API server/session prototype-member differential and full supported class 1/2/3 direct-method differential now pass alongside Rust listener lifecycle serialization, immediate-close wakeup, bounded close-timeout/retry/relisten and forced-cancellation behavior, N-API wrapper lifecycle and failed-close retry regressions, oracle-compatible in-place partial-body PUT semantics, durable-driver `syncfs` barriers with failure/retry coverage including the structural-driver callback success/error seam, focused 256-request direct-session and 256-pair live HTTP network concurrency, bounded 128-pair NodeFs/SQLite provider-backed direct-session and 64-pair loopback network concurrency, chunked streamed PUT/GET, Basic-auth challenge/acceptance, exact-once live request-error callback, and concurrent lock enforcement, plus the opt-in host-enabled WebDAV network/fault/restart phase pass; the N-API `now`, `onAssertion`, and live-lock-table controls are explicitly outside supported scope with deterministic Rust clock evidence; hosted 4-OS N-API package tests, hosted macOS/Linux native I/O with eight-way mounted concurrency and close-while-mounted remount restart, and local NodeFs/SQLite orderly-reopen and process-crash recovery—including in-flight streamed-PUT prefix recovery—with process-local-lock classification now pass for the recorded packet; independent-resource concurrency is supported, while stronger same-resource ordering beyond WebDAV lock/`If` coordination is explicitly outside scope; live provider, power-loss, and durable-lock gates remain | Applicable API ledger, auth/lock durability and restart evidence; atomic PUT publication and stronger same-resource ordering are outside the supported oracle-compatible contract, while real provider/power-loss durability, durable locks, and live-provider behavior remain open |
| W01-Auto/CLI | Auto selection, mount facade and SDK-backed consumers | Focused option/consumer paths exist; cross-transport native lifecycle remains | Per-transport options/callback ownership and signal/async-dispose evidence |
| W01-Provider/Native | Providers, hosted CI, FSKit, Windows, crash and concurrency | Local capability-limited packets exist; external lanes remain | Fresh live-provider and hosted/native results with no prerequisite-gated acceptance rows |

The latest W01-9P TCP isolation packet was published at exact SHA
`9870d58cfbed5bcea90972c4b9caaf5db3075cef` and passed [Native 9P run
`35685807744`](https://github.com/andymac4182/mount-rs/actions/runs/35685807744):
N-API job `106612633937` passed the real TCP per-connection session/fid
isolation, survivor service after one client closes, completion-order dispatch,
the full server/attach and teardown phases, and automatic/direct/structural
mounted-I/O cleanup. Rust job `106612633771` passed the Linux probe plus all
four ignored native lifecycle tests. Local syntax/diff checks and elevated
N-API execution passed; broader W01 acceptance remains NO-GO.

The latest W01-9P shared-lock packet was published at exact SHA
`514d2c533382b927c059ccd946f3e566d2c371a9` and passed [Native 9P run
`35686403815`](https://github.com/andymac4182/mount-rs/actions/runs/35686403815):
N-API job `106614048924` passed the configured shared-lock conflict,
`Tgetlock` holder, connection-close release, full server/attach and teardown
phases, and automatic/direct/structural mounted-I/O cleanup. Rust job
`106614048722` passed the Linux probe plus all four ignored native lifecycle
tests. Local elevated N-API execution passed; remote admission and the
remaining framing, payload, port, crash/reset, parity, and W01 gates remain
open, so production remains NO-GO.

The latest W01-9P listener-boundary packet was published at exact SHA
`2a3ccfa9a77cab22d154d041627369d995ba74d5` and passed [Native 9P run
`35687145769`](https://github.com/andymac4182/mount-rs/actions/runs/35687145769):
N-API job `106616293297` passed the occupied-port `EADDRINUSE` case, malformed-
frame survivor case, full server/attach, shared-lock, and teardown phases, and
automatic/direct/structural mounted-I/O cleanup. Rust job `106616293187` passed
the Linux probe plus all four ignored native lifecycle tests. Local elevated
N-API execution passed; remote admission, large payload/negotiated `msize`,
crash/reset, broader parity, and W01 gates remain open, so production remains
NO-GO.

The latest W01-9P wire-framing packet was published at exact SHA
`c42030c1807f6504660892bf829137897e910c5e` and passed [Native 9P run
`35687955065`](https://github.com/andymac4182/mount-rs/actions/runs/35687955065):
N-API job `106618714142` passed the deterministic 256 KiB multi-frame
write/read, negotiated-`msize` oversized-frame rejection and survivor, full
server/attach, shared-lock, and teardown phases, and automatic/direct/
structural mounted-I/O cleanup. Rust job `106618713939` passed the Linux probe
plus all four ignored native lifecycle tests. Local elevated N-API execution
passed; remote admission, crash/reset, broader parity, and W01 gates remain
open, so production remains NO-GO.

The latest W01-9P remote-admission packet was published at exact SHA
`87ccd68c8e2040c90037c6027eb4467b1a7bd42d` and passed [Native 9P run
`35688474092`](https://github.com/andymac4182/mount-rs/actions/runs/35688474092):
N-API job `106620236951` passed the interface-qualified default
loopback-only refusal, wire-framing, full server/attach, shared-lock, and
teardown phases, and automatic/direct/structural mounted-I/O cleanup. Rust job
`106620236776` passed the Linux probe plus all four ignored native lifecycle
tests. Local elevated N-API execution passed against the actual external IPv4
interface; explicit `allowRemote: true` network admission, crash/reset,
broader parity, and W01 gates remain open, so production remains NO-GO.

The preceding W01-9P transport-teardown packet was published at exact SHA
`1179d9e3fbdb95ea1cca9866fd249c949614a9e1` and passed [Native 9P run
`35685073733`](https://github.com/andymac4182/mount-rs/actions/runs/35685073733):
N-API job `106610049913` passed addon build, the isolated TCP/Unix/attached
server lifecycle, server close with an open fid, paused-peer FIN teardown,
silent TCP reset, orderly client EOF, and automatic/direct/structural mounted
I/O cleanup. Rust job `106610049705` passed the Linux probe plus all four
ignored native lifecycle tests. Local focused Rust tests, strict Clippy,
addon rebuild, syntax/diff checks, and elevated N-API execution passed. The
process-crash and arbitrary-kernel-reset recovery boundary remains explicit;
broader W01 acceptance remains NO-GO.

The preceding W01-9P Unix listener packet was published at exact test SHA
`dd10ac0564446c9143f8b5f68b2fed51c7eaf57f` and is included in descendant head
`d43f5ea4e4334912de86ac0db818392531a7d4ec`. Its [Native 9P run
`35683716217`](https://github.com/andymac4182/mount-rs/actions/runs/35683716217)
passed N-API job `106606580352` with private/shared-directory policy,
`0600` Unix socket mode, handshake/peer/path, cleanup, and path/port
exclusivity, followed by the existing server/attach and automatic/direct/
structural mounted-I/O cleanup steps. Rust job `106606580326` passed the Linux
probe plus all four ignored native lifecycle tests. Local elevated execution
also passed; the direct run at the test SHA was cancelled before jobs
materialized and is not promoted. This qualifies the Unix listener policy
slice; broader W01 acceptance remains NO-GO.

The preceding W01-9P packet at exact SHA
`007e6545d1b25d708abfa10f2120f81fba59a74a` passed [Native 9P run
`35682638941`](https://github.com/andymac4182/mount-rs/actions/runs/35682638941):
N-API job `106602684115` passed the isolated server/attached-stream lifecycle
step plus automatic/direct/structural mounted I/O and cleanup, and Rust job
`106602683880` passed the Linux probe plus all four ignored native lifecycle
tests. The selector independently covers the real TCP server and attached
socket/duplex paths, duplicate attach, backpressure, frame-limit rejection,
write-fault teardown, and server close; the local elevated phase also passed,
while the unprivileged Darwin run reached 9P but hit only the sandbox listener
relisten `Operation not permitted` boundary. This qualifies the covered
server/attach lifecycle slice; broader W01 acceptance remains NO-GO.

The preceding W01-9P P9User packet at exact SHA
`1c43f66ec570be35444055ab6adb0f841628fef6` passed [Native 9P run
`35681672318`](https://github.com/andymac4182/mount-rs/actions/runs/35681672318):
N-API job `106599754171` and Rust job `106599753872` both passed their Linux
probes and supported lifecycle gates. The packet preserves an own
`P9User.uid` property with `undefined` for a missing numeric uid and declares
`uid: number | undefined`; the broad local `servers.mjs` script stopped at the
unrelated Darwin NFS relisten sandbox boundary before reaching 9P. Broader W01
acceptance remains NO-GO.

The preceding W01-9P direct-type packet at exact SHA
`2bcd9aa4b0d25f284d8ae9fc4ad3de0a5cbbfeff` passed [Native 9P run
`35681127657`](https://github.com/andymac4182/mount-rs/actions/runs/35681127657):
N-API job `106598109004` and Rust job `106598109187` both passed their Linux
probes and supported lifecycle gates. The packet exports the oracle-shaped
direct `P9Platform` type alias without changing runtime behavior; broader W01
acceptance remains NO-GO.

The preceding W01-9P direct-probe packet at exact SHA
`7389be4d5ea4930075cf5278032614e931054620` passed [Native 9P run
`35680542975`](https://github.com/andymac4182/mount-rs/actions/runs/35680542975):
N-API job `106596362070` and Rust job `106596362200` both passed their Linux
probes and supported lifecycle gates. The packet restores the oracle-shaped
own `platform`/`reason` fields on the direct `P9ClientProbe` result while
leaving the root automatic-probe boundary unchanged; broader W01 acceptance
remains NO-GO.

The preceding W01-9P direct-facade packet at exact SHA
`56291e3f9b4274fec2111e4e2f88696e98f3a548` passed [Native 9P run
`35679754417`](https://github.com/andymac4182/mount-rs/actions/runs/35679754417):
N-API job `106593941892` and Rust job `106593942012` both passed their Linux
probes and supported lifecycle gates. The packet restores the synchronous
direct `live9pMounts()` contract with inactive/closed pruning while leaving
the root all-transport `liveMounts()` registry asynchronous; broader W01
acceptance remains NO-GO.

The preceding W01-9P typed-reader packet at exact SHA
`4ecdb63db64711e0fadf77d4612b6394f57f3f4d` passed [Native 9P run
`35678757675`](https://github.com/andymac4182/mount-rs/actions/runs/35678757675):
N-API job `106590841909` and Rust job `106590841982` both passed their Linux
probes and supported lifecycle gates. The native typed-reader maximums are
recorded in the detailed W01-9P ledger; broader W01 acceptance remains NO-GO.

WebDAV's streamed `PUT` boundary is deliberately oracle-compatible rather
than an atomic-publication promise: a body failure returns an error and leaves
the prefix already written at the destination. The focused Rust regression and
host-enabled N-API WebDAV phase now assert this exact behavior; power-loss and
live-provider durability remain separate open gates.

WebDAV mutation acknowledgments now await `FsDriver::syncfs()` whenever the
driver advertises durable writes, and a focused Rust test covers the complete
successful mutation set plus barrier failure/retry. This is transport-level
barrier evidence, not proof of a particular provider's power-loss ordering or
durable lock policy.

The structural N-API `FsDriver` adapter now forwards an optional `syncfs()`
callback, and the WebDAV regression drives successful, failing, and missing-
callback durable barriers through `createWebdavServer`. Durable structural
drivers without that callback still fail closed with `ENOSYS`; this local seam
evidence does not qualify live providers, power-loss ordering, or hosted
acceptance.

The focused N-API in-flight streamed-`PUT` crash probe also yields and
independently reads back the prefix before forcing the child process down.
Replacement NodeFs and SQLite providers recover that exact prefix with empty
replacement-session lock tables in three repeated runs. This is local
process-crash evidence only; it does not close power-loss, live-provider,
durable-lock, hosted lifecycle, or hosted concurrency gates.

The provider-backed direct-session matrix also passes three repetitions at 128
concurrent NodeFs and SQLite PUT/GET pairs with exact byte readback. This is
local provider evidence only; hosted remote-provider, network, power-loss,
durable-lock, and wider ordering/concurrency gates remain open.

The host-enabled provider-backed network matrix also passes at 64 concurrent
NodeFs and SQLite HTTP PUT/GET pairs in three repetitions, including streamed
PUT/GET bodies. This is local loopback provider evidence only; hosted
remote-provider, hosted network, power-loss, durable-lock, and wider ordering
gates remain open.

For published provider-network packet `41bd16f08a4ba065046f5da4bd54d90d2a16f028`,
the exact-SHA GitHub Actions API returned no associated workflow runs at the
snapshot. No hosted WebDAV PASS is claimable from that packet.

At exact current tip `e5ae05d07bbc73184952def0437e58be9efef790`, the locked
Rust WebDAV target passes 20 tests with the privileged native mount probe
explicitly ignored, warning-denied `mount-rs-napi` Clippy passes, and the
host-enabled release N-API build, generated typecheck, structural durable-
driver success/500-error/501-missing-callback regression, lifecycle, 64-pair
direct-session concurrency, and 64-pair live HTTP network/auth/streaming
concurrency checks pass. Formatting and diff checks also pass. The same
exact-tip manual CI run `35674823787` subsequently completed its macOS and
Ubuntu native-WebDAV jobs successfully, closing that hosted native-I/O slice
only; the overall run remains in progress, and hosted/provider lifecycle,
hosted concurrency, power-loss/live-provider durability, durable-lock, and
wider ordering gates remain open.

At current checkout `d761deb23513ec78b61d7b627f67b46606ee4956`, the refreshed
NodeFs/SQLite orderly-reopen and crash probes, 128-pair provider direct-session
matrix, 64-pair provider loopback network matrix, in-flight streamed-`PUT`
prefix recovery, pinned WebDAV barrel/session-member differentials, and the
40-case TypeScript/Rust S3+WebDAV HTTP differential all pass. This remains
local provider/crash, pinned-oracle, and loopback evidence only; hosted
session/lifecycle/concurrency, live remote-provider behavior, power-loss
ordering, durable locks, and wider ordering remain open.

The exact-tip hosted CI run `35674823787` passed all four N-API `node` matrix
jobs (Ubuntu x64/arm64 and macOS arm64/Intel), all three Rust matrix jobs, and
both native WebDAV jobs. The N-API job runs the full package script with the
pinned oracle, so this is hosted cross-platform package evidence for WebDAV
session, streaming, lifecycle, concurrency, provider, crash, and parity
checks. The workflow remains nonterminal on an unrelated native-FUSE job; no
hosted mounted-host concurrency, live-provider, power-loss, or durable-lock
acceptance is inferred.

The ignored native WebDAV harness now performs eight concurrent native-client
write/read pairs after its basic round trip and verifies every payload through
the driver, then closes the server while mounted, unmounts, relistens, remounts
the same driver, and verifies a post-restart round trip. The current
host-enabled macOS run passed 1/1 with the shared Cargo wrapper, and corrected
hosted CI `35678488755` passed both macOS and Linux native-WebDAV jobs for the
same flow; the aggregate workflow remained nonterminal on unrelated jobs.

The pinned WebDAV oracle deliberately has no `PathLock` for this HTTP session.
The fresh concurrent lock regression passes with two simultaneous writes
without the submitted token both returning `423`; the refreshed 256-pair
direct-session and loopback HTTP matrices pass for independent resources.
The full locked WebDAV target now passes 21/21 and warning-denied WebDAV
Clippy passes.
WebDAV therefore supports concurrent independent-resource work and lock/`If`
coordination, but does not claim linearizable same-resource ordering or atomic
same-target `PUT` publication. Power-loss durability, live-provider behavior,
and durable locks remain open gates.

The current shell has no AWS/R2/Cloudflare credential names available, so live
provider acceptance remains an explicit external blocker; no credential values
were read or persisted.

The current hosted provider audit confirms that boundary: Live AWS S3 run
`35679010203` stopped at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket` with its
protected bucket/region/account/role inputs empty, while Live Cloudflare R2 run
`35680542993` stopped at the bounded-usage gate (`count=285`, limit `20`). The
exact-tip CI run for the published WebDAV scope chunk was cancelled by a
successor mainline push; replacement CI `35680709436` at current origin tip
`1bdf8846` had no jobs at the audit snapshot, so no fresh hosted WebDAV result
is promoted.

The next exact-tip CI run `35681063238` at published WebDAV tip `7282bce8`
started, but native-WebDAV jobs `106597988170` (macOS) and `106597988268`
(Ubuntu) were cancelled by successor tip `2bcd9aa4`; replacement CI
`35681127696` was pending at the audit snapshot. The earlier terminal hosted
native-WebDAV run remains the latest claimable hosted WebDAV result.

For published provider packet `fb9caec81a7e3fa183f5fa51871117e62fa35036`, the
exact-SHA CI/Fault injection/W08 workflows were queued or pending, W04 policy
succeeded, Live Cloudflare R2 failed, and an unrelated Native 9P workflow was
still in progress. No hosted WebDAV PASS is claimable from that packet.

For published packet `4719eb50a6e2d48e4539c10f0638fa2f493301c1`, exact-SHA CI,
Fault injection, and both W08 release workflows were cancelled; W04 production
policy succeeded, while Live Cloudflare R2 and an unrelated Native 9P workflow
were still in progress. No hosted WebDAV PASS is claimable from that packet.

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
The ignored Linux FUSE harness now starts eight concurrent blocking kernel
clients; each writes, reads, renames, and rereads a distinct file, then the
harness checks that all eight entries are visible through the mounted root.
The host harness compiles and Linux-target strict Clippy passes, but only the
hosted `native-fuse` execution can qualify this as native runtime evidence;
W01 remains NO-GO until that result and the other lifecycle gates are green.
The same ignored Linux harness now includes a driver that panics only when a
mounted file is read. It requires the kernel read to fail, waits for the
session to close, asserts exactly one owned `Task` transport callback, and
completes bounded unmount and mountpoint cleanup. Local focused tests and
Linux-target strict Clippy pass; hosted execution is still required for native
callback-event and panic/cleanup acceptance, so W01 remains NO-GO.
The automatic named-FUSE harness now routes the same read-only backend panic
through `AutoMountHooks.fuse`; it asserts one owned `Task` callback at the
facade boundary, observes `active == false`, and completes bounded unmount and
cleanup. Locked host compilation and Linux-target strict Clippy pass; hosted
execution is still required for root automatic callback-event acceptance, so
W01 remains NO-GO.
The Linux request pump now aborts and drains registered positional-read
workers before dispatching `FUSE_DESTROY`, so a pending backend read cannot
block session cleanup. A Linux-gated Unix-stream regression proves the destroy
reply and bounded close while a read is blocked. Host all-target tests, host and
Linux-target strict Clippy, formatting, and diff checks pass; hosted kernel
unmount/close-race and crash/restart execution remain external, so W01 remains
NO-GO.
The ignored Linux FUSE harness now adds the corresponding kernel close-race
case: a backend read is held pending until the request is observed, then the
test calls bounded unmount and requires the blocked filesystem read to finish
with an error before removing the mountpoint. The harness compiles on the host
and passes Linux-target strict Clippy, but only the hosted `/dev/fuse` job can
qualify the runtime interruption and unmount behavior; crash/restart and the
remaining lifecycle gates stay external, so W01 remains NO-GO.
The actual Darwin 27.0.0 arm64 host has no `/dev/fuse`, and the focused
non-Linux mount regression returns `UnsupportedPlatform` without touching its
requested path. W01-FUSE therefore explicitly supports Linux FUSE only; the
macOS native path remains NFS, with no FSKit or macFUSE FUSE-protocol claim.
This closes the macOS platform-scope decision but does not qualify any Linux
hosted or lifecycle gate, so W01 remains NO-GO.

The latest FUSE teardown packet makes forced session-task cancellation a
terminal lifecycle transition: bounded unmount-timeout and post-runtime
destructor fallbacks now mark the mount inactive/closed and wake
`wait_closed()` observers. A Linux-gated regression covers that contract;
host FUSE tests, host and Linux-target strict Clippy, Linux-target test check,
formatting and diff checks pass, while actual Linux `/dev/fuse` forced-unmount,
callback, crash/restart and durability execution remain external, so W01 stays
NO-GO.
The follow-up FUSE teardown packet makes the forced `umount`/lazy-detach ladder
and final session-task drain share one escalation deadline, so a graceful
timeout cannot be extended by a third full task timeout. A Linux-gated
stuck-helper regression checks the bounded `Timeout` result and terminal
inactive/closed state; host all-target FUSE tests, host/Linux-target strict
Clippy, Linux-target test check, formatting and diff checks pass. The timing
test itself and real hosted `/dev/fuse` forced-unmount, callback, crash/restart
and durability execution remain external. This packet is published as
`987c593bc08adfb161a55a7a9eee27ff82606310`; exact-SHA CI run `35648821996`
and Fault injection run `35648821873` are pending, so W01 stays NO-GO.
The next FUSE lifecycle packet now marks a mount inactive as soon as teardown
starts, rather than waiting for the graceful helper or session task to finish;
if that helper fails while the kernel mount is still present, the retry path
restores `active`. A Linux-gated synthetic-helper regression covers the
transition and terminal cleanup; host FUSE tests, host/Linux-target strict
Clippy, Linux-target test check, formatting and diff checks pass, while the
Linux-only runtime execution and hosted native close-race/callback,
crash/restart and durability gates remain external. This active-state packet is
published as `8ddf48febaedbd78dc22d889e8f3c822a4e6ad45`; exact-SHA CI run
`35649715601` is pending and Fault injection run `35649715727` is queued, so
W01 stays NO-GO.
The follow-up FUSE callback packet now reports a forced graceful-unmount
timeout as one owned `Task` transport error through the existing exactly-once,
panic-isolated hook, matching the pinned upstream forced-teardown callback
boundary. The Linux-gated stuck-helper regression checks the callback kind and
message plus bounded timeout and terminal state; host FUSE tests,
host/Linux-target strict Clippy, and Linux-target test check pass, while the
Linux-only callback execution and hosted native forced-unmount/fault,
crash/restart and durability evidence remain external. This packet is published
as `0d06abb10899f307ce83cd5fa198bab9c5f156a9`; exact-SHA CI workflow-dispatch
run `35650347479` is queued, push CI run `35650323951` was cancelled, and Fault
injection run `35650324040` is in progress. No hosted FUSE acceptance is
claimable, so W01 stays NO-GO.
The FUSE mount-source parity packet also maps the configured native `fsname`
through the automatic facade's shared `source` property, while unsupported
platform FUSE objects return no source. The Linux-gated Rust regression,
host/Linux-target FUSE and automatic-facade checks, and strict Clippy pass;
hosted native source/lifecycle execution remains external. This packet is
published as `2a979191d1ef5db37be3a9a3a4bbb2c3efe44457`; exact-SHA Fault
injection run `35652258903`, CI run `35652258837`, and W08 release targets run
`35652258840` are pending, while W04 production policy run `35652258913`
succeeded but is unrelated. W01 stays NO-GO.

The latest FUSE boundary packet also rejects native `max_frame` values below
the modern `FUSE_WRITE` header plus one page (`4176` bytes), so INIT cannot
advertise a write frame that the device receive limit would reject. Its README
now carries the explicit upstream mount-member ledger: direct Rust supports
the validated native options; the root automatic N-API surface intentionally
exposes only common options; `readers`, process-wide `signals`, and live
native `tap` are not claimed; request `onError` is mount-free only; terminal
`onTransportError` is supported; and transport-specific root members plus
automatic crash/restart recovery remain outside the supported contract. Host
FUSE tests, host/Linux-target strict Clippy, Linux-target checks, formatting,
and diff checks pass; Darwin cannot execute the Linux-only validation test, and
hosted Linux native lifecycle/callback evidence remains external. W01 stays
NO-GO.

The follow-up FUSE teardown packet addresses the first hosted native lifecycle
failure. Run `35657075892`, native-FUSE job `106523259210`, passed the backend
read-panic callback case but failed both the concurrent round-trip unmount and
blocked-read unmount at the test's 15-second bound. The graceful
`fusermount3 -u` phase could wait for a request that the session would only
cancel after the helper returned; the forced path now requests session stop
before lazy detach, with a Linux-gated helper-ordering regression. Local host
and Linux-target checks pass, but the corrected hosted native run is still
required, so W01 remains **NO-GO**.
The latest mount-free N-API FUSE packet closes a verified public-surface gap:
all 185 pinned FUSE wire constants are statically available through the
CommonJS and ESM `./fuse` barrel with declarations, and the barrel now also
exposes typed opcode body dispatch plus complete request/reply framing,
extension validation, raw/unknown handling, and the current `SYNCFS` codec.
The rebuilt debug addon, oracle-enabled codec/session/inode tests, generated
typecheck, and locked N-API Rust target pass. The pinned oracle still marks
`SYNCFS` unimplemented, so that extension has no oracle differential. The
oracle-enabled package suite reached NFS and stopped on this environment's
`Operation not permitted` socket bind; hosted Linux FUSE and remaining native
lifecycle gates remain external, so W01 stays NO-GO.

The next FUSE teardown attempt reached hosted Linux at exact commit
`962e981f`: native-FUSE job `106530560678` in CI run `35659287961` still
timed out both unmount cases. The first cancellation signal was present, but
the lazy helper was invoked before the session task had drained and released
the FUSE descriptor. The follow-up now drains or aborts that task before lazy
detach and shares one deadline across both phases; local host and Linux-target
checks remain green, while the hosted native rerun is still required. W01
remains **NO-GO**.

The detailed 9P ledger is [docs/W01_9P_PROGRESS.md](./W01_9P_PROGRESS.md).
The forced-unmount lifecycle follow-up now rechecks `/proc/self/mounts` after
the `umount`/lazy-detach ladder before publishing terminal state. If the
kernel mount remains present, the session stays `mounted=true` but
`active=false` because its serving task has been closed, and a later unmount
retry remains possible; only confirmed absence clears `mounted`. A Linux-gated
stuck-helper regression covers the still-present `/` case. Host FUSE
all-target tests (14 unit, 6 INIT, 0 native, 6 notify/record, 11 protocol, 20
session, 4 sync-barrier), formatting/diff checks, and Linux-target strict
Clippy pass; the Linux-only regression is compiled but not run on this macOS
host, while hosted forced-unmount, callback, crash/restart, concurrency, locks
and durability evidence remain external, so W01 stays NO-GO.

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

The follow-up 9P fid packet adds the Rust-backed `FidTable` alias and live
`P9Session.fids` view, mutable path/open/iounit/cursor state, deterministic
fid ordering, qid identity/cursor helpers, detached clunk snapshots, and
retained open-handle enumeration. Focused N-API and Rust tests pass, including
hardlink/release identity, large inode values, and a real opened session fid.
The next bounded packet adds the direct `./9p` probe/refusal/option helpers,
strict named `mount9p` delegation, 9P live-mount filtering/cleanup, and
mounted transport/server/connection/closed views. Its Rust/N-API local gates
pass. A dedicated `Native 9P` workflow now builds the public addon and runs
automatic, direct `./9p`, and structural-driver N-API mounted-I/O/cleanup
checks on a privileged Linux runner; exact SHA
`0ad4928e86af89163c8c87d08fea53ccf7f5f89b` passed those checks in [run
`35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703)
/ N-API job `106558367429`, including the direct `source` string assertion;
the Rust job `106558367006` passed all four ignored native tests. The pinned
oracle runtime check also passed `124 constants; 274 barrel exports`, and the
direct `MountP9Options` audit found no additional unrepresented fields. The
local N-API member-boundary regression also covers serializable callback option
snapshots and attached stream/peer state; corrected exact SHA
`81cc6596c2c9562c3405df50126239a7bcb44f63` passed [Native 9P run
`35670279904`](https://github.com/andymac4182/mount-rs/actions/runs/35670279904),
including the native `stream: undefined` and transport-source peer assertions.
Automatic cross-transport signal
ownership, supervisor-owned crash/reset/half-close recovery, and other W01 gates remain open, so the
overall W01 production decision remains **NO-GO**.

The latest 9P packet normalizes the oracle's optional absence shapes at the
N-API boundary: pre-version `msize`/`version`, unknown `userFor(fid)`, and
conflict-free table/session `getlock()` now return `undefined` rather than the
native binding's internal `null`. Exact SHA
`0d520a1d0a9af44e08e65c5f0638a640bb3c08db` passed [Native 9P run
`35671509538`](https://github.com/andymac4182/mount-rs/actions/runs/35671509538),
N-API job `106569412372` with automatic/direct/structural mounted I/O and
cleanup plus direct native session/lock assertions, and Rust job `106569412047`
with all four ignored tests. Broader upstream member parity, automatic
cross-transport signal ownership, supervisor-owned crash/reset/half-close
recovery, and broader W01 gates remain open; W01 remains NO-GO.

The following helper-parity slice adds the oracle's platform arguments to the
direct `./9p` probe helpers: `p9ClientProbe(platform?)` now reports deterministic
override facts without attempting a mount, and `p9Platform(platform?)` maps the
requested platform to `"linux" | undefined`; the no-argument probe remains
native-backed. Typecheck and the host-independent mount-helper regression cover
the new calls. Exact SHA `9da45327a9e09a9f827a9630869d1a32119674e3` also passed
[Native 9P run `35672845113`](https://github.com/andymac4182/mount-rs/actions/runs/35672845113),
with N-API job `106573050491` passing automatic/direct/structural mounted I/O
and cleanup and Rust job `106573049500` passing all four ignored native tests;
the hosted mount steps do not replace the local synthetic-override assertions.
Native Linux mount/lifecycle evidence is current for this revision, and W01
remains NO-GO.

The current 9P N-API packet also closes the session observability shape gap:
`P9Session.stats.messages` is normalized to the oracle's `Map<string, number>`
at the JavaScript boundary, with attached-session and hosted direct-mount
assertions reading the `Tversion` count through `Map#get`. Exact SHA
`e8c6043827e6cd0232a28f94b8fc665e25985f76` passed [Native 9P run
`35673543701`](https://github.com/andymac4182/mount-rs/actions/runs/35673543701),
N-API job `106575123928`, and Rust job `106575123716`; W01 remains NO-GO for
the explicitly listed unsupported/review gates.

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
| Auto/mount option and lifecycle surface | In progress | Shared `useDriverIno`, focused native `fuse`/`9p`/`nfs` option bags, configured FUSE `Mounted.source` mapping, package-level `signals` teardown, positive NFS `Mounted.port` readback, root `Mounted[Symbol.asyncDispose]()` disposal, shared NFS plus already-listened 9P server handles, transport-specific automatic transport-error callbacks for FUSE/9P/NFS, and the root auto `onTransportError` adapter now pass through the N-API auto facade; FUSE request callbacks, runtime callback-event qualification, remaining option/session members, mount object details, and full lifecycle parity remain | 64% | — |
| NFS and 9P complete session/server contracts | Partial | NFS now exposes a read-only N-API session view with v3/v4-aware direct request routing, shared v3/v4 counters and sorted BigInt handle snapshots, a v4 session view, mounts, destroyed state, live connection objects/count, and close/wait lifecycle; 9P now exposes direct raw-frame handling, the bounded Node attached-stream contract, and the native-listener stream scope decision alongside its session/connection view; the full upstream object/session/handle/attach surface and native qualification remain | 75% | — |
| S3 and WebDAV public Node surfaces | Partial | Native server facades exist; S3/WebDAV `drainTimeout` and `onTransportError` option shapes now map, both expose buffered direct `handleRequest`, WebDAV malformed-connection, direct peer-reset, lock-conflict, expiry, malformed-request, same-driver recreation, and same-session parallel-request evidence passes, both server objects expose shared session/statistics views, S3/WebDAV expose live connection views, S3 peer-aware connection-error reporting passes at the Rust transport boundary, and S3 also exposes safe effective options, bucket wrappers, debug-gated assertions, direct Node peer-fault delivery, and incremental `handleRequestStream` bodies with cancellation and async metrics; the WebDAV subpath now exposes pinned constants/status tables, protocol/XML/lock helpers, active lock-record and recursive owner views, and streamed request/response body bindings with direct class 1/2/3 method, LOCK/UNLOCK, cancellation, and error coverage; its supported callable/member and pure pinned oracle differentials also pass; broader option/member parity, live providers, and native/hosted lifecycle remain | 78% | — |
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
S3 session now retains debug-gated assertion messages/counters alongside its
bounded latency/byte/error-class metrics, while complete oracle member parity
remains open. The NFS server-object follow-up adds N-API `NfsSession`
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
loopback PUT/GET/404 lane pass. The S3 server's live `connections` count now
follows accepted TCP socket lifetimes and disconnect cleanup in both Rust and
N-API loopback tests. Its tracked TCP boundary and N-API bridge report a
peer-aware `Connection` transport event on reset-on-close; the direct Node
peer-fault packet produces exactly one callback and returns the connection
count to zero. The S3 view also exposes safe effective options and
session-owned bucket wrappers. The
WebDAV server-object follow-up now exposes a typed buffered
`WebdavSession.handleRequest` and pull-based `handleRequestStream` methods. A
direct N-API probe covers three-chunk PUT, multi-chunk GET, early response
iterator return, and deliberate request-body failure mapping; the stream facade
accepts async iterables and Web ReadableStreams. The N-API boundary also
materializes file-backed response bodies. The direct PUT plus the existing
loopback PUT/GET lane pass, and the direct method packet covers class 1/2/3
methods plus LOCK/UNLOCK. The `@mount-rs/core/webdav` subpath now exposes the
WebDAV constants/status barrel, protocol/XML/lock helpers, and oracle-matched
literals, limits, status/errno tables and server defaults. The session exposes
read-only active lock records with expiry cleanup; the direct LOCK/UNLOCK check
observes one record and then zero. A host-enabled Node socket reset during a
large WebDAV response delivers one typed peer-aware transport callback, and an
isolated malformed request delivers one typed peer-aware callback. An
unsubmitted-token write returns `423` before a one-second lock expires and is
removed. The active lock view now also preserves the recursive namespaced owner
XML tree, including predefined entity text. One hundred twenty-eight parallel unique-file
PUTs and GETs through one direct WebDAV session also pass with byte-for-byte
readback; this is in-process same-driver concurrency evidence only. The
full supported session/server prototype-member and class 1/2/3 direct-method
differentials now pass. The three oracle-only controls are now
an explicit supported-scope decision rather than an unrecorded gap: N-API does
not expose a synchronous JavaScript `now` callback, a callback for the current
transport's unreachable assertion sites, or out-of-band mutators for the
session-owned live `DavLockTable`; the Rust transport's injected clock and
exact expiry boundary are covered by a deterministic test. `stats.methods` now
has the oracle's `Map<string, number>` shape. Direct listener lifecycle and
local network concurrency are qualified; hosted concurrency and the external
provider/restart gates also remain open. The explicit ignored macOS native WebDAV
harness now passes its mount, I/O, and cleanup round-trip locally. At exact
scope-packet SHA `e13c52bea3485fada8be03ed62fba9a107255507`, hosted CI run
`35640746296` reported successful `native-webdav (macos-latest)` job
`106469172312` and `native-webdav (ubuntu-latest)` job `106469172419`; this
closes the hosted native WebDAV I/O slice for that packet, but not hosted
session/member, provider, restart/durability, or wider concurrency acceptance.
Same-driver server recreation preserves file bytes but resets session locks;
the N-API SQLite provider/reopen probe also preserves exact file bytes while
confirming replacement-session locks are process-local; crash/power-loss and
live-provider durability remain unqualified. The pinned pure
WebDAV barrel/protocol differential now passes with
`MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921` at oracle
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`; the pinned
`CARGO=./scripts/cargo-shared MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node --experimental-strip-types scripts/check-http-parity.mjs`
also passes all 40 paired TypeScript/Rust S3+WebDAV cases, including 16
authenticated WebDAV HTTP cases covering streaming PUT, XML properties,
ranges, conditional GET, PROPFIND, COPY/MOVE, refusal, missing-resource, and
DELETE behavior. The full supported server/session prototype-member and class
1/2/3 direct-method differentials pass; only the three oracle-only controls
remain outside supported scope. A read-only status check for the published tip
`9e8e4592cd8d4fe5b42c2734621ac1cd1bce02b5` found [CI run
35631845088](https://github.com/andymac4182/mount-rs/actions/runs/35631845088)
and [fault-injection run
35631845044](https://github.com/andymac4182/mount-rs/actions/runs/35631845044)
cancelled, while [Live Cloudflare R2 run
35631845090](https://github.com/andymac4182/mount-rs/actions/runs/35631845090)
failed; no hosted WebDAV PASS is claimable from this tip. Complete
session/member parity, direct listener lifecycle, network/native/hosted
concurrency, and the external provider/native/restart gates remain open.

The 2026-09-22 FUSE native mount-object packet is published as
`4fd3e25e030db5942e267a308a66ec12369feb36`, and the final local fetch verified
`HEAD=origin/main` at that SHA. Exact-SHA hosted status is still incomplete:
[CI run 35646646162](https://github.com/andymac4182/mount-rs/actions/runs/35646646162)
is pending and [Fault injection run
35646646113](https://github.com/andymac4182/mount-rs/actions/runs/35646646113)
is in progress; the completed W04 policy result and unrelated R2 failure do not
qualify Linux FUSE mount, callback, or lifecycle acceptance. W01 remains
**NO-GO**.

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
| PGlite rows | In progress | On tested revision `57ade44a4ac59899f3ff561075701046d94564ca`, the full PGlite gate passed: PGlite-enabled upstream 1,200 passed/82 skipped, Rust SDK 6/6, Node SDK 5/5, CLI 12 passed/2 skipped, and the PGlite reconnect, split-store, VFS, lifecycle, backup/restore, and FUSE-reconnect checks passed | Keep root-only, live-R2, TiDB/RustFS, and hosted rows classified; rerun when those prerequisites exist |
| Unstorage hardlinks | Done as boundary | Four exact `ENOSYS` rows, zero skips/mismatches | Preserve the capability declaration until alias semantics exist |
| Unstorage remaining capability inventory | Done as focused boundary | Fresh 25-row remaining-skip, 14-row next-capability, 4-row hardlink, 13-row aggregate-capability, and 11-row edge packets all matched the pinned oracle with zero skipped rows or mismatches | Reconcile the focused packets with the latest upstream inventory and retain the explicit unsupported capability declarations |
| Symlink, link-timestamp, statfs, and special-node skips | In progress | Capability reasons and focused first-operation assertions exist | Decide per row between implementation and an explicitly reviewed unsupported contract |
| NFS handle skips | Scoped/external | The pinned NFS columns explicitly declare unlinked-handle preservation out of scope; focused crash tests now reject a pre-crash v3 handle with `NFS3ERR_STALE` and a pre-crash v4 root handle with `NFS4ERR_STALE`, while v4 session state returns `NFS4ERR_BADSESSION` | Retain the explicit process-local/unlinked-handle boundary; durable or unlink-preserving handle state would require a separate implementation and qualification lane |
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
| PGlite and chunked-PGlite | Done locally | On tested revision `57ade44a4ac59899f3ff561075701046d94564ca`, five seeds × 621 operations passed for each backend; the emitted trace evidence reported 40/40 combinations passing |
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
| Rust CLI memory driver on macOS NFS | Done | On exact mainline `33d1d1c1991547b4fd0122013755288a61afb2d6`, the built CLI ran `mount --transport auto --driver memory --empty` on this macOS host; auto selected NFS, the kernel reported `127.0.0.1:/` at the disposable mountpoint, a mounted-path file round-trip passed byte-for-byte, Ctrl-C reported `unmounted`, and the exact mount/process were absent afterward |
| Node SDK CLI macOS NFS | Done | Native self-test passed mount/read/write/unmount/persistence |
| Linux FUSE native mount | External gate | Structural job and CI wiring exist; hosted `/dev/fuse` result is still required |
| Linux 9P and native NFS | External gate | Hosted Linux 9P Rust run `35628187344` / job `106427627397` and exact-SHA hosted N-API run `35664614270` / job `106547449823` passed kernel probing plus Rust, automatic, direct `./9p`, and structural-driver mounted I/O/cleanup; the macOS native NFSv3 loopback package gate passes, while Linux NFSv4.1 mount/read/write/unmount and native fault/race/crash evidence remain open |
| macOS FSKit/macFUSE boundary | Accepted out of scope | Actual Darwin 27.0.0 arm64 host has no `/dev/fuse`; the focused non-Linux mount regression returns `UnsupportedPlatform` without touching its path, and the implementation makes no FSKit or macFUSE FUSE-protocol claim |
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
| 2026-09-22 | W01-WebDAV | Added the explicit shared-resource ordering boundary: the pinned WebDAV HTTP oracle has no `PathLock`; the fresh concurrent-lock Rust regression returned `423` for both simultaneous writes without the submitted token, while refreshed 256-pair direct-session and loopback HTTP matrices passed for independent resources | — | 77% planning view | Independent-resource concurrency and WebDAV lock/`If` coordination are supported; linearizable same-resource ordering and atomic same-target `PUT` publication are outside scope; live-provider, power-loss, and durable-lock gates remain open |
| 2026-09-22 | W01-WebDAV | Corrected hosted CI `35678488755` at `ed29016e82c46e23e372f0615916ed3c1608370b` passed macOS job `106589998618` and Ubuntu job `106589998796` for the close-while-mounted teardown, bounded cleanup, relisten/remount, and post-restart native WebDAV round trip | — | 77% planning view | Hosted mounted teardown/restart is green; live-provider, power-loss, durable-lock, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Fresh manual CI `35678488755` at corrected SHA `ed29016e82c46e23e372f0615916ed3c1608370b` was dispatched after the hosted macOS cleanup observation; aggregate and native-WebDAV jobs remained queued at the latest refresh, so no hosted restart result is claimable | — | 77% planning view | Recheck exact native jobs to terminal state; live-provider, power-loss, durable-lock, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Exact-tip hosted CI `35677973511` at `6ccea74f0df6001e2627bf33e3c8b4ffb2d9d58b` reached macOS native WebDAV, where server teardown detached the mount and the explicit `umount` returned `not currently mounted`; the harness treated that already-unmounted terminal state as failure and the correction now accepts an absent mount while still failing an active mount | — | 77% planning view | Fresh hosted macOS/Linux rerun is required for teardown/restart acceptance; live-provider, power-loss, durable-lock, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Extended the ignored native WebDAV harness with close-while-mounted server teardown, bounded unmount, relisten/remount, and post-restart round-trip coverage; the current host-enabled macOS test passed 1/1, warning-denied WebDAV Clippy, rustfmt, focused compilation, and the focused Rust target passed | — | 77% planning view | Hosted rerun of the updated teardown/restart harness is required; live-provider, power-loss, durable-lock, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Hosted CI `35676711711` at `87b2e2f0f95ff58830f04c1bd80e5f49e14b7fea` passed the updated macOS/Linux native WebDAV jobs with the eight concurrent mounted write/read pairs | — | 77% planning view | Mounted-host I/O concurrency is green; adverse teardown/restart, live-provider, power-loss, durable-lock, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Extended the ignored native WebDAV mount harness with eight concurrent native-client write/read pairs and exact driver-side byte readback; the host-enabled macOS native test passed 1/1, the harness compiled, rustfmt passed, and the focused Rust target passed 20/20 | — | 77% planning view | At this local evidence snapshot, hosted Linux/macOS reruns were still required; the follow-up hosted row above passed mounted-host I/O concurrency. Live-provider, power-loss, durable-lock, mounted-host teardown, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Exact-tip hosted CI `35674823787` passed the four N-API `node` jobs, three Rust jobs, and both native WebDAV jobs at `e5ae05d07bbc73184952def0437e58be9efef790`; the `node` workflow runs the full pinned-oracle N-API package script | — | 77% planning view | Hosted cross-platform package/native evidence is green, but the overall workflow remains nonterminal on unrelated native-FUSE work; live-provider, power-loss, durable-lock, mounted-host concurrency, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Refreshed current checkout `d761deb23513ec78b61d7b627f67b46606ee4956`: NodeFs/SQLite reopen and crash recovery, 128-pair provider direct-session, 64-pair provider loopback network, in-flight streamed-`PUT` recovery, pinned barrel/session-member differentials, and the 40-case TypeScript/Rust S3+WebDAV HTTP differential passed | — | 77% planning view | Local provider/crash, pinned-oracle, and loopback evidence only; hosted lifecycle/concurrency, live-provider, power-loss, durable-lock, and wider ordering gates remain open |
| 2026-09-22 | W01-WebDAV | Requalified exact current tip `e5ae05d07bbc73184952def0437e58be9efef790`: locked WebDAV Rust tests passed 20 with the native probe explicitly ignored; N-API warning-denied Clippy, host-enabled release build, generated typecheck, structural durable-driver success/500-error/501-missing-callback, lifecycle, 64-pair direct-session, 64-pair network/auth/streaming, formatting, and diff checks passed | — | 77% planning view | The manual run was queued at this evidence snapshot; its subsequent successful native jobs are recorded below. No hosted PASS is claimable, and hosted/provider lifecycle, power-loss/live-provider durability, durable locks, and wider ordering remain open |
| 2026-09-22 | W01-WebDAV | Added structural N-API `FsDriver.syncfs()` forwarding, its public declaration, and a WebDAV `createWebdavServer` success/500-error/501-missing-callback regression; host-enabled release build, generated typecheck, adjacent WebDAV lifecycle/direct-session/network checks, N-API warning-denied Clippy, and locked WebDAV Rust tests passed 20/20 | — | 77% planning view | Local structural-driver/transport evidence only; hosted lifecycle/concurrency, live-provider, power-loss, durable-lock, and wider ordering gates remain open |
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
| 2026-09-22 | W01-FUSE | Added the protocol 7.34 eight-byte `SYNCFS` body codec and routed the native request to the existing `FsDriver::syncfs` barrier; success, backend failure, malformed/trailing bodies and empty replies are covered by focused tests | — | 80% planning view | Hosted kernel syncfs behavior and the remaining native lifecycle, callback, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Added typed napi-rs `NativeFuseSyncfsIn` request bindings, explicit `./fuse` CommonJS/ESM aliases, generated declarations, and exact eight-byte/truncated/trailing codec coverage; the release addon rebuild, focused codec test, generated typecheck, and locked N-API Rust check passed | — | 80% planning view | The pinned mountx oracle still classifies `SYNCFS` as unimplemented, so no oracle differential is claimed for this operation; hosted kernel syncfs behavior and the remaining native lifecycle, callback, crash/restart and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Re-verified the published-tip repair: formatting/diff checks, locked FUSE/N-API all-target tests, isolated strict Clippy, and the Linux-target FUSE test compile passed; rebuilding the debug N-API addon followed by generated typecheck, mount-free session, pinned codec-oracle, and inode-oracle tests passed | — | 80% planning view | Hosted CI `35632382070` was cancelled and provides no Linux `/dev/fuse` evidence; native mount/callback, FSKit, close/crash/restart, write/mutation concurrency, locks, and durability remain explicit NO-GO gates |
| 2026-09-22 | W01-FUSE | Rebased the local FUSE checkpoint onto current `origin/main`, including the eight-client and callback-fault native harnesses; the locked FUSE/N-API targets, strict FUSE Clippy, Linux FUSE test compile, rebuilt debug addon, generated typecheck, mount-free session, pinned codec oracle, and inode oracle all passed | — | 80% planning view | The ignored native harnesses were not executable on this macOS host; hosted `/dev/fuse`, callback delivery, panic/cleanup, close/crash/restart, mutation/write concurrency, locks, durability, and FSKit remain explicit NO-GO gates |
| 2026-09-22 | W01-FUSE | Refreshed the macOS FSKit boundary: the locked 12-test Rust bridge target, formatting, strict Clippy, arm64 bridge build, standalone Swift delegate seam, in-process XPC lifecycle test, and unsigned arm64 `MountRsFSKit`/`MountRsXPCService`/`MountRsHost` Xcode schemes passed; expected arm64 extension and XPC artifacts were present | — | 80% planning view | Diagnostic activation (with mount attempt skipped) returned `FSKIT_ACTIVATION=BLOCKED` for zero valid signing identities, ad-hoc host signing, and unavailable `fskitd`; signed/installed/enabled FSKit, mounted read/write, hosted Linux native, callback/lifecycle, crash/restart, concurrency, locks, and durability evidence remain open |
| 2026-09-22 | W01-FUSE | Inspected hosted CI run `35645669363` at `682d2441`: the `native-fuse` job `106485735709` passed the Linux FUSE prerequisite probe but was cancelled during the actual rootless kernel file-operation step, so it supplies no native acceptance evidence | — | 80% planning view | The exact branch tip still needs a non-canceling manual hosted qualification; native Linux mount/callback/lifecycle, crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Inspected [CI run `35647249560`](https://github.com/andymac4182/mount-rs/actions/runs/35647249560) at `68f775a53aae16832a5438b74fb21c06a36dd838`: [native-fuse job `106491113021`](https://github.com/andymac4182/mount-rs/actions/runs/35647249560/jobs/106491113021) reached the native scenarios but failed the ordinary, blocked-read, and backend-panic cases because `Mounted::unmount()` timed out before the run was later cancelled; the native loop now treats kernel `FUSE_DESTROY` as terminal, aborts/drains read workers, and closes the session, with host tests/Clippy and Linux-target test compilation passing | — | 80% planning view | This is a diagnosed hosted failure plus a locally verified fix, not native acceptance; exact-tip hosted rerun, callback/lifecycle, close/crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Inspected manual [CI run `35650347479`](https://github.com/andymac4182/mount-rs/actions/runs/35650347479) at `0d06abb10899f307ce83cd5fa198bab9c5f156a9`: [native-fuse job `106500945410`](https://github.com/andymac4182/mount-rs/actions/runs/35650347479/jobs/106500945410) passed the prerequisite and backend-panic callback/close case, but ordinary and blocked-read `Mounted::unmount()` timed out; the matrix was still in progress with unrelated failures, and no native acceptance is claimed | — | 80% planning view | The current destroy-boundary fix still requires exact-tip hosted validation; native unmount, close-race, callback, crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Bounded terminal cleanup now aborts positional-read workers and caps the final drain at one second, preventing an uncooperative backend future from holding the native FUSE device open forever; a Linux-gated blocking-worker regression was added, with host FUSE tests, host/Linux-target strict Clippy, Linux-target compilation, formatting and diff checks passing | — | 80% planning view | Exact-tip hosted unmount, callback, close-race, crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Bounded kernel-initiated session cleanup by the configured unmount timeout so an uncooperative backend handle `close()` cannot hold `FUSE_DESTROY` forever; cleanup and terminal read-drain timeouts now report one owned `Task` transport error, with a Linux-gated blocking-close regression | — | 80% planning view | Host FUSE tests, host/Linux-target strict Clippy, Linux-target compilation, formatting and diff checks pass; exact-tip hosted unmount, callback, close-race, crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Removed the native read-concurrency permit deadlock: up to 16 positional reads run concurrently, another 16 are bounded in pending state, overflow receives `EAGAIN`, and lifecycle/control frames continue to be read while the workers are blocked; a Linux-gated datagram regression fills all 16 slots, queues a 17th read, and then destroys the session | — | 80% planning view | Host FUSE tests, host/Linux-target strict Clippy, Linux-target compilation, formatting and diff checks pass; exact-tip hosted unmount, callback, close-race, crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Narrowed `FUSE_INTERRUPT` cancellation to the targeted read worker so unrelated blocked reads do not force a one-second drain timeout or close the session; the Linux-gated interrupt regression now keeps a second read blocked while the first is canceled | — | 80% planning view | Host FUSE tests, host/Linux-target strict Clippy, Linux-target compilation, formatting and diff checks pass; exact-tip hosted interrupt, unmount, callback, close-race, crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Made terminal read-worker draining unconditional after abort: all registered workers are joined within the one-second bound even when a prior worker already failed, while the first owned transport error is retained for callback delivery | — | 80% planning view | Host FUSE tests, host/Linux-target strict Clippy, Linux-target compilation, formatting and diff checks pass; exact-tip hosted interrupt, unmount, callback, close-race, crash/restart, concurrency, locks, durability and the overall W01 decision remain NO-GO |
| 2026-09-22 | W01-FUSE | Tightened forced native teardown after diagnosing the blocked-read timeout budget: give the session 250ms to observe stop, then abort the owner task before starting the full lazy-detach deadline so a `/dev/fuse` read cannot consume a second full timeout while retaining the device descriptor | — | 80% planning view | Host FUSE tests, formatting/diff, and Linux-target strict Clippy pass; exact-tip hosted Linux native-FUSE rerun remains required, and W01 stays NO-GO |
| 2026-09-22 | W01-FUSE | Current-main hosted run [35662346488](https://github.com/andymac4182/mount-rs/actions/runs/35662346488) at `b27dd2b` passed the real round-trip and backend-panic callback cases but failed only the blocked-read unmount with `native blocked-read unmount timed out: Err(Elapsed(()))` in native-FUSE job `106540264683`; retained one stop-notify permit and added a Linux-gated blocked-positional-read stop regression | — | 80% planning view | Local FUSE tests, formatting/diff, and Linux-target strict Clippy pass; exact-tip hosted rerun is required before native unmount, callback/lifecycle, crash/restart, concurrency, locks, durability, and the overall W01 decision can change from NO-GO |
| 2026-09-22 | W01-FUSE | Added a stop-aware terminal reply for positional reads: when teardown interrupts a prepared backend read, the worker returns `EIO` for the original request unique before session close; the Linux-gated Unix-stream regression asserts that reply. Host FUSE all-target tests (14 unit, 6 INIT, 0 native, 6 notify/record, 11 protocol, 20 session, 4 sync-barrier), host strict Clippy, Linux-target check/strict Clippy, formatting and diff checks passed | — | 80% planning view | Manual hosted CI `35662415701` / native-FUSE job `106540484337` passed ordinary round-trip and backend-panic close but timed out both the blocked-read `Mounted::unmount()` and kernel read; this remains a diagnosed cancellation gap requiring exact-tip Linux rerun, with native lifecycle/callback, crash/restart, concurrency, locks, durability and W01 still NO-GO |
| 2026-09-22 | W01-FUSE | Manual non-canceling CI `35666436803` / native-FUSE job `106553144636` passed ordinary round-trip and backend-panic close but blocked-read unmount exceeded the 15s observation bound. Forced teardown now launches lazy detach concurrently with the 250ms stop grace so descriptor closure can release the helper within the existing bound; local verification passes, but exact hosted rerun and all remaining native gates are still required | — | 80% planning view | W01 remains NO-GO |
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
| 2026-09-22 | W01-9P | Closed the focused N-API 9P driver/observability packet: `P9Session.driver`, debug-gated assertion readback/counters, request-error and assertion callbacks, Node error revival, and root/`./9p` factory identity are now evidenced. Release build, generated typecheck, pinned 124-export/44-case codec differentials, focused N-API tests, 31 ordinary 9P tests, formatting, and warning-denied Clippy passed | — | 65% W01.1 planning view | Lock-table option injection, property-shaped `clients`, 9P mount helpers, fresh hosted native execution at this revision, crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Closed the property-shaped client slice: generated declarations and runtime now expose `P9Server.clients` as a live array covering native and attached connections; focused P9 runtime/type checks, host-enabled server integration, and the release build passed | — | 65% W01.1 planning view | Lock-table option injection, 9P mount helpers, fresh hosted native execution at this revision, crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Closed the lock-table option-injection slice: `P9ServerOptions.locks` accepts a `P9LockTable`, and injected ranges are visible through server and connection-session option handles while remaining shared with protocol lock state; release build, generated typecheck, host-enabled server integration, focused P9 runtime checks, 31 Rust tests, and strict Clippy passed | — | 65% W01.1 planning view | 9P mount helpers, fresh hosted native execution at this revision, crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added the bounded direct `./9p` mount-helper facade: Linux-client probe, Unix/TCP refusal and option-string helpers, strict named `mount9p` delegation, 9P live-mount filtering/cleanup, nested auto 9P fields, and mounted transport/server/connection/closed views; release build, generated typecheck, focused P9 runtime regressions, host-enabled server integration, `mount-rs-napi --lib` 17/17, 35 ordinary 9P tests, formatting, and warning-denied Clippy passed | — | 65% W01.1 planning view | Oracle shared-server/signal/extended server-policy mount options, hosted N-API native-mount lifecycle at this revision, crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added configured shared-server adoption to the native `./9p` mount option: `mount9p` starts a supplied server only when the Linux client probe is usable, and the Rust adapter passes its exact bound transport to the native adoption path; N-API compile/build, generated typecheck, helper validation, syntax, and diff checks passed | — | 65% W01.1 planning view | Oracle signal/extended server-policy/session controls, hosted N-API native-mount lifecycle at this revision, crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added the opt-in direct `./9p` native lifecycle test and a dedicated hosted N-API Linux job covering automatic, direct, and structural-driver 9P mounted I/O and cleanup after `9p`/`9pnet_fd` probing; exact SHA `1dcf4dee4d01fb5e3807335579659b54efd74351` passed the N-API job `106547449823` and Rust job `106547449501` in run `35664614270` | — | 65% W01.1 planning view | Automatic cross-transport signal ownership, remaining mount controls, crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Closed the direct return-shape parity mismatch: `P9Mount.source` is now declared as `string` and asserted as a non-empty string in the hosted direct mount; generated typecheck, syntax, helper, and diff checks passed locally. Exact SHA `3c884bd8c0d0199a17e4c355c36d45f660c7c786` passed N-API job `106552944097` and Rust job `106552944349` in run `35665824215`; a pinned direct-option audit found no additional unrepresented `MountP9Options` fields | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Closed the remaining pinned public-barrel/default parity slice: the direct facade, postlude binding, and generated declarations now expose `DEFAULT_P9_PORT`, `DEFAULT_SOCKET_MODE`, `DEFAULT_MAX_IN_FLIGHT`, `DEFAULT_MSIZE`, `P9_LOCK_EOF_END`, and `DEFAULT_MAX_LOCKS_PER_FILE`; the pinned runtime check passed all 124 constants and all 274 upstream 9P barrel exports, while local typecheck, syntax, helper, and diff checks passed. Exact SHA `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` passed N-API job `106558367429` and Rust job `106558367006` in [Native 9P run `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703) | Automatic cross-transport signal ownership, native listener `stream: undefined`, supervisor-owned crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added focused N-API representation-boundary checks: effective `onError`/`onAssertion` hooks are omitted from serializable option snapshots, attached connections retain their supplied Node `Duplex` and peer, and the direct native test asserts native listener `stream: undefined` plus a transport-source peer string; local metadata, mount-helper, typecheck, syntax, and diff checks passed | — | 65% W01.1 planning view | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Hosted exact SHA `03529cf30985c2be6503c2909b94e646565cf6fe` in [Native 9P run `35669536706`](https://github.com/andymac4182/mount-rs/actions/runs/35669536706) passed Rust job `106562666985` and automatic N-API mounted I/O, but the direct N-API check failed only because the new test expected native Unix `peer: null` while the transport correctly returned its socket path; the test and supported-scope wording now require `stream: undefined` with a non-empty transport-source peer string | — | 65% W01.1 planning view | Corrected exact-SHA hosted rerun required; automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Corrected exact SHA `81cc6596c2c9562c3405df50126239a7bcb44f63` passed [Native 9P run `35670279904`](https://github.com/andymac4182/mount-rs/actions/runs/35670279904): N-API job `106565351978` passed automatic/direct/structural mounted I/O and cleanup, and Rust job `106565352174` passed all four ignored native tests; this qualifies native `stream: undefined` with the transport-source Unix peer string after the prior test-oracle correction | — | 65% W01.1 planning view | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Normalized the N-API 9P optional absence shapes to the oracle: pre-version `msize`/`version`, unknown `userFor(fid)`, and conflict-free `P9LockTable`/`P9LockClient.getlock` now return `undefined`; generated declarations, metadata/lock runtime checks, syntax, typecheck, and diff checks pass locally. Exact SHA `0d520a1d0a9af44e08e65c5f0638a640bb3c08db` passed [Native 9P run `35671509538`](https://github.com/andymac4182/mount-rs/actions/runs/35671509538): N-API job `106569412372` passed automatic/direct/structural mounted I/O and cleanup plus direct native session/lock assertions, and Rust job `106569412047` passed all four ignored native tests | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added oracle-compatible optional platform arguments to the direct `./9p` probe helpers: `p9ClientProbe(platform?)` and `p9Platform(platform?)` now support deterministic Linux/Darwin override checks without attempting a mount; typecheck and the host-independent mount-helper regression passed, while the no-argument probe remains native-backed | — | 65% W01.1 planning view | Native Linux mount/lifecycle evidence is unchanged; automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Closed the N-API session-statistics shape gap: `P9Session.stats.messages` now matches the oracle's `Map<string, number>` at the JavaScript boundary; attached-session observability, session metadata, generated typecheck, syntax, and diff checks passed locally, with the direct native mount asserting `Map#get("Tversion")`. Exact SHA `e8c6043827e6cd0232a28f94b8fc665e25985f76` passed [Native 9P run `35673543701`](https://github.com/andymac4182/mount-rs/actions/runs/35673543701), N-API job `106575123928` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106575123716` with all four ignored native tests | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Added oracle-compatible optional maximum-item arguments to the N-API body readers `readRread`, `readTwrite`, and `readRreaddir`; the 44-case codec differential covers bounded success and oversized-body errors, and generated typecheck, syntax, fid/runtime, diff, formatting, strict Clippy, and 18 focused Rust tests pass locally. Exact SHA `bba379ebe4339e951de9cb7ca02b4b499c3a3874` passed [Native 9P run `35677755888`](https://github.com/andymac4182/mount-rs/actions/runs/35677755888), N-API job `106587739639` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106587739402` with all four ignored native tests | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and broader W01 gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-9P | Extended the bounded-reader parity to native `P9Reader.readRread`, `readTwrite`, and `readRreaddir` convenience methods; the helper/typed-reader 44-case differential covers bounded success and oversized-body errors, and generated declarations, typecheck, syntax, fid/runtime, diff, formatting, strict Clippy, and 18 focused Rust tests pass locally. Exact SHA `4ecdb63db64711e0fadf77d4612b6394f57f3f4d` passed [Native 9P run `35678757675`](https://github.com/andymac4182/mount-rs/actions/runs/35678757675), N-API job `106590841909` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106590841982` with all four ignored native tests | Automatic cross-transport signal ownership, supervisor-owned crash/reset/half-close recovery, broader upstream member parity, and broader W01 gates remain open; W01 stays NO-GO |

| 2026-09-22 | W01-FUSE | Added fail-closed validation for caller-supplied native FUSE mount option tokens and transport-owned overrides; focused `mount-rs-fuse` all-target tests and strict Clippy passed on macOS | — | 35% W01.4 planning view | Hosted Linux `/dev/fuse`, callback-event, crash/concurrency/durability, and signed/activated FSKit evidence remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added active NFS socket-task accounting with abort-safe close draining and read-only sorted BigInt shared-handle snapshots on both the v3 and v4 N-API views. Rust NFS tests passed 31 unit, rootless wire 1, transport errors 4, v4 barrier 1, and v4 wire 2; the release addon, generated typecheck, and live N-API server integration passed | — | 72% W01.1 planning view | Full v3/v4 stateful matrix, remaining upstream session/member parity, hosted lifecycle, Linux NFSv4.1 and crash/durability gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added live NFS connection objects with stable id/peer/shared-session views and abort-safe `close`/`waitClosed`; the `./nfs` subpath identity check, release addon, generated typecheck, distribution check, and host-enabled N-API server integration passed | — | 75% W01.1 planning view | Full v3/v4 stateful matrix, hosted lifecycle, Linux NFSv4.1, crash/durability, and any remaining upstream member differences remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added rootless NFSv4.1 session continuity across an orderly TCP reconnect and a rootless eight-request NFSv3 pipelining test with bounded in-flight dispatch; the complete locked NFS target passed 31 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 3, with strict affected Clippy and formatting green | — | 75% W01.1 planning view | Process-lifetime userspace evidence does not qualify automatic reconnect, crash/restart or durable recovery, native Linux NFSv4.1, hosted lifecycle, or the full stateful/member matrix; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added a rootless restart-boundary classification: after `NfsServer::close()`, a replacement server sharing the backend rejects the old NFSv4.1 session with `NFS4ERR_BADSESSION`; the v4 wire target passed 4/4 | — | 75% W01.4 planning view | This explicitly classifies v4 session/lease/replay state as process-local, but backend durability, crash injection, native Linux NFSv4.1, hosted lifecycle, and full upstream state/member parity remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | The refreshed opt-in macOS native NFSv3 loopback mount gate passed 1/1 in 0.11s on the exact pushed tip, including filesystem round trips and bounded cleanup | — | 75% W01.4 planning view | Linux NFSv4.1, Linux 9P, hosted lifecycle, full stateful parity, crash/concurrency/durability, and live-provider gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | The v4 channel/state follow-up aligned `CREATE_SESSION` error and counter-offer boundaries (`NFS4ERR_TOOSMALL`, `NFS4ERR_NOSPC`, and preserved back-channel counts), while the rootless v4 wire suite remained 5/5 and scoped Clippy stayed green | — | 75% W01.1 planning view | ID-map/clock/seed and session `onError` parity, Linux NFSv4.1, hosted lifecycle, full stateful parity, crash/concurrency/durability, and live-provider gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | The v4 `CREATE_SESSION` state slot now caches refusals for replay and advances to the next sequence for a retry; the focused state-limit wire test proves undersized-offer replay followed by successful next-sequence creation | — | 75% W01.1 planning view | ID-map/clock/seed and session `onError` parity, Linux NFSv4.1, hosted lifecycle, full stateful parity, crash/concurrency/durability, and live-provider gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | The bounded `requireReclaimComplete` policy now gates both v4 `OPEN` and `LOCK` state establishment with `NFS4ERR_GRACE` until `RECLAIM_COMPLETE`; the affected rootless v4.1 round-trip and same-owner/cross-client share tests pass | — | 75% W01.1 planning view | ID-map/clock/seed and session `onError` parity, Linux NFSv4.1, hosted lifecycle, full stateful parity, crash/concurrency/durability, and live-provider gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added deterministic static NFSv4 user/group ID maps across the Rust transport and N-API `nfs4.idmap`: mapped names are domain-qualified, unmapped ids remain numeric, and other domains return `NFS4ERR_BADOWNER`; the full locked NFS target passed 35 unit tests, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 5, with release build/typecheck, live N-API integration, and strict Clippy green | — | 75% W01.1 planning view | Callback-based ID maps, deterministic clock/seed controls, session `onError`, Linux NFSv4.1, hosted lifecycle, full stateful parity, crash/concurrency/durability, and live-provider gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added panic-isolated Rust NFSv4 owner callbacks and synchronous N-API `nfs4.idmap.nameOf`/`idOf` plus `nfs4.now` bridges. The live N-API v4.1 sequence exercised `EXCHANGE_ID`/`CREATE_SESSION`, owner/group `GETATTR`, reverse owner `SETATTR`, and injected clock calls; the locked NFS target passed 38 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 6, with release build/typecheck, live server integration, pinned NFS codec differential, and strict Clippy green | — | 75% W01.1 planning view | Full upstream state/member parity, privileged Linux NFSv4.1, hosted lifecycle, cross-process crash/cancellation/concurrency/durability, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added bounded real-TCP cancellation coverage for both connection/server close, then added direct N-API `NfsSession.destroy()` and `Nfs4Session.destroy()` with generated typing and live runtime assertions. The lifecycle target passed 2/2; the complete locked NFS target passed 38 unit, rootless wire 1, transport concurrency 1, transport errors 4, lifecycle 2, v4 barrier 1, and v4 wire 6; release build, generated typecheck, live N-API server integration, pinned codec differential, and strict affected Clippy were green | — | 75% W01.1 planning view | Remaining upstream `driver`/`options`/direct-v3 member differences, privileged Linux NFSv4.1, a completed hosted lifecycle run, cross-process crash/concurrency/durability, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added N-API NFS session/member parity: unified, direct v3, and direct v4 views now expose server-owned read-only driver wrappers, effective scalar options, configured ID-map presence, write verifiers, and shared v3 routing; `./nfs` exports `Nfs3Session`. Release declarations, generated type assignments, live runtime identity/options/verifier assertions, the refreshed pinned NFS conformance (266 passed, 18 explicit capability/root skips, 0 mismatches), the pinned NFS codec differential, the complete locked NFS targets, and strict affected Clippy passed. The low-level mutable v4 state table and callback function values are explicitly outside the supported N-API inspection surface and remain covered through wire/callback/lease evidence; hosted CI runs `35656252661` and `35657100915` were cancelled before any job started | — | 75% W01.1 planning view | Privileged Linux NFSv4.1, a completed hosted lifecycle run, cross-process crash/concurrency/durability, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Hosted run `35658285441` passed both `native-nfs` jobs: macOS job `106528418544` passed native NFSv3, CLI persistence/cleanup, and SQLite-over-NFS; Ubuntu job `106528418983` passed privileged native NFSv3/NFSv4.1 and SQLite-over-NFS, with macOS-only CLI checks skipped on Ubuntu | — | 75% W01.4 planning view | The named NFS jobs are accepted platform evidence, but the overall workflow remains non-green because unrelated jobs failed; full upstream state/member scope, cross-process crash/concurrency/durability, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added a forced cross-process NFSv3 restart test: a `FILE_SYNC` write through a child server survived SIGKILL, and a replacement child server using the same `HostFs` root recovered it through MOUNT/LOOKUP/READ; the focused target passed 1/1 and the complete locked NFS target remained green. The subsequent code-bearing hosted run `35661453808` cancelled both native-NFS jobs before any steps | — | 75% W01.4 planning view | NFSv4 lease/replay/file-handle recovery, power-loss durability, cross-process concurrency, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Tightened the forced cross-process restart test: the replacement server rejects the pre-crash NFSv3 file handle with `NFS3ERR_STALE`, then recovers the same `FILE_SYNC` bytes through a fresh MOUNT/LOOKUP/READ path; the focused target passed 1/1 | — | 75% W01.4 planning view | This explicitly classifies NFSv3 handles as process-local while confirming host-backed data recovery; NFSv4 lease/replay/file-handle recovery, power-loss durability, cross-process concurrency, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added a forced-crash NFSv4.1 session boundary: a real child session was force-terminated and a replacement child rejected the old session with `NFS4ERR_BADSESSION` before dispatch; both process-restart tests passed and strict affected Clippy remained green | — | 75% W01.4 planning view | This classifies NFSv4 session/lease/replay state as process-local under crash, not durable v4 recovery; cross-process/native-client concurrency, power-loss durability, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added rootless NFSv4.1 two-client concurrency coverage: two independent sessions concurrently completed distinct-file OPEN/WRITE/READ round trips over separate TCP connections, with the full v4 wire target passing 7/7 and warning-denied NFS Clippy green; multiple server processes sharing one backend are explicitly outside supported scope because state arbitration is process-local | — | 75% W01.4 planning view | Native-client ordering remains a separate hosted/platform gate; NFSv4 lease/replay/file-handle recovery, power-loss durability, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Exact-tip hosted status for the v4 concurrency commit: [CI run `35663954461`](https://github.com/andymac4182/mount-rs/actions/runs/35663954461) was cancelled before GitHub created any jobs, so it supplies no hosted native-NFS result; the last accepted named native jobs remain [run `35658285441`](https://github.com/andymac4182/mount-rs/actions/runs/35658285441) | — | 75% W01.4 planning view | Hosted native evidence is still job-scoped to `35658285441`; cross-process/native-client concurrency, NFSv4 recovery/durability, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Strengthened the forced-crash boundary packet: v3 rejects the pre-crash file handle with `NFS3ERR_STALE` while recovering `FILE_SYNC` bytes through a fresh path; v4 rejects the old session with `NFS4ERR_BADSESSION` and the old root handle with `NFS4ERR_STALE`; session IDs now fold both write-verifier halves. The focused process-restart tests passed 2/2, the complete locked NFS target passed 39 unit tests plus its applicable integration targets, and strict Clippy, formatting, and diff checks passed | — | 75% W01.4 planning view | This evidences process-local v3 handles and v4 session/root-handle state, not durable or unlink-preserving recovery; cross-process/native-client concurrency, power-loss durability, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Refreshed the pinned NFS conformance oracle at the current pushed tip: `nfs-conformance.test.mjs` passed 266 tests, recorded 18 explicit capability/root skips, and reported zero mismatches using the isolated shared Cargo target and pinned `MOUNTX_SOURCE` checkout | — | 75% W01.1 planning view | The explicit handle/root capability boundary remains scoped rather than an unreviewed open skip; full upstream state/member parity, durable/reconnect semantics, cross-process/native-client concurrency, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Closed the queued-request teardown race: with `max_in_flight=1`, a second real-TCP MOUNT waits behind a blocked first request and `NfsConnection::close()` now cancels the semaphore wait, aborts the active worker, and retires the connection within the bounded lifecycle gate. The complete locked NFS target passed 39 unit tests, process restart 2, rootless wire 1, transport concurrency 1, transport errors 4, lifecycle 3, v4 barrier 1, and v4 wire 7; strict Clippy, formatting, and diff checks passed | — | 75% W01.4 planning view | This closes local queued-request teardown; automatic reconnect, full upstream state/member parity, native-client ordering, cross-process concurrency, crash/durability qualification, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Serialized concurrent NFS listen/close lifecycle calls: concurrent `NfsServer::listen()` calls share one bound TCP listener, and `close()` cannot race a bind. The focused lifecycle target passed 4/4, the complete locked NFS target passed 39 unit tests and all applicable integration targets, and strict Clippy, formatting, and diff checks passed | — | 75% W01.4 planning view | This closes the local duplicate-listener/bind-close race; automatic reconnect, full upstream state/member parity, native-client ordering, cross-process concurrency, crash/durability qualification, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added real-TCP completion-order coverage: a blocked earlier NFSv3 `GETATTR` no longer holds a later fast `GETATTR` on the same connection, and both XIDs return exactly once. The focused transport-concurrency target passed 2/2; the complete locked NFS target passed 39 unit tests, process restart 2, rootless wire 1, transport concurrency 2, transport errors 4, lifecycle 4, v4 barrier 1, and v4 wire 7; strict Clippy, formatting, and diff checks passed | — | 75% W01.4 planning view | This qualifies in-process userspace reply ordering only; native-client ordering, cross-process concurrency, crash/durability qualification, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Made Rust NFS server close terminal: after session teardown, `listen()` now rejects with `NotConnected` instead of returning a stale bound address. The focused lifecycle target passed 5/5 and the complete locked NFS target passed 39 unit tests, process restart 2, rootless wire 1, transport concurrency 2, transport errors 4, lifecycle 5, v4 barrier 1, and v4 wire 7; strict Clippy, formatting, and diff checks passed | — | 75% W01.4 planning view | The local relisten-after-close hole is closed; native-client ordering, cross-process concurrency, crash/durability qualification, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Closed the connection waiter lost-wakeup interval: `wait_closed()` registers before checking the completion flag. The focused unit test passed 32 concurrent waiters and a late waiter; the complete locked NFS target passed 40 unit tests, process restart 2, rootless wire 1, transport concurrency 2, transport errors 4, lifecycle 5, v4 barrier 1, and v4 wire 7, with strict Clippy, formatting, and diff checks green | — | 75% W01.4 planning view | This qualifies local waiter completion; native-client ordering, cross-process concurrency, crash/durability qualification, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Added rootless real-TCP in-flight cap coverage: with `max_in_flight=1`, a second pipelined NFSv3 `GETATTR` remains undispatched behind a blocked first call; after release, both distinct XIDs arrive exactly once. The focused concurrency target passed 3/3 and the complete locked NFS target passed 40 unit tests, process restart 2, rootless wire 1, transport concurrency 3, transport errors 4, lifecycle 5, v4 barrier 1, and v4 wire 7; strict Clippy, formatting, and diff checks passed | — | 75% W01.4 planning view | Socket write backpressure, native-client ordering, cross-process concurrency, crash/durability qualification, full upstream state/member scope, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Refreshed the pinned oracle at published terminal-close commit `90e130b8`: 266 NFSv3/MOUNT and NFSv4.1 cases passed, 18 capability/root cases were explicitly skipped, and zero mismatches were reported. CI run `35670416469` cancelled both native-NFS jobs before any steps ran | — | 75% W01.1 planning view | The oracle confirms userspace TCP behavior at this commit; the cancelled jobs add no hosted native evidence. Full state/member scope, native-client ordering, crash/durability qualification, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Refreshed the pinned NFS oracle after listen/close serialization at the current published tip: 266 NFSv3/MOUNT and NFSv4.1 TCP cases passed, 18 capability/root cases remained explicit skips, and zero mismatches were reported | — | 75% W01.1 planning view | This confirms no conformance regression from the lifecycle serialization fix; the explicit capability boundary, full upstream state/member scope, native-client ordering, crash/durability qualification, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-NFS | Refreshed the pinned NFS oracle after the queued-request cancellation fix at the current published tip: 266 NFSv3/MOUNT and NFSv4.1 TCP cases passed, 18 capability/root cases remained explicit skips, and zero mismatches were reported | — | 75% W01.1 planning view | This confirms no conformance regression from the lifecycle fix; the explicit capability boundary, full upstream state/member scope, native-client ordering, crash/durability qualification, and production acceptance remain open; W01 stays NO-GO |
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
| 2026-09-22 | W01-WebDAV | Classified the pre-callback session member boundary against the pinned source: N-API exposed scalar options, snapshot lock records, stats and assertions, but did not yet claim injectable `now`, `onError`, `onAssertion`, a live `DavLockTable`, or `Map`-shaped method counters | — | 72% W01.1 planning view | The current request-error and Map parity rows supersede those two gaps; injectable `now`, `onAssertion`, live `DavLockTable`, native/hosted lifecycle, provider qualification, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Normalized N-API `WebdavSession.stats.methods` to the oracle's `Map<string, number>` shape in the WebDAV postlude; release build, generated typecheck, isolated direct `PUT` probe, and the pinned pure WebDAV differential passed, while the package-wide server harness stopped in the unrelated 9P relisten phase before WebDAV | — | 72% W01.1 planning view | Injectable `now`, `onAssertion`, live `DavLockTable`, complete session/member parity, native/hosted lifecycle, provider qualification, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added a request-level N-API `onError(error, head)` callback through a separate session-hook constructor; the Rust WebDAV target passed 14/14 including unsupported-method and authenticated-401 callback cases, `./scripts/cargo-shared check -p mount-rs-napi --locked`, the release addon, generated declaration/typecheck, and an isolated rebuilt N-API PATCH callback probe passed | — | 72% W01.1 planning view | Injectable `now`, `onAssertion`, live `DavLockTable`, complete session/member parity, native/hosted lifecycle, provider qualification, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Recorded the N-API supported-scope decision for the oracle-only clock/assertion/live-lock-table controls; `./scripts/cargo-shared test -p mount-rs-webdav --test webdav --locked injected_session_clock_controls_lock_expiry_deterministically` passed 1/1 and proved the Rust injected clock expires a lock exactly at its configured millisecond boundary; the N-API binding retains safe serializable options, empty assertion readback, and expiry-aware lock snapshots | — | 72% W01.1 planning view | Broader session/member differential, provider/native/hosted lifecycle, crash/power-loss durability, and wider concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | `CARGO=./scripts/cargo-shared MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node --experimental-strip-types scripts/check-http-parity.mjs` passed the pinned TypeScript/Rust loopback differential: 40 paired S3+WebDAV cases, including 16 authenticated WebDAV cases for streaming PUT, XML property update, GET/HEAD/range/conditional behavior, PROPFIND, COPY/MOVE, refusal, missing-resource, and DELETE | — | 72% W01.1 planning view | This is local pinned-oracle HTTP evidence; broader N-API member differential, provider/native/hosted lifecycle, crash/power-loss durability, and wider network/native concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | At exact scope-packet SHA `e13c52bea3485fada8be03ed62fba9a107255507`, [CI run 35640746296](https://github.com/andymac4182/mount-rs/actions/runs/35640746296) reported successful [macOS native-WebDAV job 106469172312](https://github.com/andymac4182/mount-rs/actions/runs/35640746296/job/106469172312) and [Ubuntu native-WebDAV job 106469172419](https://github.com/andymac4182/mount-rs/actions/runs/35640746296/job/106469172419); the ignored native harness completed hosted macOS/Linux WebDAV I/O | Hosted native I/O is evidenced for this packet, but the overall CI run was still in progress and hosted session/member, provider, crash/power-loss durability, and wider concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | From `integrations/mount-rs-napi`, `node test/typecheck.mjs && node test/webdav-sqlite.mjs` passed the SQLite-backed WebDAV provider/reopen probe: exact PUT bytes survived server/provider shutdown and replacement-session lock count was zero | — | 72% W01.1 planning view | This is local SQLite persistence and process-local lock classification only; crash/power-loss recovery, live-provider behavior, hosted session/member lifecycle, and wider concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | From `integrations/mount-rs-napi`, `node test/typecheck.mjs && node test/webdav-node-fs.mjs` passed the rooted NodeFs-backed WebDAV provider/reopen probe: exact PUT bytes survived server/provider shutdown and replacement-session lock count was zero | — | 72% W01.1 planning view | This is local NodeFs persistence and process-local lock classification only; power-loss ordering, live-provider behavior, hosted session/member lifecycle, and wider concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The explicit ignored native WebDAV harness passed 1/1 on this macOS arm64 host using `/sbin/mount_webdav`, completing native mount, file I/O, and cleanup; this is local host evidence rather than hosted macOS/Linux acceptance | — | 72% W01.1 planning view | Hosted macOS/Linux, provider qualification, crash/power-loss durability, broader network/native concurrency, and full session/member parity remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only status for published tip `9e8e4592cd8d4fe5b42c2734621ac1cd1bce02b5`: [CI run 35631845088](https://github.com/andymac4182/mount-rs/actions/runs/35631845088) and [fault-injection run 35631845044](https://github.com/andymac4182/mount-rs/actions/runs/35631845044) were cancelled, and [Live Cloudflare R2 run 35631845090](https://github.com/andymac4182/mount-rs/actions/runs/35631845090) failed | — | 72% W01.1 planning view | No hosted WebDAV PASS is claimable; hosted/native lifecycle, provider, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only status for current docs-only tip `f76a637fdc6d62f400b75505579628facb3cc871`: [CI run 35633305914](https://github.com/andymac4182/mount-rs/actions/runs/35633305914) and [fault-injection run 35633305962](https://github.com/andymac4182/mount-rs/actions/runs/35633305962) were cancelled; the unrelated [W04 production-policy run 35633305932](https://github.com/andymac4182/mount-rs/actions/runs/35633305932) succeeded, and no fresh Live Cloudflare R2 run was listed | — | 72% W01.1 planning view | No hosted WebDAV PASS is claimable from the current tip; hosted/native lifecycle, provider, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only status for tracker tip `4b103b1ab9be142b15638a9679999bfd43d3bd80`: [CI run 35633547228](https://github.com/andymac4182/mount-rs/actions/runs/35633547228) was pending and [fault-injection run 35633547086](https://github.com/andymac4182/mount-rs/actions/runs/35633547086) was in progress; the unrelated [W04 production-policy run 35633547306](https://github.com/andymac4182/mount-rs/actions/runs/35633547306) succeeded | — | 72% W01.1 planning view | Pending/in-progress workflows are not hosted WebDAV acceptance; no WebDAV PASS is claimable and the hosted/native lifecycle, provider, crash/power-loss durability, and broader concurrency gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Hardened the Rust HTTP server's bounded-drain state: after a stalled partial `PUT` times out, the server aborts the tracked connection task, keeps relisten blocked in its terminal closing state, and allows a retrying close to complete; the focused host-enabled WebDAV target passed 18/18, strict WebDAV Clippy and formatting passed | — | 72% W01.1 planning view | This is Rust transport evidence only; hosted network concurrency, crash/power-loss restart, provider durability, and full session/member parity remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Extended the shared JavaScript server lifecycle facade to clear a failed close promise for retry while preserving in-flight close caching; the rebuilt release addon passed generated typecheck, the 40-iteration N-API lifecycle race, the stalled partial-PUT timeout/forced-cancellation/peer-exit/retry regression, the WebDAV-only network/fault/restart phase, the SQLite reopen probe, and the shutdown-lifecycle regression | — | 72% W01.1 planning view | This qualifies orderly N-API close-timeout retry and forced connection cancellation only; hosted network concurrency, crash/power-loss restart, provider durability, and full session/member parity remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added the supported-scope N-API WebDAV session/server differential against the pinned TypeScript oracle: effective credentials/lock/session options, driver and lifecycle members, Map-shaped counters, supported direct methods, recursive owner lock snapshots, and request/reply/error/assertion counts all matched; `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/webdav-session-parity.mjs` passed | — | 72% W01.1 planning view | The injectable `now`, `onAssertion`, and live `DavLockTable` remain outside supported N-API scope; hosted/provider/session lifecycle, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Extended the pinned N-API session differential to the full supported WebDAV class 1/2/3 direct-method matrix: `MKCOL`, `PROPFIND`, `PROPPATCH`, `COPY`, `MOVE`, and `DELETE` now pair against the TypeScript session, with date-normalized XML and byte-for-byte COPY/MOVE destination plus DELETE-missing side-effect checks; `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/webdav-session-parity.mjs` passed | — | 72% W01.1 planning view | The injectable `now`, `onAssertion`, and live `DavLockTable` remain outside supported N-API scope; hosted/provider/session lifecycle, crash/power-loss durability, and broader concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Hardened remote WebDAV directory and recursive-copy failure boundaries: `Depth: 1` PROPFIND now requests `FsDriver::readdir_bounded` with a fixed 4,096-entry ceiling and maps `EOVERFLOW` to `413 Connection: close`; COPY now reports child-stat failures and rejects premature or over-reported source reads instead of silently returning an incomplete successful tree. The focused WebDAV target passed 24/24, all package targets passed with the native mount test ignored, strict WebDAV Clippy, formatting, and `git diff --check` passed | — | 72% W01.1 planning view | This closes a local bounded-materialization/copy-correctness gap only; provider/native/hosted lifecycle, live-provider, crash/power-loss durability, durable-lock, and broader ordering gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Hardened the Basic authentication grammar at the HTTP boundary: the scheme now requires the oracle/RFC separator before canonical base64, and the live auth test rejects `Basic<base64>` while accepting the configured pair; the focused WebDAV target passed 24/24, all package targets, strict WebDAV Clippy, formatting, and `git diff --check` passed | — | 72% W01.1 planning view | This closes a local auth-parser boundary only; provider/native/hosted lifecycle, live-provider, crash/power-loss durability, durable-lock, and broader ordering gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Hardened listener lifecycle recovery: `listen()` now distinguishes a finished accept-loop task from a running listener, so a subsequent serialized bind can recover after an accept failure instead of returning a false success; focused task-state regressions cover finished and pending Tokio handles | — | 72% W01.1 planning view | This closes a local listener-state edge only; deterministic socket-level accept-failure injection, provider/native/hosted lifecycle, live-provider, crash/power-loss durability, durable-lock, and broader ordering gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only status for published packet `8e23ca06`: exact-tip CI run `35682524510` was cancelled before any jobs materialized, so no hosted WebDAV result is claimable from that packet; prior terminal hosted native runs remain the latest accepted hosted evidence | — | 72% W01.1 planning view | Hosted WebDAV evidence did not advance at this tip; provider/native lifecycle, live-provider, crash/power-loss durability, durable-lock, and broader ordering gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Preserved duplicate request fields at the Rust HTTP and N-API WebDAV boundaries instead of silently overwriting them; duplicate `If` lines are joined with grammar-safe whitespace, and a raw loopback regression with one true plus one false state list returned `200` with the exact body | — | 72% W01.1 planning view | This closes a local request-head normalization gap only; hosted/provider lifecycle, live-provider, crash/power-loss durability, durable-lock, and broader ordering gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Bounded recursive WebDAV mutation traversal: recursive `COPY` and `DELETE` now use the driver-enforced 4,096-entry directory seam instead of unbounded `readdir`; overflow or unsupported enumeration returns a per-resource `207 Multi-Status` failure without traversing that directory. The focused regression verifies COPY leaves only its created destination and DELETE leaves the source intact on `EOVERFLOW` | — | 72% W01.1 planning view | This closes a local recursive-materialization boundary only; drivers without `readdir_bounded`, live-provider qualification, hosted lifecycle/concurrency, power-loss ordering, durable locks, crash/power-loss restart, and broader ordering remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Current exact-tip audit for published packet `baf19664`: CI run `35685409287` was cancelled with no jobs, so no hosted WebDAV result is claimable; Live Cloudflare R2 run `35685409328` failed its protected usage admission with `count=297 limit=20` and skipped the live integration job | — | 72% W01.1 planning view | The R2 usage envelope and live AWS/provider configuration remain external blockers; hosted WebDAV lifecycle/concurrency, power-loss ordering, durable locks, crash/power-loss restart, and broader ordering remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only status for published packet `d17e5583`: [CI run 35662093517](https://github.com/andymac4182/mount-rs/actions/runs/35662093517) was pending, [fault-injection run 35662093477](https://github.com/andymac4182/mount-rs/actions/runs/35662093477) was queued, and [Live Cloudflare R2 run 35662093429](https://github.com/andymac4182/mount-rs/actions/runs/35662093429) failed at the bounded monthly-usage preflight while its actual integration job was skipped | No hosted WebDAV PASS is claimable from this tip; the R2 preflight is an external provider-usage blocker and hosted WebDAV lifecycle/concurrency, provider, and durability gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Increased the local N-API network concurrency slice to 64 concurrent HTTP `PUT`s and 64 concurrent `GET`s; `node test/typecheck.mjs && node test/webdav-network-concurrency.mjs` passed with exact bodies, 65 PUT/GET method counters, streamed PUT/GET, Basic-auth challenge/acceptance, and one exact-once live `PATCH` error callback | — | 72% W01.1 planning view | This is local host-enabled network-client evidence only; hosted concurrency, power-loss/live-provider durability, wider ordering, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Widened the dedicated local N-API direct-session concurrency probe: `node test/typecheck.mjs && node test/webdav-session-concurrency.mjs` passed 64 concurrent `PUT`s followed by 64 concurrent `GET`s through one `WebdavSession.handleRequest`, with exact byte readback and matching 64/64 method counters | — | 72% W01.1 planning view | This is in-process same-driver evidence only; hosted/network-client concurrency, power-loss/live-provider durability, wider ordering, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added a NodeFs process-crash/reopen classification: `node test/typecheck.mjs && node test/webdav-node-fs-crash.mjs` passed exact byte readback after forced child termination and a replacement-session zero-lock check | — | 72% W01.1 planning view | This is local NodeFs process-crash recovery and process-local lock evidence only; power-loss ordering, live-provider behavior, durable locks, hosted lifecycle, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added the WebDAV durable-mutation barrier: successful PUT, MKCOL, PROPPATCH, COPY, MOVE, DELETE, and resource creation by LOCK now await `FsDriver::syncfs()` when `Capabilities::durable_writes` is set; the focused Rust test also injects a barrier failure and verifies a retry, while the host-enabled locked WebDAV target passed 20/20, strict warning-denied Clippy, and formatting | — | 72% W01.1 planning view | This is local transport barrier dispatch/error evidence only; provider-specific power-loss ordering, durable locks, live-provider behavior, hosted lifecycle/concurrency, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Rebuilt the release N-API addon from the barrier-enabled tree and requalified the WebDAV-only surface: generated typecheck, host-enabled server phase, lifecycle, 64-pair direct-session and network/auth/streaming probes, NodeFs/SQLite reopen, provider direct/network concurrency, and NodeFs/SQLite crash/in-flight streamed-PUT recovery all passed | — | 72% W01.1 planning view | This is local rebuilt N-API/provider evidence only; hosted session/lifecycle/concurrency, live remote-provider behavior, power-loss ordering, durable locks, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only status for final ledger tip `3148aa5a`: exact-SHA CI `35673381803` and W08 policy/targets `35673381898`/`35673381797` were pending, Fault injection `35673381853` and W04 policy `35673381814` were queued, and no Live AWS S3 or Live Cloudflare R2 run was listed | — | 72% W01.1 planning view | No hosted WebDAV PASS is claimable from the final published tip; hosted lifecycle/concurrency, live-provider behavior, power-loss durability, durable locks, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only status for published durable-barrier packet `4e19f226`: exact-SHA CI `35672738319` and W08 release targets/policy `35672738309`/`35672738333` were pending, Fault injection `35672738370` and Live AWS S3 `35672738306` were in progress, and Live Cloudflare R2 `35672738332` failed; W04 policy succeeded and unrelated Native 9P was in progress | — | 72% W01.1 planning view | No hosted WebDAV PASS is claimable from this snapshot; hosted WebDAV lifecycle/concurrency, live-provider behavior, power-loss durability, durable locks, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | At implementation packet `4bc10ad1`, the shared-target Rust gate `./scripts/cargo-shared test -p mount-rs-webdav --test webdav --locked` passed 18/18 and warning-denied `./scripts/cargo-shared clippy -p mount-rs-webdav --tests --locked -- -D warnings` passed; the native mount probe remains ignored | — | 72% W01.1 planning view | This refresh confirms the local Rust transport gate only; hosted/provider/session lifecycle, power-loss durability, broader concurrency, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | At exact packet `22f9169bbc90c6887bb1bddafcde5795cd7098d1`, the shared-target Rust WebDAV gate passed 18/18, strict warning-denied Clippy passed, and formatting passed; the native mount probe remains ignored | — | 72% W01.1 planning view | This confirms the local Rust transport gate at the NodeFs crash/reopen packet only; hosted/provider/session lifecycle, power-loss durability, broader concurrency, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | The supported callable/member assertion refresh and synchronized ledgers were published in packet `8f0e74138286a678cbc5868d3cc4a528fb1b9fe9`; exact-SHA [CI run 35664334842](https://github.com/andymac4182/mount-rs/actions/runs/35664334842), [W08 release targets run 35664335056](https://github.com/andymac4182/mount-rs/actions/runs/35664335056), and [W08 release policy run 35664334771](https://github.com/andymac4182/mount-rs/actions/runs/35664334771) were pending, while [W04 production policy run 35664334817](https://github.com/andymac4182/mount-rs/actions/runs/35664334817), [Live Cloudflare R2 run 35664334855](https://github.com/andymac4182/mount-rs/actions/runs/35664334855), and [Fault injection run 35664334861](https://github.com/andymac4182/mount-rs/actions/runs/35664334861) were queued | No hosted WebDAV PASS is claimable from this packet; W01 remains NO-GO pending terminal hosted/provider/session lifecycle, durability, and broader concurrency evidence |
| 2026-09-22 | W01-WebDAV | The 64-pair implementation packet `d391f9b798df455311177f462ab160736ed3ba4c` reached no terminal hosted WebDAV result: exact-SHA [CI run 35665105286](https://github.com/andymac4182/mount-rs/actions/runs/35665105286), [W08 release policy run 35665105293](https://github.com/andymac4182/mount-rs/actions/runs/35665105293), [W08 release targets run 35665105417](https://github.com/andymac4182/mount-rs/actions/runs/35665105417), [Fault injection run 35665105186](https://github.com/andymac4182/mount-rs/actions/runs/35665105186), and [W04 production policy run 35665105441](https://github.com/andymac4182/mount-rs/actions/runs/35665105441) were cancelled by subsequent mainline publication; [Live Cloudflare R2 run 35665105299](https://github.com/andymac4182/mount-rs/actions/runs/35665105299) remained queued | The widened local concurrency gate passes, but no hosted WebDAV PASS is claimable; W01 remains NO-GO pending terminal hosted/provider/session lifecycle, durability, and broader concurrency evidence |
| 2026-09-22 | W01-WebDAV | Added full supported server/session prototype-member and public-symbol differential coverage plus deterministic XML ETag normalization; `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/webdav-session-parity.mjs` passed three repeated times, enumerating the pinned oracle's server/session prototype surface while allowing only the supported native `handleRequestStream`/`lockCount` additions and internal wrapper-symbol filters | The three oracle-only controls (`now`, `onAssertion`, live `DavLockTable`) remain outside supported scope; external lifecycle/provider/durability/wider concurrency gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Expanded the bounded local concurrency probes to `MOUNT_RS_WEBDAV_CONCURRENCY=128`; direct-session and host-enabled loopback network WebDAV probes each passed three repeated times with exact PUT/GET byte readback, requested method counts, streaming, auth, and exact-once request-error checks | This is local same-driver and host-enabled loopback evidence only; hosted concurrency, power-loss/live-provider durability, wider ordering, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Expanded the bounded local concurrency probes to the configured ceiling `MOUNT_RS_WEBDAV_CONCURRENCY=256`; direct-session and host-enabled loopback network WebDAV probes each passed three repeated times with exact PUT/GET byte readback, requested method counts, streaming, auth, and exact-once request-error checks | This is local same-driver and host-enabled loopback evidence only; hosted concurrency, power-loss/live-provider durability, wider ordering, and production acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Read-only hosted status for published 256-request packet `efd6ed33`: exact-SHA CI run `35669390058`, W08 release targets `35669390013`, and W08 release policy `35669389968` were pending; Fault injection `35669390039` and W04 production policy `35669390028` were queued, with no Live Cloudflare R2 run listed | No hosted WebDAV PASS is claimable from this packet; W01 remains NO-GO pending terminal hosted/provider/session lifecycle, crash/power-loss durability, and hosted concurrency evidence |
| 2026-09-22 | W01-WebDAV | Read-only status for published WebDAV implementation packet `5c6716a2`: exact-SHA [CI run 35667576642](https://github.com/andymac4182/mount-rs/actions/runs/35667576642), [W08 release targets run 35667576687](https://github.com/andymac4182/mount-rs/actions/runs/35667576687), [Fault injection run 35667576587](https://github.com/andymac4182/mount-rs/actions/runs/35667576587), and [W08 release policy run 35667576738](https://github.com/andymac4182/mount-rs/actions/runs/35667576738) were pending; [Live Cloudflare R2 run 35667576677](https://github.com/andymac4182/mount-rs/actions/runs/35667576677) and [W04 production policy run 35667576848](https://github.com/andymac4182/mount-rs/actions/runs/35667576848) were queued | No hosted WebDAV PASS is claimable from this implementation packet; W01 remains NO-GO pending terminal hosted/provider/session lifecycle, durability, and wider concurrency evidence |
| 2026-09-22 | W01-S3 | Closed the applicable S3 N-API member and peer-fault packet: `S3Server.connections`, `onTransportError`, safe effective session options, session-owned bucket wrappers, and debug-gated assertions are generated and exercised; Rust passed 4 unit, 6 chunked, 18 gateway, and 5 public-API tests, N-API library tests passed 16/16, release build/typecheck/strict Clippy/formatting passed, and host-enabled `node test/servers.mjs` passed one typed peer-aware callback after a direct Node reset | — | 78% W01.1 planning view | Live AWS/R2, complete oracle-specific S3 member/codec parity, restart/durability/concurrency matrices, and native/hosted lifecycle gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01.1 / W01.4 | The complete S3 packet was reapplied onto the moving mainline and finally pushed fast-forward-only at `1675c50a` (member-parity `47b21c2` plus timeout follow-up `1675c50a`). On the exact promotion tree, shared S3/N-API check, warning-denied Clippy, release addon build, generated typecheck, and focused host smoke all passed: loopback PUT/GET, streamed PUT, zero assertion leakage, live-connection cleanup, and a typed peer-fault callback after a deliberate 4 MiB reset. Production remains NO-GO for live AWS/R2, complete oracle parity, restart/durability/concurrency matrices, and native/hosted lifecycle evidence | — | S3 final mainline publication | Live providers, complete oracle parity, restart/durability/concurrency, and native/hosted lifecycle gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Added oracle-backed structural-factory construction parity: host-enabled `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/structural-factories.mjs` covers empty/mixed/single-driver sources, invalid sources, seven invalid bucket names, and a non-ASCII UTF-16 boundary; the N-API facade now preserves the oracle's `TypeError` class/message, while scoped check/Clippy, release build, generated typecheck, and distribution/export checks passed | — | 78% W01.1 planning view | Live AWS/R2, complete oracle-specific codec/member parity, restart/durability/concurrency matrices, and native/hosted lifecycle gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Closed the local multipart restart/terminal-race slice | The exact gateway target passed 22/22 with loopback access: staged multipart state survived replacement-session completion, close swept both bucket roots idempotently while the session remained usable, and concurrent Complete/Abort produced only the oracle outcomes `200/404` or `404/204` with no staging debris, and an invalid Complete released its finalization claim for a correct retry; exclusive finalization claims were added for the terminal transition | Crash/power-loss durability, provider-backed acceptance, broader ordering/concurrency, live AWS/R2, complete oracle-specific codec/member parity, and native/hosted lifecycle gates remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Added failed-Complete retry release coverage | The focused `invalid_multipart_complete_releases_finalization_claim_for_retry` gateway test passed after an invalid ETag response, confirming the filesystem-visible finalization marker is released before a correct retry; this is local structural-driver evidence only | Same-process N-API evidence only; native mount, hosted CI, crash/power-loss durability, live AWS/R2, complete oracle codec/member parity, and broader ordering/concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Added multipart assembly-read fault recovery | The complete S3 target passed 4 unit, 6 chunked, 23 gateway, and 5 public-API tests; one injected `EIO` during staged-part assembly released the finalization marker and the same upload completed successfully on retry | Bounded local fault injection only; power-loss durability, provider failure, broader ordering/concurrency, live AWS/R2, and native/hosted lifecycle remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Added cancellation-safe staging and bounded multipart ordering | Streaming PUT and multipart part replacement now stage into unique private files and publish with atomic rename; aborted requests remove private staging and preserve an existing part, while concurrent two-part publication completes in numeric order. The loopback-enabled S3 target passed 4 unit, 6 chunked, 26 gateway, and 5 public-API tests; rebuilt N-API typecheck/distribution, host-enabled server integration, process restart, and Node scope checks passed | Local cancellation and bounded in-process ordering only; power-loss/torn-write durability, provider-backed acceptance, live AWS/R2, complete oracle codec/member parity, and native/hosted lifecycle remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Exercised the N-API multipart replacement-session boundary | Regenerated the release addon/declarations with `CI=true CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-s3-target pnpm build`; host-enabled `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node test/servers.mjs` passed the direct N-API replacement-session create/part/list/complete/GET flow alongside streamed traffic, cancellation, bucket isolation, connection cleanup, and one typed peer-fault callback; generated typecheck and distribution/export checks passed | Same-process N-API evidence only; native mount, hosted CI, crash/power-loss durability, live AWS/R2, complete oracle codec/member parity, and broader ordering/concurrency remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Exercised native-filesystem process-restart multipart recovery | `node test/s3-restart.mjs` passed: the child process left staged state without calling `S3Server.close()`, and a fresh native filesystem driver/session listed, completed, and read the object; this is process-restart evidence only | Crash/power-loss durability, provider-backed acceptance, broader ordering/concurrency, live AWS/R2, complete oracle-specific codec/member parity, and native/hosted lifecycle remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Added bounded direct-session concurrency and same-key conditional CAS evidence | `node test/s3-session-concurrency.mjs` passed 64 concurrent unique-object PUT/GETs across two buckets, two concurrent buffered `If-Match` PUTs with exactly one `200` and one `412`, a streamed conditional update, exact final bytes, and clean counters/assertions; `conditional_puts_serialize_compare_and_swap_checks` passed the deterministic paused-write Rust race | This is local same-session ordering evidence only; exhaustive workload bounds, live-provider conflict behavior, physical power-loss durability, and native/hosted lifecycle remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Refreshed live AWS/R2 admission evidence | AWS run [`35679010203`](https://github.com/andymac4182/mount-rs/actions/runs/35679010203) stopped at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; R2 run [`35679010292`](https://github.com/andymac4182/mount-rs/actions/runs/35679010292) stopped at `count=281 limit=20` before its live job; neither is a provider PASS | Protected AWS configuration and R2 monthly budget remain external prerequisites; no live-provider or hosted/native acceptance is claimable and W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Re-qualified the current `origin/main` tip `71972b28ca7ae561325342ddf466c8353546547a`: release N-API build, the four-file pinned upstream oracle suite (990 passed/79 skipped), Rust S3 4/6/29/5 tests plus strict Clippy, exact Node scope/session/concurrency/restart checks, structural factory parity, package typecheck/distribution/server smoke, and the 40-case S3+WebDAV HTTP differential all passed | — | 78% W01.1 planning view | This refreshes local and pinned-oracle evidence only; live AWS/R2, physical power-loss durability, broader workload bounds, and native/hosted lifecycle remain open, so W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Closed the applicable S3 option and callback hook surface: Rust `S3SessionHooks` and N-API `now`, `requestId`, `onError`, and `onAssertion` are wired with callback keepalive/release; the release addon/declaration build, callback observability test, Rust 5/6/29/5 packet, warning-denied Clippy, package typecheck/distribution, session differential, 64-way/CAS concurrency, and process-restart recovery all passed | — | 78% W01.1 planning view | This closes the local applicable option/callback boundary only; current AWS `35679010203` is blocked by `missing_bucket`, current R2 `35680542993` is blocked at `count=285 limit=20`, and physical power-loss durability, broader workload bounds, and native/hosted acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Closed per-key `DeleteObjects` error-hook parity: Rust now forwards per-key driver failures to `S3SessionHooks.on_error` while retaining the 200 partial-result response; the focused regression and full Rust 5/6/30/5 packet, warning-denied Clippy, release N-API build, callback observability, session differential, 64-way/CAS concurrency, process restart, typecheck, and distribution checks all passed | — | 78% W01.1 planning view | This closes the local per-key diagnostic callback boundary only; AWS run `35683716247` is blocked by `missing_bucket`, R2 run `35683716251` is blocked at `count=294 limit=20`, and physical power-loss durability, broader workload bounds, and native/hosted acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Closed S3 session-close diagnostic parity: Rust `S3Session.close()` now reports cleanup failures through `onError(error, undefined)` without rejecting, while the focused regression and full Rust 5/6/31/5 packet, warning-denied Clippy, release N-API build, callback observability, session differential, 64-way/CAS concurrency, process restart, typecheck, and distribution checks passed | — | 78% W01.1 planning view | This closes the local cleanup/error-head boundary only; AWS run `35684677320` remains blocked by `missing_bucket`, R2 run `35684677273` remains blocked at `count=295 limit=20`, and physical power-loss durability, broader workload bounds, and native/hosted acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Closed bounded S3 server-close drain timeout: Rust `S3Server.close()` now cancels tracked connections and aborts the detached Axum task when the drain deadline expires, resolving without leaving a live connection behind; the focused regression and full Rust 5/6/32/5 packet, strict Clippy, release N-API build, host-enabled server integration, callback observability, session differential, 64-way/CAS concurrency, process restart, typecheck, and distribution checks passed | — | 78% W01.1 planning view | This closes the local bounded server-close/connection-lifecycle boundary only; live AWS/R2, physical power-loss/torn-write durability, broader workload bounds, and native/hosted acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01-S3 | Refreshed live provider admission on published `5e910e80`: AWS run `35686512809` stopped at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; R2 run `35686512842` stopped at `count=302 limit=20` before live integration | — | 78% W01.1 planning view | No live AWS/R2 service PASS is claimable; protected AWS configuration, R2 budget reset, physical power-loss durability, broader workload bounds, and native/hosted acceptance remain open; W01 stays NO-GO |
| 2026-09-22 | W01.2 | Re-ran the pinned upstream conformance suite on exact `origin/main` `b680ee4b` with the release N-API addon rebuilt from that tree; all four test files passed with 990 tests passed and 79 explicit skips against mountx oracle `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` | — | 65% W01.2 evidence refreshed | The 79 skip rows remain the reviewed capability/protocol and environment boundaries; PGlite, live R2, root-only, hosted-platform, and native-mount gates remain separate and W01 stays NO-GO |
| 2026-09-22 | W01.2 / W01.3 | Ran `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 ./scripts/test-pglite.sh` on tested revision `57ade44a4ac59899f3ff561075701046d94564ca`: PGlite server cleanup/slot, backend parity, reconnect/versioned/SQLite-VFS/split-store/lifecycle, backup/restore, FUSE reconnect, Rust SDK 6/6, Node SDK 5/5, CLI 12 pass/2 skip, the PGlite-enabled upstream suite (1,200 passed/82 skipped), and all 40 seeded trace lanes (621 operations each) passed | — | 65% W01.2 / 75% W01.3 evidence refreshed | R2 and TiDB/RustFS rows remained explicit credential-gated skips; root-only, hosted platform/native transport, live-provider, and other external gates remain open, so W01 stays NO-GO |
| 2026-09-22 | W01.1 / W01.4 | Ran the exact memory-backed macOS CLI native lifecycle on mainline `33d1d1c1991547b4fd0122013755288a61afb2d6`: the latest CLI build passed, auto selected NFS, the real kernel mount appeared at a disposable user-owned path, mounted-path write/read passed, Ctrl-C unmounted cleanly, and post-stop mount/process checks passed before removing the empty mountpoint | — | CLI memory-backed macOS native evidence closed | This closes the exact memory-driver CLI mount gap; Linux FUSE/9P, FSKit/macFUSE, live providers, hosted aggregate release, crash/power-loss durability, and broader W01 gates remain open, so W01 stays NO-GO |

| 2026-09-22 | W01-WebDAV | Added optional structural N-API `FsDriver.readdirBounded(path, maxEntries)` forwarding. The rebuilt release addon, generated typecheck, WebDAV-only host-enabled server phase, and structural-driver regression passed bounded `Depth: 1` PROPFIND, recursive collection COPY/DELETE, provider-reported `EOVERFLOW`, adapter rejection of an over-large callback result, and the absent-callback `501` boundary | — | 77% W01.1 planning view | This qualifies the local structural N-API capability seam only; hosted package/provider qualification, live-provider behavior, power-loss ordering, durable locks, crash/power-loss restart, and the explicit same-resource ordering boundary remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Hardened unread request-body fault handling: non-recoverable drain errors are reported once and add `Connection: close`, including when dispatch already returned a response, while known 413 size-limit faults continue to drain so HTTP/1.1 framing remains reusable; the focused WebDAV target passed 27/27, strict Clippy, formatting, rebuilt N-API addon, typecheck, WebDAV-only host-enabled integration, and structural WebDAV regression | — | 77% W01.1 planning view | This closes a local body-fault/framing boundary only; hosted package/provider qualification, live-provider behavior, power-loss ordering, durable locks, crash/power-loss restart, and the explicit same-resource ordering boundary remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Added native loopback evidence for streamed response read faults: a driver-reported short file causes the HTTP response body to fail after `200` headers, and exactly one peer-qualified `Connection` transport fault is observed; the focused WebDAV target passed 28/28 with strict Clippy and formatting | — | 77% W01.1 planning view | This confirms local response-stream fault propagation only; hosted lifecycle/provider qualification, live-provider behavior, power-loss ordering, durable locks, crash/power-loss restart, and the explicit same-resource ordering boundary remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Clarified the public body-stream contract so only known 413 limit faults are drainable for HTTP/1.1 reuse; non-recoverable body faults are a connection-close boundary, matching the published implementation and its 28/28 WebDAV evidence | — | 77% W01.1 planning view | Documentation alignment only; hosted lifecycle/provider qualification, live-provider behavior, power-loss ordering, durable locks, crash/power-loss restart, and the explicit same-resource ordering boundary remain open; W01 stays NO-GO |
| 2026-09-22 | W01-WebDAV | Exact-tip hosted/provider audit after body-stream contract publication: CI run `35687955166` for `c42030c1807f6504660892bf829137897e910c5e` was cancelled with no jobs; protected Live Cloudflare R2 run `35687955189` failed usage admission at `count=309 limit=20` and skipped its live integration job, before mainline advanced to `594ad797a67c77daa5d504042d15353da925688a` | — | 77% W01.1 planning view | No hosted WebDAV PASS is claimable from this tip; the R2 usage cap, live AWS/provider configuration, power-loss ordering, durable locks, crash/power-loss restart, and the explicit same-resource ordering boundary remain open; W01 stays NO-GO |

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
