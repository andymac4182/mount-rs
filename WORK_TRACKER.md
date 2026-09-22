# Workstream and task tracker

Updated: 2026-09-22. Baseline: local commit `21803fd` plus the sequentially
published `main` updates listed below. Overall status: **in progress;
not release-ready**.

Current W26 exact-head replacement qualification boundary (2026-09-22): run
`35707725455 <https://github.com/andymac4182/mount-rs/actions/runs/35707725455>`
targeted source/lockfile SHA `1ceaa96486a96ed4288c079dcde2b3b2d18900bb` after
the standalone Ozone lockfile correction. Base job `106680595638` passed.
The producer packet completed the required functional/restart/cleanup markers,
but the hard per-drive performance gate is currently `1/4`: PGlite/R2 passed
at `1340.137102` IOPS; SQLite/R2 failed at `781.576517`, TiDB/R2 at
`492.510872`, and FoundationDB/R2 at `306.794117`. The three failures are
hard IOPS failures with complete lifecycle metrics, not skipped or missing
tests. Aggregate job `106685891954` completed with a fail-closed
`W26_OZONE_EVIDENCE_PACKET_FAIL` because the compositions log lacked
`OZONE_IOPS_PASS` for the SQLite/PGlite provider set; no aggregate result is
promoted. The latest docs-only mainline tip is
`58fc18c0a3d62dc92108a2be4efff81eb023950a`; it does not alter the tested
source/lockfile SHA. Production remains **NO-GO** until all providers pass the
1,000-IOPS/drive gate and the complete end-to-end, security, Tier-1 99.99%
SLO, five-minute RPO/RTO and customer-owned backup/DR evidence is satisfied.
W26 continues to own Ozone compatibility and qualification only; customers
deploy Ozone, backup/DR remains with Ozone/customer ownership, and releases
remain with the separate stream.

Current W26 FoundationDB publication read-overlap boundary (2026-09-22):
source commit `fba61979f1f6c9858026cd5ebc4c5d3d357f366b`
(`perf(w26): overlap FoundationDB publication reads`) is now on `origin/main`.
The hot metadata publication still uses one
transaction/read version, lease/fence predicate, revision CAS, shared
authority time and fail-closed ambiguous-commit handling; the lease,
manifest and authority reads are only polled concurrently. The direct
`futures-util` dependency and root lockfile entry are included in that source
chunk. Formatting, diff checks, FoundationDB feature lib/test-target checks,
strict Clippy and portable feature-off tests pass; focused native tests reach
only the missing `fdb_c` linker gate. Security scan
`26c525f1-03bc-4a3a-9c91-b7577eab8316` has complete changed-file coverage
and zero findings. The prior run `35712159705` tested the ancestor
`ff0ccfdd`, not `fba61979`, so a fresh exact-head Ozone run is required before
promoting any performance result. Production remains **NO-GO** until the new
source passes all feasible provider rows at the hard 1,000-IOPS/drive target,
the aggregate contains every end-to-end marker, and customer/Ozone security,
Tier-1 99.99% SLO, five-minute RPO/RTO and backup/DR evidence are closed.
W26 continues to own compatibility and qualification only; customers deploy
Ozone, backup/DR remains Ozone/customer owned, and releases remain with the
separate stream.

Fresh exact-head qualification run `35715790619
<https://github.com/andymac4182/mount-rs/actions/runs/35715790619>` is now
queued from `main` at workflow head
`af7e73dfb7f54a31c1a91be829238571950a48e1`; source
`fba61979f1f6c9858026cd5ebc4c5d3d357f366b` is an ancestor and therefore
included. Its SQLite, PGlite, TiDB, FoundationDB and aggregate jobs have no
terminal result yet, so no performance or end-to-end result is promoted. At
the 20:41 AEST poll the four provider jobs were still queued; a 20:47 AEST
queue inventory showed nine complete `ci.yml` workflows queued since 10:01
UTC. This is a CI-capacity blocker, not provider evidence. The manual run
remains the authoritative non-canceling exact-head gate despite newer
unrelated push runs. A later manual descendant run `35716722852` has head
`32b85965deef60c84d90f5fb353f45fb6001a303`, locally verified to include
`fba61979`, and is also queued. Automatic descendant run `35717708439` is
rejected because its four provider jobs were canceled by push concurrency.

Current W26 FoundationDB optimization boundary (2026-09-22): source commit
`ff0ccfddcad786fdce142adebc16fa50347b9b13`
(`perf(w26): avoid redundant FoundationDB chunk clears`) is now on
`origin/main` after rebase. It preserves the lease/revision and manifest
transaction controls, skips the redundant full chunk-range clear when the
chunk count is unchanged, and clears only stale trailing chunks when a
manifest shrinks; missing-manifest initialization still clears the full
prefix. Formatting, diff checks, FoundationDB feature lib/test compilation
and strict Clippy pass. The native FoundationDB test is blocked only by the
missing `fdb_c` linker library. Security scan
`fa9447cc-4cff-4680-baef-d7f7c260d8c5` has complete one-file coverage and zero
reportable findings. Fresh exact-head Ozone qualification run
`35712159705 <https://github.com/andymac4182/mount-rs/actions/runs/35712159705>`
was dispatched against this SHA at workflow head
`844f4032c857bc5033ab04fd0f853b3f66a25ab2`, which contains the source commit
as an ancestor. Its FoundationDB artifact `10688630886` is functionally clean
with `400/400` successful lifecycles, zero timeout/cleanup failures and all
restart/authority markers, but records `458.266944` IOPS over
`2618.561118 ms`; the TiDB artifact `10688255611` is likewise functionally
clean with `400/400` successful lifecycles, zero timeout/cleanup failures and
fencing/ambiguity markers, but records `421.067715` IOPS over
`2849.897909 ms`. Both miss the hard target, and the full workflow remains
non-terminal while SQLite/PGlite, base and aggregate jobs are pending; these
producer artifacts are not an aggregate pass. Production remains **NO-GO**
until all feasible provider rows pass the hard 1,000-IOPS/drive target, the
aggregate has every end-to-end marker, and customer/Ozone security, Tier-1
99.99% SLO, five-minute RPO/RTO and backup/DR evidence are closed. W26
continues to own compatibility and qualification only; customers deploy Ozone,
backup/DR remains Ozone/customer owned, and releases remain with the separate
stream.

Current W26 CI-reproducibility boundary (2026-09-22): lockfile fix commit
`1ceaa96486a96ed4288c079dcde2b3b2d18900bb`
(`fix(w26): sync Ozone test lockfile`) is published and verified on
`origin/main`. The PGlite dependency addition had left the standalone
`tests/ozone/Cargo.lock` missing its already-resolved `md-5 0.10.6` edge;
exact-head TiDB job `106676248986` and FoundationDB job `106676249267` in run
`35706390612` consequently failed under `--locked` before provider tests.
The bounded correction is security-scanned by
`f2ba64e6-373d-4741-b27f-60bdcec6d7e6` with complete lockfile coverage and zero
reportable findings. Locked standalone Ozone metadata and
`./scripts/cargo-shared check --manifest-path tests/ozone/Cargo.toml
--all-targets --locked` now pass. Replacement run
`35707725455 <https://github.com/andymac4182/mount-rs/actions/runs/35707725455>`
targets exact SHA `1ceaa964`; its base `106680595638`, compositions
`106680595812`, FoundationDB `106680595831` and TiDB `106680595912` jobs were
queued at capture. Production remains **NO-GO** until that packet is terminal
and all provider, end-to-end, security, 99.99% SLO, five-minute RPO/RTO and
customer-owned backup/DR gates are separately satisfied.

Current W26 PGlite implementation boundary (2026-09-22): commit
`c791ab318af4c5253dbe3e9e4d1d2f7a2026fa4c`
(`perf(w26): compute PGlite block IDs locally`) is published and verified on
`origin/main`. PGlite now computes the exact existing PostgreSQL
`md5(encode($1, 'hex'))` identity locally, removing one provider round trip on
the unique-block put path while retaining conditional insert, duplicate
read-back, byte-for-byte collision, volume-scope and fail-closed disappearance
checks. Formatting, diff checks and locked workspace metadata passed. Focused
compile, library-test and strict-Clippy commands reached the native macOS
linker and were blocked at exit 69 because this host has not accepted the
Xcode license; this remains an explicit native-host gate, not a project pass.
Security scan `2b2a12cd-0984-4dfb-9d89-2139055afb3d` sealed complete changed-
file coverage over the two PGlite surfaces with zero reportable findings; the
captured snapshot digest is
`codex-security-snapshot/v1:sha256:d5536ab709e2b857bb5190686ba0056cd14ce798da886616241aa9a68a332833`.
Exact-head manual W26 run
`35706390612 <https://github.com/andymac4182/mount-rs/actions/runs/35706390612>`
targets `c791ab31`; at capture its base `106676249167`, compositions
`106676249209`, TiDB `106676248986` and FoundationDB `106676249267` jobs were
queued, so no result is promoted. The latest terminal packet remains 1/4
providers above the hard 1,000-IOPS/drive target. Production remains
**NO-GO**: W26 owns Ozone compatibility and qualification, while customer
deployment, security/SLO/RPO/RTO evidence, backup/DR and the separate release
stream remain explicit external or cross-workstream gates.

Current W26 TiDB implementation boundary (2026-09-22): commit
`2ce6f753a588628593baf1000ae75330780dabd3`
(`perf(w26): skip TiDB block readback on confirmed insert`) is published on
`origin/main`. The TiDB block store now returns immediately after an
unambiguous one-row insert acknowledgement, while duplicate/no-op and
provider-ambiguous results still drain the statement and read back bytes for
the existing collision/disappearance checks. Formatting, diff checks,
compile-only all-targets validation and strict TiDB Clippy passed, and the
focused library suite passed 8/8. A fresh all-targets test attempt was blocked
at macOS linking because the Xcode license is not accepted; this is a native
host gate, not a suppressed test failure. Security scan
`b2761b0b-aa05-4f49-a52a-3c5b7f09cad9` completed with complete TiDB-surface
coverage and zero reportable findings; its pre-publication snapshot report
remains evidence until a matching exact-head packet is sealed. The prior
terminal W26 result remains 1/4 providers above the 1,000-IOPS/drive target,
and run `35702188498` is stale/nonterminal for this chunk (FoundationDB job
`106662583211` failed; base `106662583505`, compositions `106662583710` and
TiDB `106662583344` were queued at the latest capture). The next action is a
fresh exact-head four-provider run and terminal aggregate. Production remains
**NO-GO**: W26 owns Ozone compatibility/qualification, while customer
deployment, backup/DR, Tier-1 SLO evidence and the separate release stream
remain explicit external gates.

Current W26 implementation/qualification boundary (2026-09-22): the latest
shared build-on tip before this tracker update is
`origin/main=2dab2386ac86df1256299e0052f81319d1164130`, which contains the
published adaptive mutation coordinator chunk `fe4a6bbf`
(`perf(w26): adapt mutation batching to concurrent waves`). It keeps the
eight-round local fast path, extends only while the queue is growing, targets
the 64-worker qualification wave, and caps collection at 64 rounds/requests;
the 64-way create and replace regressions each prove one fenced publication.
Full locked Rust tests, strict workspace Clippy, the shared-target debug N-API
build, and the complete pinned-oracle N-API suite passed. A local split-SQLite
diagnostic completed 400/400 lifecycles with zero timeout/cleanup failures but
measured `631.141356` IOPS, so it is explicitly not Ozone/R2 acceptance. The
new manual qualification is GitHub Actions run
`35702188498 <https://github.com/andymac4182/mount-rs/actions/runs/35702188498>`
on shared head `245258d9`; W26 base `106662583505`, compositions
`106662583710`, TiDB `106662583344` and FoundationDB `106662583211` were
queued at capture. The last terminal exact-SHA packet measured SQLite/R2
`1051.976655` IOPS, PGlite/R2 `837.779225`, TiDB/R2 `463.080116` and
FoundationDB/R2 `450.696578`; only SQLite passed and the aggregate failed
closed. The adaptive diff security scan `b5429807-35af-4978-83d4-ed7bcde2d6f5`
is now sealed with complete two-surface coverage and zero reportable findings
for its captured snapshot, but it warned that repository HEAD changed while
scanning; the terminal exact-head packet must still carry a matching current-
head security result. Production remains **NO-GO** until the new exact-SHA packet passes all
providers, end-to-end markers, security, Tier-1 SLO and customer-owned
backup/DR gates. W26 tracks compatibility and qualification for
customer-deployed Ozone; it does not deploy Ozone, own backup/DR or own
releases.

This is the delivery dashboard. [Requirements](REQUIREMENTS.md) define scope;
[porting evidence](PORTING_STATUS.md) and the [API parity ledger](docs/public-api-parity.md)
retain detailed results. A passing component test is not end-to-end acceptance.

Current W01-9P packet (2026-09-22): the N-API 9P facade now owns the bounded
Node `attach(stream, options)` adapter, direct session `handleCall`/`destroy`,
attached connection identity/peer/stream/closed state (including native
transport-source peer strings versus attached-stream `undefined` when no
fallback is supplied), duplicate-attach and
ownership teardown, shared byte-range lock state, and backpressure/write-fault
coverage. It now also exposes the effective scalar server/session policy and
`P9Session.userFor(fid)`, a live transport-backed `P9Session.locks` client, and
the public `P9LockTable`/`P9LockClient` surface, with generated declarations and
attach/runtime lock checks; the public Rust-backed `FidTable` alias and live
`P9Session.fids` now cover mutable path/open/iounit/cursor views, deterministic
fid ordering, qid identity/cursor helpers, detached clunk snapshots, and
retained open-handle enumeration, with focused hardlink/release and live-open
evidence. The session now also exposes its retained `Filesystem` driver,
debug-gated assertion readback/counters, and request-error/assertion callbacks
with Node error/header semantics for attached and native sessions. The server
now exposes a live property-shaped `P9Server.clients` array combining native
and attached connections. `P9ServerOptions.locks` accepts a `P9LockTable` and
shares its ranges across native and attached sessions, with live option
handles exposing that table. The bounded `./9p` mount-helper facade now
exposes Linux-client probing, refusal and option-string helpers, strict named
`mount9p` delegation, 9P live-mount filtering/cleanup, and mounted
transport/server/connection/closed views. It accepts a configured native
`P9Server` and adopts that exact listener, policy, lock table, callbacks, and
client set. Direct and automatic mount-created listeners now also receive the
bounded scalar policy and direct session `onError`/`onAssertion` callbacks from
the mount option bag; injected shared servers retain their own hooks. It
deliberately does not claim automatic cross-transport signal ownership or
unsupported native platforms. The pinned direct-option audit found no
additional unrepresented `MountP9Options` fields, and `P9Mount.source` is now
string-qualified and runtime-checked in the hosted direct mount. The pinned
runtime audit also covers all 124 constants and all 274 upstream runtime
`./9p` barrel exports, including the six public defaults. Exact SHA
`0ad4928e86af89163c8c87d08fea53ccf7f5f89b` passed hosted Linux automatic,
direct `./9p`, and structural-driver N-API mounted I/O/cleanup in [Native 9P
run `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703),
N-API job `106558367429`, with Rust job `106558367006` also passing; the
direct `./9p` facade now owns the bounded `signals` teardown option.
The local N-API member-boundary regression now records the serializable option
snapshot and attached-stream representations; the first hosted direct-mount
attempt exposed an incorrect `peer: null` expectation for Unix sockets, so a
corrected exact SHA `81cc6596c2c9562c3405df50126239a7bcb44f63` passed Native 9P
run `35670279904`, qualifying native `stream: undefined` and its
transport-source peer string.
The attached-connection wrapper packet at exact SHA
`1c791cf67861efdfe8e5da223048904c88c6b168` now implements the declared
`waitClosed()` member and awaits it in metadata teardown. Local focused 9P
checks passed; Native 9P run `35696071202` passed N-API job `106645117281`
and Rust job `106645117408`, so this packet has terminal hosted evidence.
Production remains NO-GO for the other gates.
The server/connection member-audit packet at exact SHA
`75c149f857f6056a7435a80a1493a9dfc8e53f59` adds the focused semantic member
regression and hosted workflow step. Local direct/oracle checks passed; Native
9P run `35697338227` passed N-API job `106647016617` and Rust job
`106647016767`, including the new member-surface step, so this packet has
terminal hosted evidence. Production remains NO-GO for the other gates.
The native server client-identity packet at exact SHA
`15cb940988913c666d8d592a583e7eb3d2d82241` caches native `P9Connection`
wrappers by stable transport id. Its real-TCP regression checks stable repeated
`P9Server.clients` connection/session/closed views and pruning after
`close()`/`waitClosed()`. Hosted Native 9P run `35698924766` passed N-API job
`106652302954` and Rust job `106652303250`, including the new identity step and
all four ignored native lifecycle tests. Production remains NO-GO.
The latest N-API packet normalizes the oracle's optional absence shapes at the
JavaScript boundary: pre-version `msize`/`version`, unknown `userFor(fid)`, and
conflict-free table/session `getlock()` now return `undefined`. Exact SHA
`0d520a1d0a9af44e08e65c5f0638a640bb3c08db` passed Native 9P run
`35671509538`, including the Rust job `106569412047` with all four ignored
tests and the N-API job `106569412372` with automatic, direct `./9p`, and
structural-driver mounted I/O/cleanup; the direct native assertions also
qualified the session and lock absence shapes. Production remains NO-GO for
the broader open gates.
The next helper-parity slice adds the oracle's optional platform arguments to
the direct `./9p` probe functions: `p9ClientProbe(platform?)` now produces
deterministic override facts without attempting a mount, and
`p9Platform(platform?)` maps the requested platform to `"linux" | undefined`;
the host-only zero-argument probe remains native-backed. Exact SHA
`9da45327a9e09a9f827a9630869d1a32119674e3` also passed Native 9P run
`35672845113`, with N-API job `106573050491` passing automatic/direct/structural
mounted I/O and cleanup and Rust job `106573049500` passing all four ignored
native tests; the synthetic override branches remain covered by the local
host-independent helper regression.
The `./9p` constants/message-name/default barrel is now complete against the
pinned upstream surface, with all 124 constants and all 274 runtime barrel
exports differentially checked. The transport
now also broadcasts shutdown safely
across the accept loop and all connections, closes the active-connection
accept-loop race, and
reaps completed request tasks while reporting task failures; its in-flight
permit acquisition now also observes connection/server shutdown instead of
wedging close behind a slow request. An ignored native
Linux harnesses now run eight concurrent mounted file write/read/rename/read
round trips before a bounded unmount, and close the server side while verifying
kernel-connection teardown. Session destruction also wakes and drains in-flight
`Tflush` waiters. Local lifecycle 6/6, transport-error 8/8, the focused native
target, strict 9P Clippy and formatting pass. Hosted run `35616832528` / job
`106389895603` passed the prior Linux kernel-client mount/read/write/unmount
packet; the later current run `35625437327` / native-9p job `106418844564`
passed 3/4 ignored tests but exposed a live-mount cleanup failure in the
server-close test. Corrective runs `35626340158` at `c2290b2` and
`35626765411` at `6fc9a19` were canceled before jobs materialized. The local
follow-up separates kernel unmount coordination from resource teardown and
uses cancellation-safe serialization. The dedicated [Native 9P run
`35628187344`](https://github.com/andymac4182/mount-rs/actions/runs/35628187344)
at exact SHA `431affd` passed module probing and all four ignored Linux native
tests, including the concurrent-I/O and server-close/unmount cases.
`wait_closed()` releases server resources but does not implicitly detach a live
kernel mount; callers must invoke `unmount()`.
Native accepted connections deliberately expose no Node stream because their
Tokio stream is not transferable across the N-API boundary; `attach` is the
supported Node Duplex seam. Crash/reset/half-close recovery is supervisor-owned
and not a library guarantee; overall production remains NO-GO for the remaining
W01 gates and unresolved public parity.

Current W01-NFS packet (2026-09-22): NFSv3/v4 direct routing now exposes
shared BigInt handle snapshots, live accepted-socket counts, and stable live
client objects with peer/shared-session views plus abort-safe close/wait state,
including cancellation while a queued request waits for an in-flight slot and
serialized concurrent listen/close lifecycle calls. Rust server close is
terminal, and a later `listen()` rejects rather than returning a stale address.
Connection close waiters register before checking completion, removing the
lost-wakeup interval around `wait_closed()`.
The focused Rust/N-API checks pass; rootless tests also prove process-lifetime
NFSv4.1 session continuity across an orderly TCP reconnect and eight pipelined
NFSv3 calls under bounded in-flight dispatch, and a blocked NFSv3 RPC does not
hold a later fast RPC on the same connection behind it. A `max_in_flight=1`
wire test also proves the second call waits for the blocked first and both
replies complete. The forced-crash boundary tests
now reject a pre-crash NFSv3 file handle with `NFS3ERR_STALE`, a pre-crash v4
session with `NFS4ERR_BADSESSION`, and a pre-crash v4 root handle with
`NFS4ERR_STALE`; the v4 session identity folds both write-verifier halves to
avoid the observed replacement alias. Two independent v4 sessions also
complete concurrent distinct-file OPEN/WRITE/READ round trips. Multiple server
processes sharing one backend remain explicitly outside the supported scope.
The pinned oracle at published terminal-close commit `90e130b8` passed 266
tests with 18 explicit capability/root skips and zero mismatches. CI run
`35670416469` cancelled both native-NFS jobs before any steps ran. The opt-in macOS native
NFSv3 loopback mount gate passed 1/1 in 0.11s, and hosted run
`35658285441` passed the named macOS and Ubuntu native-NFS jobs; the overall
workflow was not green because unrelated jobs failed. Production remains
NO-GO pending complete v3/v4 stateful/member parity, a green hosted release
qualification, automatic reconnect/backend durability, crash injection,
durable-restart qualification, and native-client concurrency beyond the
bounded in-process scope. The
new bounded NFSv4 channel/state packet exposes `leaseSeconds`, per-client
session/fore-slot/COMPOUND ceilings, request/replay-cache ceilings, per-file
open/lock limits, and `requireReclaimComplete` through Rust and nested N-API
options; the wire suite passes 5/5, including `maxLocksPerFile` rejection for
an existing lock state, `NFS4ERR_TOOSMALL`/`NFS4ERR_NOSPC` channel-cap statuses,
refused-`CREATE_SESSION` replay followed by a next-sequence retry, and
`NFS4ERR_GRACE` gating of `OPEN`/`LOCK` before `RECLAIM_COMPLETE`. Rust
`Nfs4Clock` now drives automatic and explicit lease expiry sweeps with a
rootless wire test covering both paths; N-API clock injection and dynamic
callback ID-map parity now also pass through synchronous, panic/invalid-result-
safe N-API bridges, with a live v4.1 owner/clock sequence. Rust
`NfsSessionHooks` and N-API `NfsServerOptions.onError` now report decoded
request failures with panic isolation; the malformed-v4 callback test,
complete NFS target, release addon/typecheck, live N-API harness, and strict
affected Clippy pass. Named native-NFS hosted jobs are accepted platform
evidence, but complete hosted qualification and crash/durability gates remain
open. Deterministic seeded identities are covered.

Current local acceptance: on 2026-09-20, `scripts/test-all.sh` exited 0 at
`73c33e0` with the pinned mountx checkout and live, bucket-scoped Cloudflare R2
credentials held outside the repository. The run passed the complete Rust and
Node suites, memfs/SQLite/PGlite parity, live PGlite lifecycle and split-store
gates, authenticated R2 contract and five-seed R2 differential traces, the
configuration-driven R2 CLI (including both metadata-provider compositions),
HTTP/remote reopen, and R2 cleanup. Native privileged mounts, FSKit signing and
hosted Windows/macOS CI remain separate evidence boundaries.

Latest hosted evidence: [CI run 35499717435](https://github.com/andymac4182/mount-rs/actions/runs/35499717435)
at `37e9ba1` passed RustFS, Ozone, Linux Rust, Linux x64/arm64 Node,
Windows Node, and Linux native FUSE/NFS/9P/WebDAV jobs. Windows Rust failed
the HostFs lexical-root and directory-open tests; macOS jobs remain queued.
This evidence predates `3f52b44` and does not qualify that newer revision.
Its separate fault-injection workflow also passed (run `35499717417`).

Windows HostFs follow-up: directory handles now use backup semantics, raw
Win32 error mappings are separate from platform libc errno values, and root
tests cover drive/UNC paths. Main passed 14 macOS tests and strict Clippy;
post-fix Windows runtime CI is still required. Directory behavior was checked
against [libuv's Windows implementation](https://github.com/libuv/libuv/blob/v1.x/src/win/fs.c).
Run `35500474806` reached a different failure first: SQLite fencing setup
exhausted a 75 ms lease before publication. The test now uses long-lived setup
leases and explicitly expires the persisted lease before takeover, retaining
busy-owner, stale-writer, and winner-persistence assertions. Five local chunked
concurrency tests passed; Windows runtime confirmation remains required.
At `83bca86`, Windows job `106052286866` passed those fencing and HostFs unit
tests, then failed five HostFs integration tests: unsupported symlinks,
read-only create flags, and hard-link metadata. Lagrange is implementing these
without skipping the tests. Windows Node job `106052286914` passed.

Main ran `cargo test --workspace --all-targets --all-features --locked --offline`
against an isolated committed `cf3c485` snapshot on macOS: exit 0. Explicitly
ignored remote/native gates are not acceptance evidence from that run.
Main also reran the full oracle-enabled Node package suite after `753df86`:
exit 0, including harness/structural drivers, server protocols, 44 typed 9P
cases, memory parity, distribution and artifact aggregation. PGlite/R2 factory
and native-mount opt-ins were skipped in this run and remain separate gates.

Current-cycle evidence on 2026-09-21 adds a mount-free W01 concurrency packet,
an isolated provider/consumer matrix, and an eight-cell SQLite reliability
matrix. The dedicated PGlite lifecycle gate passed the Rust SDK rows for memfs,
memory/memory, SQLite/SQLite and PGlite/PGlite; the Node SDK rows for memfs,
SQLite, chunked-memory, chunked-SQLite and chunked-PGlite; and the five CLI
config/runtime rows. R2 rows remained explicit skips because this shell did not
have the credential variables. The post-fix full gate also passed the Rust and
Node suites, five-seed traces across memory/SQLite/object-store/chunked/PGlite
backends, and the N-API distribution checks. The full root gate exited 0 with
1,194 upstream tests passed and 40/40 seeded trace lanes passed; native mounts,
hosted Windows/Linux runs and live R2 remain separate acceptance gates.

Fresh current-tree `scripts/test-pglite.sh` acceptance also exited 0: real
PGlite provider parity/reconnect/fencing/cancellation, disk-server restart,
split metadata/blocks, N-API, chunked, userspace FUSE, Rust/Node/CLI provider
matrices, the 1,194-test upstream suite, and all 40 PGlite-inclusive seeded
trace lanes passed. R2 rows were explicit skips because this shell still lacks
the required scoped credentials.

The exact published tree at `747b160` was rerun on 2026-09-21 with the same
gate and exited 0: PGlite lifecycle/reconnect/fencing/cancellation, SQLite VFS
round-trip and fresh-provider reconnect, disk-server restart, split stores,
N-API, chunked, userspace FUSE, Rust SDK 6/6, Node SDK 5/5, CLI 11/11, the
upstream suite 1,200 passed/82 skipped, and all 40 seeded oracle lanes passed.
R2 factory/runtime rows remained explicit credential-gated skips; this is not
live R2 acceptance.

The latest published tree `f07579e` was rerun with the oracle-enabled N-API
package suite on 2026-09-21 and exited 0. Harness, factory, lifecycle, server,
JS-driver, FUSE/NFS/9P codecs, Unstorage, chunked, differential, restart and
distribution/artifact packets all passed; PGlite/R2 factories and native-mount
opt-ins remained explicit skips. The authorized macOS native NFS N-API gate
then passed real mounted write/read/unmount, and the Node CLI native gate passed
with independent Rust and Node clients writing to the same mount, Rust reading
Node bytes, clean unmount, and both payloads retained in the backing root.

The same tree also passed `./scripts/cargo-shared test --locked --workspace
--all-targets --all-features --offline` on macOS. Core, every registered
integration/transport crate, the SDK/CLI/HTTP/N-API paths, SQLite/VFS,
chunked, versioned and observability tests completed with exit 0; live R2,
PGlite, TiDB, RustFS and privileged native tests remained explicit prerequisite
skips rather than being counted as service acceptance.

The follow-up PGlite CLI consumer check in `dcc3aa4` also exited 0: the Rust
CLI now performs the same configured PGlite split-store write, shutdown,
reopen and readback that the Node CLI already performed. The live CLI matrix
is now 10 passes and one explicit R2 skip; no config-validation row is being
counted as live R2 evidence.

The bounded RustFS run on the current tree also exited 0. It passed real
immutable block writes/reopens, SQLite and PGlite metadata compositions, N-API
factories, remote CLI HTTP graceful-reopen, the SQLite VFS over RustFS blocks,
fault recovery, service restart/reopen, and the RustFS block benchmark. This
is strong S3-compatible evidence, but it is intentionally not counted as live
Cloudflare R2 evidence.

Latest SDK-consumer slice at local `21803fd` was published to `main` as the
sequential API commits `9868e93`, `83153ef`, `b8a49a0`, `9163a39`, `b09965c`,
`cc605c7` and `b43a4e9`. The Rust provider matrix now opens PGlite/R2 through
the public Rust SDK facade and verifies clean shutdown/reopen; the Node matrix
does the same through `createChunkedDriver`; and the Node CLI accepts the same
versioned provider config shape as the Rust CLI. The local PGlite run passed
Rust SDK 4/4, Node SDK 5/5 and CLI 7/7 gated cases, while R2 remained an
explicit credential skip. The CLI also rejects `--reopen` for process-local
memory rather than reporting a false durability result. Full offline workspace
tests and the N-API package gate passed after this slice; hosted CI is still
queued and does not count as green evidence.

The follow-up structural-driver slice at local `fd5eb04` was published as
`88f8f9b` and `f7e630e`. It forwards normalized numeric `open_flags` with the
Linux/macOS/Windows Node flag namespaces, and the rebuilt addon passed the
opt-in macOS NFS structural mount lifecycle with mounted write/readback,
callback reachability, unmount and cleanup. Linux, non-NFS transports, hosted
Windows execution and full FUSE/FSKit acceptance remain separate gates.
The full locked offline workspace gate also exited 0 after this patch.

Focused current-head acceptance after the follow-up packets: `mount-rs-sdk`
unit tests passed (2/2), the Rust provider matrix passed memfs, memory/memory,
SQLite/SQLite and PGlite/PGlite (4/4, with R2 explicit skips), the Node SDK CLI
direct driver self-test passed, the complete N-API suite passed, chunked tests
passed (12/12 plus 7/7 concurrency), SQLite VFS tests passed (4/4 unit, 16/16
engine and 16/16 bridge), and local HostFs tests passed (14/14 including the
Windows oracle cases where runnable). These are focused local gates at
`3042d09`; the current shell still lacks live R2 credentials and hosted
Windows/macOS and privileged native-mount runs remain unqualified.

The current W01 mount-free FUSE packet adds typed napi-rs `GETLK`, `SETLK` and
`SETLKW` request codecs plus the typed `GETLK` reply codec. The generated root
bindings, explicit CommonJS/ESM barrel exports and `./fuse` declarations are
covered by a typecheck and a pinned-oracle differential covering the 48-byte
request, 24-byte typed reply, truncation, trailing-byte and empty status-reply
boundaries. The release N-API build, oracle-enabled package suite and focused
locked `mount-rs-fuse` tests passed. This is mount-free wire evidence only;
native FUSE lock/session semantics remain an explicit boundary.

The delegated W01-FUSE owner now has a canonical working ledger in
`docs/W01_FUSE_PROGRESS.md`, with the parent roll-up in `docs/W01_PROGRESS.md`.
The first refreshed-tree Rust session-controls chunk adds public construction
options and lifecycle/error observability while preserving the established
durable `FLUSH` sync default. Its focused package evidence is mount-free only;
native Linux FUSE, callback events, hosted platform gates, FSKit activation,
cancellation, concurrency, crash/restart and durability remain open.
The follow-up N-API chunk adds the Rust-backed `FuseSession` class and public
`./fuse` facade with typed options/defaults, negotiated state, inode views,
request/reply/error counters, assertion/error callbacks, notification encoders,
destroy-state readback, generated declarations, and raw INIT/LOOKUP/READLINK
coverage. Its locked N-API check/Clippy, debug addon build, focused session/
codec/typecheck tests, FUSE tests, formatting and diff checks pass; the full
`MOUNTX_SOURCE` package suite, native Linux callback events, FSKit, cancellation,
concurrency, crash/restart and durability remain open.

The current W01-FUSE callback slice threads `FuseMountHooks` through
`mount-rs-auto` and exposes the root N-API `mount(..., { onTransportError })`
option. The JavaScript callback is converted to an owned thread-safe function
before `Env::spawn_future`, so the async mount path never carries a non-`Send`
N-API value. The generated declarations, locked auto/N-API check and Clippy,
16 N-API unit tests, debug addon build, generated TypeScript check, default
facade test, and focused FUSE suite pass locally. `MOUNTX_SOURCE` codec rows
remain explicit skips, and hosted Linux callback-event delivery, native
mount/lifecycle, FSKit, cancellation, concurrency, crash/restart and durability
remain open; W01 stays NO-GO.

The follow-up mount-free FUSE session packet adds `GETLK`/`SETLK` byte-range
conflict tracking, same-owner replacement/unlock, and `RELEASE`/`DESTROY`
cleanup with strict lock-body and flag validation. `SETLKW` returns explicit
`EAGAIN` instead of blocking the serialized session, and native POSIX/flock
flags remain unadvertised until a concurrent blocking path is implemented and
qualified on Linux. The focused session target passed 19/19, the complete
locked FUSE target and strict Clippy passed, and the packet remains separate
from hosted native mount, callback-event, FSKit, crash/restart and durability
acceptance; W01 stays NO-GO.
The next focused FUSE packet adds the protocol 7.34 eight-byte `SYNCFS` body
codec and routes the native request to the existing `FsDriver::syncfs` barrier.
Success, backend failure, malformed/trailing bodies and empty replies are
covered by focused tests; hosted kernel syncfs and the remaining native
lifecycle/crash/durability gates remain external, so W01 stays NO-GO.
The follow-up N-API FUSE packet exposes the same request as typed
`NativeFuseSyncfsIn` `decodeSyncfsIn`/`encodeSyncfsIn` bindings, with explicit
`./fuse` CommonJS/ESM aliases and regenerated declarations. Exact eight-byte,
truncated and trailing-body checks pass alongside the release addon rebuild,
focused codec test, generated typecheck and locked N-API Rust check. The
pinned mountx oracle still classifies `SYNCFS` as unimplemented, so no oracle
differential is claimed for this operation; hosted kernel syncfs behavior and
the remaining native lifecycle, callback, crash/restart and durability gates
remain open, and W01 stays NO-GO.
The latest FUSE teardown packet makes forced session-task cancellation a
terminal lifecycle transition: bounded unmount-timeout and post-runtime
destructor fallbacks now mark the mount inactive/closed and wake
`wait_closed()` observers. A Linux-gated regression covers the lifecycle
contract; host FUSE tests, host/Linux-target strict Clippy, Linux-target test
check, formatting and diff checks pass, while actual Linux `/dev/fuse`
forced-unmount, callback, crash/restart and durability execution remain
external, so W01 stays NO-GO.
The latest FUSE lifecycle packet wraps the Linux request loop and asynchronous
session destroy in unwind isolation. A backend or cleanup panic now becomes
one owned `Task` transport error, still closes the session, marks the mount
inactive/closed, and wakes lifecycle waiters; a Linux-gated Unix-stream panic
harness covers the callback and state boundary. The macOS all-target FUSE
suite, strict Clippy, formatting, and Linux-target test type-check passed;
hosted Linux native fault/crash/restart, callback-event, concurrency, lock and
durability evidence remain external, so W01 stays NO-GO.
The follow-up cancellation packet makes the Linux loop observe `request_stop()`
while a backend request is in flight. The cancellable request future is
dropped, session cleanup runs, and lifecycle state closes without reporting an
orderly stop as a transport error; a Linux-gated blocking-driver harness
covers the bounded close. Local all-target FUSE tests, strict Clippy,
formatting, and Linux-target test type-check pass, while hosted close races,
concurrent request behavior, crash/restart, callback-event, lock and
durability evidence remain external; W01 stays NO-GO.
The bounded concurrency packet now permits up to 16 positional `FUSE_READ`
workers with a single serialized reply writer. Stateful operations and writes
still use the serialized session boundary, and stop aborts plus drains read
workers; a Linux-gated barrier-driver harness proves two reads overlap. Local
all-target FUSE tests, strict Clippy, formatting, and Linux-target test
type-check pass, while hosted `/dev/fuse`, native mutation/write concurrency,
close/crash/restart, callback-event, lock and durability evidence remain
external; W01 stays NO-GO.
The follow-up interrupt packet registers those read workers by request unique:
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
The macOS FSKit boundary was refreshed locally: the locked Rust bridge target
passed 12 tests, formatting and strict Clippy, the arm64 bridge build passed,
the standalone Swift delegate seam and in-process XPC lifecycle test passed,
and unsigned arm64 `MountRsFSKit`, `MountRsXPCService`, and `MountRsHost`
Xcode schemes produced the expected extension/XPC artifacts. The diagnostic
activation gate skipped its mount attempt and reported zero valid signing
identities, an ad-hoc host bundle, and an unavailable `fskitd` connection;
it ended `FSKIT_ACTIVATION=BLOCKED`. This is compile/in-process evidence only:
signed installation, FSClient enablement, mounted read/write, hosted Linux
FUSE, callback/lifecycle, crash/restart, concurrency, locks, and durability
remain open, so W01 stays NO-GO.
The latest mount-free N-API FUSE public-surface packet closes a verified
barrel gap: all 185 pinned wire constants are statically discoverable through
CommonJS and ESM named imports with `./fuse` declarations, and the facade now
exposes opcode body dispatch plus complete request/reply framing, 8-byte
extension validation, raw/unknown handling, and current `SYNCFS` support.
`npm run build:debug`, oracle-enabled `test/fuse-codec.mjs`,
`test/fuse-session.mjs`, `test/fuse-inodes.mjs`, generated `test/typecheck.mjs`,
and `CARGO_TARGET_DIR=/private/tmp/mount-rs-napi-public-20260922
./scripts/cargo-shared test -p mount-rs-napi --all-targets --locked` (16/16)
pass. The pinned oracle still marks `SYNCFS` unimplemented, so no oracle
differential is claimed for that extension. The oracle-enabled package suite
reached NFS and stopped on this environment's `Operation not permitted` socket
bind; hosted Linux FUSE, callback/lifecycle, close/crash/restart, concurrency,
locks, and durability remain open, so W01 stays NO-GO.
The native transport follow-up adds owned `FuseTransportError` kinds,
`FuseMountHooks`, `mount_with_hooks`, exactly-once terminal reporting,
callback-panic isolation, and a mount-free Unix-stream protocol-failure
harness. The locked FUSE target, formatting and diff checks pass on macOS; the
Linux-only hook harness and hosted `/dev/fuse` callback delivery remain
external evidence, as do FSKit, cancellation, concurrency, crash/restart and
durability.
The automatic-mount follow-up now threads one owned N-API `onTransportError`
callback through the FUSE, 9P, and NFS native mount paths, converts the
JavaScript callback synchronously to a TSFN before spawning the async mount,
and releases it on `Mounted` teardown. The scoped auto/NFS/N-API Rust tests,
debug addon build, native-facade skip lane, generated typecheck, formatting
and diff checks pass; hosted native fault-event delivery and the remaining
native lifecycle gates remain open.
The latest mount-free N-API session follow-up now starts from the same
conservative INIT policy as the serialized native dispatcher. Default
negotiation no longer advertises asynchronous direct I/O, parallel directory
dispatch, or `SETXATTR_EXT` without an explicit caller override; the generated
N-API session test asserts those flags remain clear. This closes a capability-
honesty gap but does not qualify hosted Linux negotiation, native callback
delivery, cancellation/concurrency, crash/restart, durability, or FSKit.
The current pinned FUSE oracle refresh then passed both focused commands from
`integrations/mount-rs-napi`: `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921
node test/fuse-codec.mjs` passed every listed FUSE protocol differential family,
and `node test/fuse-inodes.mjs` passed inode parity. This closes focused
mount-free codec/inode evidence only; the full package matrix, hosted Linux
`/dev/fuse`, native callback/lifecycle, remaining session parity, FSKit,
cancellation/concurrency, crash/restart, and durability remain open.
The follow-up session boundary now runs the public codec validators for
`BMAP`, legacy and negotiated-extended `SETXATTR`, `GETXATTR`, `LISTXATTR`,
and `REMOVEXATTR` before returning their valid-request `ENOSYS` boundary.
Malformed bodies return `EINVAL` without backend mutation. The complete locked
FUSE target passed 14 unit, 6 INIT, 6 notify/record, 11 protocol, 20 session,
and 3 sync-barrier tests, and strict warning-denied Clippy passed; native xattr
and BMAP support remains unadvertised and unimplemented.
The subsequent native read-interrupt packet is also compiled against this
context-aware validator, so `FUSE_INTERRUPT` retains its fixed-body boundary
while known read workers can be aborted independently.
After that repair, the published-tip verification passed formatting and diff
checks, the locked FUSE/N-API all-target tests, isolated warning-denied Clippy,
and the `x86_64-unknown-linux-gnu` FUSE test-target compile. Rebuilding the
debug N-API addon followed by generated typecheck, the mount-free FUSE session
test, and the pinned codec and inode oracle tests also passed. Hosted CI run
`35632382070` was cancelled by a later concurrent push, so it provides no
Linux `/dev/fuse` evidence; native mount/callback, FSKit, close/crash/restart,
mutation/write concurrency, locks, and durability remain open and W01 stays
NO-GO.
The local FUSE checkpoint was then rebased onto current `origin/main`, which
includes the eight-client native concurrency and read-panic callback-fault
harnesses. The locked FUSE target, strict Clippy, Linux test-target compile,
locked N-API target, rebuilt debug addon, generated typecheck, mount-free
session, pinned codec oracle, and inode oracle all passed. The ignored native
harnesses were not executable on this macOS host, so hosted `/dev/fuse`,
callback delivery, panic/cleanup, close/crash/restart, mutation/write
concurrency, locks, durability, and FSKit remain external; W01 stays NO-GO.
The subsequent hosted CI run `35645669363` / native-FUSE job `106485735709`
passed the Linux FUSE prerequisite probe but was cancelled during the actual
rootless kernel file-operation step, so it supplies no native acceptance
evidence. The exact branch tip still requires a non-canceling manual hosted
qualification before any Linux mount, callback, lifecycle, concurrency, lock,
crash/restart, or durability result can be promoted; W01 remains NO-GO.
The published FUSE follow-up was then verified against hosted run `35668748366` /
native-FUSE job `106560234631`: ordinary round-trip and backend-panic
callback/close passed, while blocked-read unmount failed after 30.03s. The
current teardown-drain fix is locally green and requires a fresh non-canceling
hosted rerun; W01 remains NO-GO.

Parallel W01 sidecars completed on 2026-09-21 and were published to `main`:

- `ccaf8f5` + `4cdeb58` correct the Windows SQLite WAL shared-memory access mask
  and add a Windows-only mapping regression test. The focused SQLite VFS gate
  passed 5 unit, 16 engine and 16 storage-bridge tests, but hosted Windows
  rerun is still required.
- `7236e0a` + `4422cd0` add oracle-backed Unstorage capability, unsupported
  operation, metadata and ownership-overlay parity. Direct and N-API tests
  pass; the broader W01 ledger remains open.
- `57632c6` + `e213856` add structural N-API adapter checks and an opt-in native
  lifecycle test. The authorized macOS NFS run passed kernel read, callback
  reachability, unmount and cleanup; native structural writes remain an explicit
  `EIO`/unsupported boundary until decoded `open_flags` are exposed.
- `1af1986` + `8ac77f3` + `9215ba3` + `a1ec0c5` retain NFSv3/v4 backend handles
  across unlink/rename and classify the pinned TypeScript control separately.
  Focused Rust tests passed 30 unit, 8 integration and 266 oracle cases with
  18 capability-gated skips; privileged Linux/macOS NFS qualification remains
  separate.

These packets reduce the W01 queue but do not close W01.1-W01.4: the complete
parity ledger, all classified skips, cross-backend seeded evidence, live R2,
hosted Windows/macOS and privileged native transport gates remain required.

The FUSE session follow-up `1cf0350` corrects the support matrix: plain
`RENAME2` is dispatched as a normal rename, while exchange/whiteout flag
variants return `ENOSYS` without mutation. The README now states that boundary
and the focused session test covers all three nonzero flag variants. The
16-test session suite, strict Clippy, formatting and diff checks passed.

Final integrated local gate after `192749e`: `cargo test --workspace
--all-targets --all-features --locked --offline` exited 0, and the
oracle-enabled `pnpm test` in `integrations/mount-rs-napi` exited 0 with the
new Unstorage parity included. R2/PGlite factory and privileged native-mount
rows remain explicit skips without credentials or opt-in host prerequisites.

Latest W01 rotation completed with three disjoint Luna Max packets. Local
commits `31d62df`, `3355cc1` and `323231f` were each validated and published
to `origin/main` through the GitHub contents API. The Linux packet wires an
explicit `/dev/fuse`/`fuse3` structural-driver mount/read/write/unmount job;
the hosted Linux result is still required. The Unstorage packet adds oracle
parity for atime, mtime, ctime and birthtime (11/11 Rust tests plus direct and
N-API parity). The FUSE packet exposes and differentially tests the `READDIR`
body codec, including UTF-8 names, alignment, bounded packing and malformed
input. The full locked Rust workspace gate, oracle-enabled N-API gate and the
real macOS Rust+Node CLI NFS demo all passed after the rotation.

The next SDK-consumer packet is now implemented and locally verified. The
Rust CLI exposes `sdk-self-test`, a mount-free process-level example that
constructs memory or configured durable filesystems through
`mount-rs-sdk::Filesystem`, writes/readbacks through the shared driver, and
supports a shutdown/reopen assertion. The Node CLI has the matching SQLite
split-store shutdown/reopen integration row. The provider matrix runs both
real CLI processes: offline results are 8 CLI passes and 2 explicit
PGlite/R2 skips; the Rust SDK matrix is 4 passes and 4 explicit external-gate
skips. The oracle-enabled N-API suite passed, including the public
`READDIRPLUS` body differential and artifact aggregation. This is a focused
consumer-path checkpoint, not closure of W01 or live-provider/native-mount
acceptance.

The latest parallel acceptance rotation added three bounded packets and
published them to `origin/main`. `3b59dc8` exposes typed FUSE `GETATTR` and
`SETATTR` request/reply codecs with pinned 7.41/7.8 differential coverage;
`7da18fb` verifies exact prefix cleanup and sibling/parent sentinel
preservation in the real FoundationDB metadata + RustFS chunk harness; and
`d6c8a29` hardens the real TiDB + RustFS composition fixture with explicit
durable scope, block absence checks, metadata-row cleanup and symlink-path
rejection. The combined locked offline Rust workspace gate and oracle-enabled
N-API suite both exited 0. The FoundationDB and TiDB mixed-provider harnesses
passed real service runs; TiDB used the available single-node v8.5.7 topology,
so replicated/durable capacity and provider restart promotion remain open
acceptance boundaries rather than being implied by these passes.

The following W01 rotation then completed three more disjoint Luna Max packets
and published them to `main`: `a31880d` adds seeded positional write,
truncate, flush and reopen coverage across the Rust SDK, Node SDK, Rust CLI and
Node CLI provider matrix; `cc73ad5` adds 11 Unstorage path/metadata/handle
oracle rows with zero mismatches or skips; and `a3795f0` exposes typed FUSE
`OPEN`/`OPENDIR` request codecs across protocol 7.8, 7.39 and 7.41. The
focused provider matrix passed 5 Rust SDK, 4 Node SDK and 9 CLI cases with
explicit PGlite/R2 skips; the Unstorage packet passed its direct, capability,
edge, N-API and upstream conformance gates; and the full locked Rust workspace
plus oracle-enabled N-API suite passed. These packets further reduce W01 but
do not claim native FUSE session/mount or live-provider acceptance.

The latest W01 rotation completed three additional disjoint Luna Max packets
and published them to `main` on 2026-09-21:

- `80f2efd` (published as `e5902bed`) fixes Windows HostFs symlink creation to
  use normal Win32 paths with a long-path fallback. The focused HostFs suite
  passed 15 tests, the Windows target check and strict target Clippy passed;
  hosted Windows runtime confirmation, read-only create flags and hard-link
  metadata remain open.
- `16b2180` (published as `9832b60`) adds typed Rust FUSE `ACCESS` session
  dispatch with exact 8-byte request validation, root handling, owner/group/
  other permission checks, invalid-mask errors and focused session tests.
- `7dd9a60` (published as `83466d7f0be058390f5c951a58d04d3a67f4923e`)
  exposes Rust-backed FUSE `LOOKUP` request/reply codecs through the N-API
  package, including declarations, generated artifacts, protocol 7.8/7.39/
  7.41 differentials and malformed/truncated/trailing-input checks.

The combined local gate for this rotation passed `cargo fmt --all -- --check`,
the locked offline all-feature Rust workspace suite, and the oracle-enabled
N-API suite. The N-API run passed the FUSE `READDIR`, `READDIRPLUS`, `READ`,
`WRITE`, `GETATTR`, `SETATTR`, `OPEN`/`OPENDIR`, `CREATE` and `LOOKUP`
families, 9P, NFS, WebDAV, Unstorage, chunked storage and distribution checks;
PGlite/R2 factories and native mounts remained explicit prerequisite skips.
The newest CI and fault-injection runs for `83466d7` are queued, not green
evidence. These packets further reduce W01 but do not close W01 or establish a
privileged native FUSE mount, FSKit activation, live R2, or hosted Windows run.

The Windows HostFs follow-up was then published as `44cef6e` (from sidecar
`d64d3bb`). It aligns long-path symlink fallback, Windows unlink disposition,
and read-only hard-link metadata/lifetime behavior, with Windows-gated
regressions. The focused macOS suite passed 10/10, the installed
`x86_64-pc-windows-gnu` target compiled all HostFs targets, strict Clippy and
format/diff checks passed. Hosted Windows runtime execution remains open, so
this packet is target-compile evidence rather than Windows platform acceptance.

Fresh macOS demo evidence on this checkout: `bash scripts/demo-end-to-end.sh`
exited 0 after building the Rust CLI. The CLI mounted the local HostFs through
native NFS; an independent Rust process wrote/read `rust-process.txt`, an
independent Node `fs/promises` process wrote/read `javascript-process.txt`, a
second Rust process verified the JavaScript bytes, and the CLI unmounted cleanly
with the bytes still present in the host-backed directory. This is a real
single-host NFS mount demonstration, not FUSE/FSKit or a remote-provider result.

The matching Node CLI native path also passed on macOS: `node
examples/node-cli/index.mjs --driver host --root <temporary backing>
--transport nfs --mountpoint <temporary mount> --self-test` exited 0, with the
Node SDK mounting through native NFS, writing and reading its file, unmounting,
and leaving the temporary backing directory clean. This is a separate Node CLI
consumer check; it does not qualify Linux FUSE, FSKit or live providers.

The latest W01 acceptance packets were published to `origin/main` at
`25644c5`. `462d54b` closes the remaining Unstorage evidence harness gap:
preparations now execute for both oracle and native adapters, refusals assert
state preservation, hidden hardlink/mknod boundaries are split into separate
rows, and the packet is part of the N-API chain. The direct and N-API gates
passed 31 rows (5 PASS, 26 exact ENOSYS, zero ENOTSUP or skipped rows).
`a3d980d` extends the opt-in Node SDK CLI native gate so the mounted path is
exercised by an independent Rust client and an independent Node client, with
Rust verifying the bytes written by Node. The authorized macOS NFS run passed
mount, Rust read/write, Node read/write, cross-client readback, unmount and
backing-root persistence. `25644c5` adds plain-flag FUSE `RENAME2` session
support while keeping unsupported flags fail-closed; 16 focused FUSE tests,
strict Clippy and formatting passed. FALLOCATE, LSEEK and COPY_FILE_RANGE
remain explicit unsupported boundaries, and Linux hosted FUSE remains a
separate acceptance gate.
The follow-up `7183778` strengthens the same native gate by asserting that
both Rust- and Node-written payloads remain in the backing root after unmount.

The third W01 implementation rotation was validated locally and published
sequentially to `origin/main` (verified remote ref
`10666e8dcca35c0cb39483317272de6832acb984`). The Rust FUSE packet is
`15026f2`, the napi-rs packet is `5b7f982`, and the CI packet is `d28f31a`.
Rust now validates the fixed-width FUSE `POLL` request at
the session boundary, rejects malformed/truncated/trailing frames with
`EINVAL`, and keeps valid `POLL` at an explicit safe `ENOSYS` boundary until a
driver supplies poll semantics. napi-rs exposes the matching request/reply
codecs, generated declarations/artifacts, protocol-minor oracle coverage and
malformed-frame tests. CI now runs the opt-in Node SDK CLI native mount gate on
Linux and macOS when the required host prerequisites are present. The combined
locked workspace gate, scoped FUSE tests/Clippy and oracle-enabled N-API suite
passed; hosted Linux/macOS results and actual kernel POLL support remain open.

The next three implementation packets were validated and published to
`origin/main` on 2026-09-21. The HostFs packet is local `72d570f` and finished
publishing at remote commit `1352fa9`; the N-API FUSE lifecycle packet is local
`13c3acb` and finished at remote commit `5ba69dd`; and the pure Rust FUSE INIT
packet is local `b2040f6` and finished at remote commit `63c4264`. The HostFs
packet adds libuv-aligned Windows read-only creation/unlink behavior and
Win32 hard-link lifetime coverage. The N-API packet adds pinned-oracle
`RELEASE`/`RELEASEDIR`, `FLUSH`, and `FSYNC`/`FSYNCDIR` request/status codecs.
The Rust packet hardens version-gated `FUSE_INIT_EXT`/`flags2` handling and
adds six wire-negotiation integration tests. The combined locked Rust suite
and full pinned-oracle N-API suite passed; the N-API packet's scoped Clippy
gate still excludes the pre-existing `js_driver.rs` type-complexity warning.
Hosted Windows runtime, live R2/PGlite, FSKit activation and privileged native
FUSE acceptance remain separate gates.

The fourth W01 implementation packet is locally validated in `1a8b112` and
published through remote commit `7c6186f`. Rust FUSE now strictly validates
the `FALLOCATE`, `RENAME2`, `LSEEK`, and `COPY_FILE_RANGE` request bodies,
rejects malformed/trailing frames with `EINVAL`, and keeps valid operations at
an explicit `ENOSYS` boundary without mutating the session. The focused FUSE
tests and strict scoped Clippy gate passed; hosted/native acceptance remains a
separate gate.

The W01 follow-up packets are now published sequentially. The N-API xattr
packet is local `cf7a132` and finished at remote commit `4f484ad`; it adds
pinned-oracle `SETXATTR`, `GETXATTR`, `LISTXATTR`, and `REMOVEXATTR` codecs,
generated declarations/artifacts and malformed/truncated/trailing coverage.
The Unstorage packet is local `e0e8195` and finished at remote commit
`9b74c87`; its 14 capability-boundary rows passed with 5 supported results, 9
explicit `ENOSYS` results, zero `ENOTSUP` mismatches and zero skips.

The Unstorage hardlink follow-up is local `99c7d32` and published through
`2bce444`: four hardlink capability rows passed with zero supported results,
four exact `ENOSYS` results, zero `ENOTSUP` mismatches and zero skips; every
row preserved errno `-38`, syscall `link`, and null path/destination fields.

The W04.3 PGlite packet was rebased onto remote `8ea3dc8` and published as a
seven-file sequence ending at `90c33949`. It adds durable version metadata,
real-server reconnect/version tests, mount-free SQLite VFS coverage and the
configuration-driven PGlite gate. Focused versioning/VFS tests, locked
compilation, formatting, Clippy and script checks passed; hosted reconnect
acceptance remains open.

The provider packets subsequently published on the current remote are W08
TiDB/RustFS hardening (rebased local tip `6327857`, published through
`bbe8ebb`), W07 FoundationDB/RustFS restart-gate hardening (rebased local tip
`629c2f6`, published through `ffd06bf`) and W30 observability (published at
`6ce8228`). W26's nine-file Ozone packet remains byte-for-byte present in the
current branch at its verified `a936eba` content. The W26 follow-up now adds a
dedicated hosted `ozone-compositions` CI job for the real SQLite/PGlite mixed
metadata gate; its hosted result remains pending until that job runs. W08 and
W07 service runs remain explicitly blocked by unavailable Docker/libfdb runtime
prerequisites;
W30 external collector reachability and Linux/Windows qualification remain
open; the W30.5 local loopback collector, exporter-failure/shutdown tests,
macOS qualification packet, and no-exporter facade benchmark are now verified
on the current macOS arm64 host.

## How to read and maintain this tracker

- **Landed:** committed implementation, not necessarily full acceptance.
- **Implementing:** active, uncommitted work; not delivered.
- **Verifying:** implementation exists, acceptance evidence is incomplete.
- **Planned:** required but not yet assigned or implemented.
- **Deferred:** deliberately sequenced later, not forgotten or completed.
- Checked tasks describe only the precise scope stated. Unchecked tasks remain open.
- Main maintains this file after handoffs, validated commits, failures and new
  findings. Workers report task IDs, changed files, commands/results and blockers
  to main rather than concurrently editing it. Split newly discovered work into
  named tasks below. Keep failed, skipped and unavailable evidence visible.
- Commit and push validated chunks to `origin/main`; do not call a workstream
  complete until its integration and platform acceptance gates pass.

### Delegated execution and rotation

`Main` in the dashboard means the coordinator owns cross-stream integration,
acceptance evidence, tracker updates and commit/push; it does not mean that
every implementation task is being done serially. Workers receive disjoint
write scopes, return exact paths and test evidence, and are closed after their
patch is integrated. A completed worker is then rotated into the next open,
non-overlapping packet. The current bounded allocation is:

| Worker | Packet | Write scope | Handoff state |
| --- | --- | --- | --- |
| `dbfa2ea` | N-API strict-Clippy type cleanup | Factors the structural-driver open future into a named alias; full scoped N-API Clippy now passes with `-D warnings` and no type-complexity exclusion. This is lint evidence, not native-mount or hosted-platform acceptance |
| `66c79e0` | Rust FUSE session parity for simple namespace operations | 16 frame-level session tests cover SYMLINK/MKNOD/MKDIR/UNLINK/RMDIR/RENAME/LINK/ACCESS, error/state cleanup, MKNOD fallback and POSIX name limits; focused FUSE tests, strict Clippy and formatting passed. Native device/mount and advanced FALLOCATE/LSEEK semantics remain open |
| `6972fbe` | Unstorage hardlink alias capability boundary | Direct and N-API packets now cover six exact `ENOSYS` hardlink rows, including alias write-through and unlink-lifetime cases, with zero `ENOTSUP` mismatches and zero skips; generic hardlink inode support remains open |
| `96bbbd9` | Structural-driver native lifecycle assertion hardening | Explicit transport identity is asserted and the PASS marker is emitted only after unmount, live-mount cleanup and mountpoint teardown; macOS NFS structural mount/read/write/unmount passed. Linux FUSE hosted execution remains unverified |
| `24c8a6b` | Public napi-rs FUSE request/reply codecs for SYMLINK/MKNOD/MKDIR/UNLINK/RMDIR/RENAME/RENAME2/LINK/ACCESS/FALLOCATE/LSEEK | Pinned mountx differential plus typed round-trip coverage passed; full oracle-enabled N-API test chain, typecheck and artifact aggregation passed. Native FUSE session/device/mount remains open |
| Peirce | W12/W15 SQLite VFS and WAL/reliability seam | `integrations/mount-rs-sqlite-vfs/**`, related VFS plan | Integrated |
| Mill | W08 TiDB provider and RustFS composition harness | `integrations/mount-rs-tidb/**`, `tests/tidb/**`, TiDB harness | Integrated; bounded TiDB + RustFS composition passed |
| Aristotle | W13 macOS FSKit seam | `integrations/mount-rs-fskit/**` | Integrated checkpoint |
| Meitner | W24 TanStack Start marketing/docs site | `apps/site/**` | Child task complete; custom domain live |
| Ohm | W18.6 storage-dispatch draft review | `benchmarks/storage/dispatch/**` | Closed; no change recommended |

Completed W01 rotation packets (closed after main reviewed and published each
patch):

| Worker | Packet | Write scope | Handoff state |
| --- | --- | --- | --- |
| Hume | W01 Linux structural FUSE acceptance wiring | `.github/workflows/ci.yml`, `integrations/mount-rs-napi/{README.md,test/structural-native.mjs}` | Integrated as `31d62df`; hosted Linux result pending |
| Gibbs | W01 Unstorage timestamp metadata parity | `integrations/mount-rs-kv/tests/driver.rs`, Unstorage parity tests | Integrated as `3355cc1`; focused oracle gates passed |
| Euler | W01 FUSE READDIR body codec | `integrations/mount-rs-napi/{fuse.cjs,postlude-fuse-codec.cjs,index.d.ts,test/fuse-codec.mjs}` | Integrated as `323231f`; pinned differential passed |
| Confucius | W01 FUSE READDIRPLUS body codec | `integrations/mount-rs-napi/{fuse.cjs,postlude-fuse-codec.cjs,index.d.ts,test/fuse-codec.mjs}` | Integrated in the current SDK/transport packet; pinned differential and aggregation passed |
| James | W01 Unstorage capability classification | `tests/unstorage/capability-parity.mjs` | Integrated in the current packet; 5 oracle-classified unsupported rows passed |
| Curie | W01 provider-consumer durability expansion | `tests/provider_matrix/src/main.rs` | Integrated in the current packet; direct and split SQLite reopen rows passed |
| Galileo | W01 seeded provider/consumer lifecycle parity | `tests/provider_matrix/**` | Integrated as `a31880d`; 5 Rust SDK, 4 Node SDK and 9 CLI passes; PGlite/R2 explicit skips |
| Poincare | W01 Unstorage path/metadata/handle parity | `tests/unstorage/**` | Integrated as `cc73ad5`; 11 rows, zero mismatches/skips; remaining capability limits explicit |
| Socrates | W01 FUSE OPEN/OPENDIR request codecs | `integrations/mount-rs-napi/**` | Integrated as `a3795f0`; protocols 7.8/7.39/7.41 differential passed; native mount remains open |
| Lagrange | W27 Windows HostFs symlink portability | `crates/mount-rs-host/**` | Integrated as `80f2efd` / published `e5902bed`; focused macOS and Windows-target gates passed; hosted Windows runtime remains open |
| Mendel | W10 Rust FUSE ACCESS session dispatch | `transports/mount-rs-fuse/**` | Integrated as `16b2180` / published `9832b60`; focused locked session tests passed; native device/mount remains open |
| Euler | W01 napi-rs FUSE LOOKUP request/reply codecs | `integrations/mount-rs-napi/**` | Integrated as `7dd9a60` / published `83466d7`; pinned protocol differential and full N-API gates passed |
| Locke | W27 Windows HostFs read-only/link lifetime parity | `integrations/mount-rs-host/**` | Integrated as `72d570f` / published through `1352fa9`; 15 macOS tests, Windows-target check and target Clippy passed; hosted Windows runtime remains open |
| Singer | W01 napi-rs FUSE lifecycle codecs | `integrations/mount-rs-napi/**` | Integrated as `13c3acb` / published through `5ba69dd`; pinned differential and full N-API suite passed; strict pre-existing Clippy lint remains |
| Zeno | W01 Rust FUSE INIT wire negotiation | `transports/mount-rs-fuse/{src/init.rs,tests/init.rs}` | Integrated as `b2040f6` / published through `63c4264`; six focused tests and strict scoped Clippy passed |
| Schrodinger | W01 napi-rs FUSE READLINK/STATFS codecs | `integrations/mount-rs-napi/**` | Integrated as `054fb95`; pinned protocol differential, declarations/artifacts, typecheck, build and full N-API suite passed; scoped Clippy retains the known pre-existing lint exclusion |
| Pasteur | W01 Node SDK CLI native integration | `examples/node-cli/**` | Integrated as `8e1f218`; opt-in macOS NFS mount/read/write/unmount/persistence passed; Linux FUSE and unavailable-platform paths remain explicit skips |
| Pascal | W01 Rust FUSE READLINK/STATFS wire/session behavior | `transports/mount-rs-fuse/{src/session.rs,tests/**}` | Integrated as `08731ed`; full FUSE suite and strict scoped Clippy passed; native kernel/session acceptance remains separate |
| Nietzsche | W01 napi-rs FUSE BATCH_FORGET/INTERRUPT codecs | `integrations/mount-rs-napi/**` | Integrated as `4dc90d5`; generated bindings, pinned-oracle protocol-minor tests, typecheck, build and full N-API suite passed; scoped Clippy retains the known pre-existing lint exclusion |
| Nash the 2nd | W01 Rust FUSE forget/interrupt validation | `transports/mount-rs-fuse/{src/session.rs,tests/**}` | Integrated as `7934062`; strict BATCH_FORGET validation, fail-closed INTERRUPT semantics, 48 package tests and strict scoped Clippy passed; native kernel cancellation remains open |
| Kant | W01 Unstorage remaining capability parity | `tests/unstorage/**` | Integrated as `b13350f`; 25 oracle rows passed with 4 supported, 21 explicit ENOSYS classifications, 0 ENOTSUP mismatches and 0 skips |
| Aquinas the 2nd | W01 Rust FUSE POLL session validation | `transports/mount-rs-fuse/{src/session.rs,tests/**}` | Integrated as `15026f2`; strict 24-byte framing, malformed/trailing rejection, explicit ENOSYS boundary, 50 package tests and strict scoped Clippy passed |
| Euclid the 2nd | W01 napi-rs FUSE POLL request/reply codecs | `integrations/mount-rs-napi/**` | Integrated as `5b7f982`; generated artifacts/declarations, protocol-minor oracle differentials, malformed/truncated/trailing tests, build/typecheck and full N-API suite passed |
| Dalton the 2nd | W01 Node SDK CLI native CI gate | `.github/workflows/ci.yml` | Integrated as `d28f31a`; opt-in Linux/macOS native gate with bounded timeouts and prerequisite probes; hosted results remain unverified |
| Mendel the 2nd | W01 Rust FUSE advanced-operation validation | `transports/mount-rs-fuse/{src/session.rs,tests/session.rs}` | Integrated as `1a8b112`; published through `7c6186f`; strict FALLOCATE/RENAME2/LSEEK/COPY_FILE_RANGE framing, explicit EINVAL/ENOSYS boundaries, no-mutation/session-survival tests and strict Clippy passed |
| Ramanujan the 2nd | W01 napi-rs FUSE xattr codecs | `integrations/mount-rs-napi/**` | Integrated as `cf7a132`; published through `4f484ad`; SETXATTR/GETXATTR/LISTXATTR/REMOVEXATTR pinned-oracle differentials, generated artifacts/declarations, typecheck, build and full focused suite passed |
| Zeno the 2nd | W01 Unstorage capability boundary parity | `tests/unstorage/**` | Integrated as `e0e8195`; published through `9b74c87`; 14 rows passed with 5 supported, 9 explicit ENOSYS, zero ENOTSUP mismatches and zero skips |
| Kant the 2nd | W01 Unstorage hardlink capability boundary | `tests/unstorage/**` | Integrated as `99c7d32`; published through `2bce444`; 4 rows passed with 0 supported, 4 exact ENOSYS, zero ENOTSUP mismatches and zero skips |
| Main | W01 Rust FUSE IOCTL session framing | `transports/mount-rs-fuse/{src/session.rs,tests/session.rs}` | Integrated as `f1872f8`; live source/test blobs verified; strict 32-byte header and declared-input-size framing, malformed/trailing `EINVAL`, valid-request `ENOSYS`, and no-mutation coverage passed in 12 focused tests and strict scoped Clippy |
| Main | W01 Rust FUSE IOCTL typed wire parity | `transports/mount-rs-fuse/{src/protocol.rs,tests/protocol.rs}` | Current packet: public typed request/reply codecs now preserve the declared inline input payload, reject truncation/trailing/length mismatches, and retain the native session `ENOSYS` boundary; focused protocol suite passed 12/12, while N-API/strict Clippy/Linux-target and hosted native gates remain to be rechecked |
| Meitner the 2nd | W01 napi-rs FUSE IOCTL codecs | `integrations/mount-rs-napi/**` | Integrated as `32ddee3`; published sequentially through `8ea5f38`; build, typecheck, focused pinned-oracle raw-layout differential, and the full oracle-enabled N-API suite passed |
| Pasteur the 2nd | W01 napi-rs FUSE BMAP codecs | `integrations/mount-rs-napi/**` | Integrated as `387940b`; published sequentially through `091ddcf`; Rust/N-API release build, typecheck, protocol-minor/truncation/trailing/wrong-shape oracle differentials, and the full oracle-enabled N-API suite passed |
| Main | W01 napi-rs FUSE GETLK/SETLK/SETLKW codecs | `integrations/mount-rs-napi/**` | Current packet: generated bindings/declarations, explicit ESM/CommonJS exports, typecheck, pinned-oracle request/reply/error-boundary differential, release build, focused locked FUSE tests and full oracle-enabled N-API suite passed; native FUSE session/mount remains open |
| Main | W01 N-API 9P fid table/session parity | `integrations/mount-rs-napi/**`, `transports/mount-rs-9p/**`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: Rust-backed `FidTable`/live `P9Session.fids`, qid/cursor/open-handle views, hardlink/release and live-open coverage; generated typecheck, build, 124-constant/44-codec differentials, focused N-API tests, 30 ordinary 9P tests, and strict Clippy passed; driver/assertion/debug, lock-option, property-shaped clients, mount-helper and hosted revision gates remain open |
| Main | W01 N-API 9P driver and observability parity | `integrations/mount-rs-napi/**`, `transports/mount-rs-9p/**`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: live `P9Session.driver`, debug-gated assertion readback/counters, request-error/assertion callbacks, Node error revival, and root/`./9p` factory identity; release build, generated typecheck, focused N-API tests, 31 ordinary 9P tests, formatting, and strict Clippy passed; lock-option, property-shaped clients, mount-helper, and hosted revision gates remain open |
| Main | W01 N-API 9P property-shaped clients parity | `integrations/mount-rs-napi/**`, `transports/mount-rs-9p/**`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: `P9Server.clients` is now a generated/property-shaped live array combining native and attached connections; release build, generated typecheck, host-enabled server integration, P9 runtime checks, focused Rust tests, formatting, and strict Clippy passed; lock-option, mount-helper, and hosted revision gates remain open |
| Main | W01 N-API 9P lock-table option injection parity | `integrations/mount-rs-napi/**`, `transports/mount-rs-9p/**`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: `P9ServerOptions.locks` accepts a `P9LockTable`, and injected ranges are shared with native/attached protocol sessions and visible through server/session option handles; release build, generated typecheck, host-enabled server integration, P9 runtime checks, focused Rust tests, formatting, and strict Clippy passed; mount-helper and hosted revision gates remain open |
| Main | W01 N-API 9P mount-created server policy | `integrations/mount-rs-napi/**`, `transports/mount-rs-9p/**`, `transports/mount-rs-auto/**`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: direct and automatic 9P mount options map scalar server policy and injected lock tables to mount-created listeners, preserving the prior `./9p` probe/refusal/option/helper and configured shared-server behavior; Rust/N-API mapping tests, generated typecheck/build, focused runtime checks, host-enabled server integration, formatting and strict Clippy passed; process signals, remaining mount controls and hosted N-API native-mount lifecycle evidence remain open |
| Main | W01 N-API 9P mount-created session callbacks | `integrations/mount-rs-napi/**`, `transports/mount-rs-9p/**`, `transports/mount-rs-auto/**`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: direct and automatic 9P mount options carry `onError`/`onAssertion` into mount-created listeners through the existing Rust session-hook path, while configured shared servers retain their own callbacks; debug build, generated typecheck, focused runtime checks, host-enabled server integration, N-API/Rust tests, formatting and strict Clippy passed; process signals, remaining mount controls and hosted N-API native-mount lifecycle evidence remain open |
| Main | W01 N-API 9P direct-facade signal teardown | `integrations/mount-rs-napi/p9.cjs`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/p9-mount-helpers.mjs`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: direct `./9p` mounts support `signals` with process-wide `SIGINT`/`SIGTERM` cleanup, unmount-all dispatch, last-mount handler removal, and default-signal re-raise; the signal and mount-helper regressions plus syntax/diff checks passed; automatic cross-transport signal ownership, remaining mount controls and hosted N-API native-mount lifecycle evidence remain open |
| Main | W01 hosted N-API 9P native lifecycle gate | `.github/workflows/native-9p.yml`, `integrations/mount-rs-napi/package.json`, `integrations/mount-rs-napi/test/p9-native.mjs`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: run `35664614270`, N-API job `106547449823`, at exact SHA `1dcf4dee4d01fb5e3807335579659b54efd74351` passed `9p`/`9pnet_fd` probing, addon build, automatic and direct `./9p` mounted I/O/cleanup, and structural-driver mounted I/O/cleanup; the Rust `native-9p` job `106547449501` also passed; automatic cross-transport signal ownership, remaining mount controls, and supervisor-owned crash/reset/half-close recovery remain outside this acceptance slice, so production remains NO-GO |
| Main | W01 N-API 9P return-shape and direct-option parity | `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/test/p9-native.mjs`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: `P9Mount.source` is declared as `string`, compile-time checked, and asserted non-empty by the hosted direct mount; generated typecheck, syntax, helper, and diff checks passed locally. Exact SHA `3c884bd8c0d0199a17e4c355c36d45f660c7c786` passed Native 9P run `35665824215`, N-API job `106552944097`, with automatic/direct/structural mounted I/O and cleanup, and Rust job `106552944349` passed all four ignored native tests; the pinned direct `MountP9Options` audit found no additional unrepresented fields. Automatic cross-transport signal ownership and supervisor-owned crash/reset/half-close recovery remain outside this acceptance slice, so production remains NO-GO |
| Main | W01 N-API 9P public barrel/default parity | `integrations/mount-rs-napi/p9.cjs`, `integrations/mount-rs-napi/postlude-p9-codec.cjs`, `integrations/mount-rs-napi/test/p9-constants.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `docs/W01_9P_PROGRESS.md` | Current bounded packet: the direct facade, postlude binding, and generated declarations expose the six pinned oracle defaults; the runtime parity test passes all 124 constants and all 274 upstream 9P barrel exports, with local typecheck, syntax, helper, and diff checks green. Exact SHA `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` passed Native 9P run `35668145703`, N-API job `106558367429`, with automatic/direct/structural mounted I/O and cleanup, and Rust job `106558367006` passed all four ignored native tests; automatic cross-transport signal ownership, native listener `stream: undefined`, supervisor-owned crash/reset/half-close recovery, and broader W01 gates remain explicit, so production remains NO-GO |
| Main | W01 N-API 9P member representation boundaries | `integrations/mount-rs-napi/test/p9-session-metadata.mjs`, `integrations/mount-rs-napi/test/p9-native.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: local metadata checks prove effective callback hooks are omitted from serializable option snapshots while attached connections retain the supplied Node `Duplex`, peer, closed state, and string/null server views; the direct native test now asserts the native listener's `stream: undefined` and transport-source peer string. Local typecheck, metadata, mount-helper, syntax, and diff checks passed. Hosted run `35669536706` at exact SHA `03529cf30985c2be6503c2909b94e646565cf6fe` exposed the test's incorrect native-Unix `peer: null` expectation; corrected exact SHA `81cc6596c2c9562c3405df50126239a7bcb44f63` passed Native 9P run `35670279904` with N-API job `106565351978` and Rust job `106565352174`, so this representation boundary is hosted-qualified; production remains NO-GO for the broader outstanding gates |
| Main | W01 N-API 9P optional absence-shape parity | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/index.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/test/p9-session-metadata.mjs`, `integrations/mount-rs-napi/test/p9-locks.mjs`, `integrations/mount-rs-napi/test/p9-native.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: the JavaScript facade converts native `null` absence results to oracle-compatible `undefined` for session `msize`/`version`/`userFor` and table/session `getlock`; declarations and focused runtime/type/syntax/diff checks pass locally. Exact SHA `0d520a1d0a9af44e08e65c5f0638a640bb3c08db` passed Native 9P run `35671509538`, N-API job `106569412372` with automatic/direct/structural mounted I/O and cleanup plus direct native session/lock assertions, and Rust job `106569412047` with all four ignored tests; production remains NO-GO for the broader open gates |
| Main | W01 N-API 9P platform-probe argument parity | `integrations/mount-rs-napi/p9.cjs`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/test/p9-mount-helpers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: the direct `./9p` facade accepts oracle-compatible optional platform arguments for `p9ClientProbe(platform?)` and `p9Platform(platform?)`; the no-argument probe remains native-backed, while override calls report deterministic Linux/non-Linux facts without attempting a mount. Typecheck and the host-independent mount-helper regression pass locally. Exact SHA `9da45327a9e09a9f827a9630869d1a32119674e3` also passed Native 9P run `35672845113`, N-API job `106573050491` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106573049500` with all four ignored native tests; production remains NO-GO |
| Main | W01 N-API 9P session message-statistics shape parity | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/index.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/test/p9-observability.mjs`, `integrations/mount-rs-napi/test/p9-native.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: `P9Session.stats.messages` now normalizes the native object/hash-map to the oracle's `Map<string, number>`; attached-session observability and session-metadata regressions, generated typecheck, syntax, and diff checks pass locally, with the direct native mount asserting `Map#get("Tversion")`. Exact SHA `e8c6043827e6cd0232a28f94b8fc665e25985f76` passed Native 9P run `35673543701`, N-API job `106575123928` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106575123716` with all four ignored native tests; production remains NO-GO |
| Main | W01 N-API 9P fid-view declaration and representation parity | `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/test/p9-fids.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: the direct `./9p` fid declaration now matches the native N-API view, with array-shaped `{ offset: bigint, index: number }` cursor offsets and writable `Fid.iounit`/`Fid.cursor`; `node test/p9-fids.mjs`, generated typecheck, syntax, and diff checks pass locally. Exact SHA `ba20d29d7e8ad00b3c4b5270dc21cf6ab913e4c2` passed N-API job `106578252549` and Rust job `106578252700` in Native 9P run `35674581481`; the N-API job passed the Linux probe and automatic/direct/structural mounted-I/O/cleanup checks, and the Rust job passed the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P dirent-packer `maxSize` parity | `integrations/mount-rs-napi/src/p9_codec.rs`, `integrations/mount-rs-napi/index.d.ts`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/9p-codec.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: the native `P9DirentPacker` now exposes the pinned oracle's fixed `maxSize` getter; generated/direct declarations and the 44-case codec differential assert the getter and `size + remaining` invariant after packing. Typecheck, fid/runtime, syntax, formatting, strict Clippy, and focused Rust tests passed locally. Exact SHA `b3757fd288e6f52888873838946343e7cd37f953` passed Native 9P run `35675876913`, N-API job `106582464900` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106582465059` with the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P `framesFrom` iterable/assembler parity | `integrations/mount-rs-napi/postlude-p9-codec.cjs`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/9p-codec.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: `framesFrom` now accepts synchronous or asynchronous byte iterables and an optional caller-owned `P9FrameAssembler`, matching the pinned oracle's shared-limit seam. The 9P differential, generated typecheck, fid/runtime, syntax, and diff checks passed; the full N-API script reached all 9P checks before the unrelated NFS relisten phase hit sandbox `Operation not permitted`. Exact SHA `412c422e2485a5c7ce2caf55892ec6475faab8d8` passed Native 9P run `35676832586`, N-API job `106585007802` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106585007667` with the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P bounded-reader maximum parity | `integrations/mount-rs-napi/postlude-p9-codec.cjs`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/9p-codec.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: `readRread`, `readTwrite`, and `readRreaddir` now preserve the pinned oracle's optional maximum-item argument, including default `P9_MAX_ITEM` and bounded success/error behavior. The 44-case differential, generated typecheck, syntax, fid/runtime, diff, formatting, strict Clippy, and 18 focused Rust tests passed locally. Exact SHA `bba379ebe4339e951de9cb7ca02b4b499c3a3874` passed Native 9P run `35677755888`, N-API job `106587739639` with the Linux probe, addon build, automatic/direct/structural mounted I/O and cleanup, and Rust job `106587739402` with the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P typed-reader maximum parity | `integrations/mount-rs-napi/src/p9_codec.rs`, `integrations/mount-rs-napi/index.d.ts`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/9p-codec.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md` | Current bounded packet: native `P9Reader.readRread`, `readTwrite`, and `readRreaddir` now preserve the same optional maximum-item argument as the public helpers. The helper/typed-reader differential, generated declarations, typecheck, syntax, fid/runtime, diff, formatting, strict Clippy, and 18 focused Rust tests passed locally. Exact SHA `4ecdb63db64711e0fadf77d4612b6394f57f3f4d` passed Native 9P run `35678757675`, N-API job `106590841909` with the Linux probe, addon build, automatic/direct/structural mounted I/O and cleanup, and Rust job `106590841982` with the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P synchronous direct live-mount parity | `integrations/mount-rs-napi/p9.cjs`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/test/p9-mount-helpers.mjs`, `integrations/mount-rs-napi/test/p9-native.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet: direct `./9p` `live9pMounts()` now returns the oracle-compatible synchronous `Array<P9Mount>`; its process-local registry tracks direct `mount9p()` results regardless of `signals`, prunes inactive mounts, and removes closed mounts. The root all-transport `liveMounts()` registry remains asynchronous. Local addon build, helper/typecheck/syntax, 44-case codec, fid/session/observability, formatting, strict Clippy, and 18 focused Rust tests passed. Exact SHA `56291e3f9b4274fec2111e4e2f88696e98f3a548` passed Native 9P run `35679754417`, N-API job `106593941892` with the Linux probe, addon build, automatic/direct/structural mounted I/O and cleanup, and Rust job `106593942012` with the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P direct probe absence-shape parity | `integrations/mount-rs-napi/p9.cjs`, `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `integrations/mount-rs-napi/test/p9-mount-helpers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet: direct `./9p` `p9ClientProbe()` now always owns `platform` and `reason`, normalizing native `null`/omitted values to `undefined`; the direct declaration requires `platform: "linux" | undefined` and `reason: string | undefined`, while the root automatic `JsP9ClientProbe` boundary remains unchanged. Local addon/generated build, helper, required-field typecheck, syntax, and diff checks passed. Exact SHA `7389be4d5ea4930075cf5278032614e931054620` passed Native 9P run `35680542975`, N-API job `106596362070` with the Linux probe, addon build, automatic/direct/structural mounted I/O and cleanup, and Rust job `106596362200` with the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P direct `P9Platform` type export parity | `integrations/mount-rs-napi/types/p9-codec.d.ts`, `integrations/mount-rs-napi/test/types.test.ts`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet: direct `./9p` now exports the oracle's type-only `P9Platform = "linux"` alias and uses it in `P9ClientProbe` and `p9Platform()`; runtime behavior is unchanged. The direct type-import/use check, helper, syntax, and diff checks passed. Exact SHA `2bcd9aa4b0d25f284d8ae9fc4ad3de0a5cbbfeff` passed Native 9P run `35681127657`, N-API job `106598109004` with the Linux probe, addon build, automatic/direct/structural mounted I/O and cleanup, and Rust job `106598109187` with the Linux probe plus all four ignored native lifecycle tests; production remains NO-GO |
| Main | W01 N-API 9P `P9User.uid` object-shape parity | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/postbuild.mjs`, `integrations/mount-rs-napi/index.d.ts`, `integrations/mount-rs-napi/test/p9-session-metadata.mjs`, `integrations/mount-rs-napi/test/servers.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet: the direct/root N-API `P9User` facade preserves the oracle-required own `uid` property, using `undefined` when native identity has no numeric uid; generated direct declarations use `uid: number | undefined`. Local `pnpm build:debug`, session metadata, generated typecheck, syntax, and diff checks passed. Exact SHA `1c43f66ec570be35444055ab6adb0f841628fef6` passed Native 9P run `35681672318`, N-API job `106599754171` with automatic/direct/structural mounted I/O and cleanup, and Rust job `106599753872` with all four ignored native lifecycle tests; the broad `node test/servers.mjs` script remains separately blocked by the Darwin NFS relisten sandbox `Operation not permitted` before 9P, so production remains NO-GO |
| Main | W01 N-API 9P transport teardown under backpressure | `transports/mount-rs-9p/src/server.rs`, `integrations/mount-rs-napi/test/servers.mjs`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet fixes the connection task's permit-wait deadlock by retaining a bounded pending-frame queue while continuing to read for peer EOF, and reports a frame transport failure if that queue is exceeded. Real loopback N-API coverage now checks server close with an open fid, paused-peer FIN with a large queued reply burst, silent TCP reset, and orderly client EOF. Local focused Rust tests (36 passed), strict Clippy, addon rebuild, syntax/diff checks, and elevated isolated N-API execution passed. Exact SHA `1179d9e3fbdb95ea1cca9866fd249c949614a9e1` passed Native 9P run `35685073733`, N-API job `106610049913` with teardown and automatic/direct/structural mounted-I/O cleanup, and Rust job `106610049705` with the Linux probe plus all four ignored native lifecycle tests; process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P TCP connection isolation and dispatch ordering | `integrations/mount-rs-napi/test/servers.mjs`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds real loopback coverage for two native listener connections using distinct sessions but the same fid number, survivor service after one client closes, and a slow-open/fast-getattr burst that replies in completion order. Local syntax/diff checks and elevated isolated N-API execution passed. Exact SHA `9870d58cfbed5bcea90972c4b9caaf5db3075cef` passed Native 9P run `35685807744`, N-API job `106612633937` with the full server/attach, teardown, and automatic/direct/structural mounted-I/O cleanup gates, and Rust job `106612633771` with the Linux probe plus all four ignored native lifecycle tests; shared lock-table network behavior, remote-admission/interface qualification, remaining framing/payload/port cases, process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P shared network lock table | `integrations/mount-rs-napi/test/servers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds real loopback coverage for two native TCP sessions sharing one configured `P9LockTable`: the first write lock succeeds, the second receives `P9_LOCK_BLOCKED` and reads the first holder through `Tgetlock`, and closing the first connection releases the range for the second. Local `git diff --check` and elevated isolated N-API execution passed. Exact SHA `514d2c533382b927c059ccd946f3e566d2c371a9` passed Native 9P run `35686403815`, N-API job `106614048924` with the shared-lock, server/attach, teardown, and automatic/direct/structural mounted-I/O cleanup gates, and Rust job `106614048722` with the Linux probe plus all four ignored native lifecycle tests; remote-admission/interface qualification, remaining framing/payload/port cases, process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P listener boundary failures | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/test/servers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet maps occupied-port listener failures to Node-compatible `EADDRINUSE` with platform errno/syscall and adds real loopback malformed-frame isolation: a size-1 frame closes only the broken connection while a healthy survivor reads. Local `git diff --check` and elevated isolated N-API execution passed. Exact SHA `2a3ccfa9a77cab22d154d041627369d995ba74d5` passed Native 9P run `35687145769`, N-API job `106616293297` with boundary plus full lifecycle, shared-lock, teardown, and automatic/direct/structural mounted-I/O cleanup, and Rust job `106616293187` with the Linux probe plus all four ignored native lifecycle tests; remote-admission/interface qualification, large-payload/negotiated-msize cases, process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P multi-frame payload and negotiated `msize` | `integrations/mount-rs-napi/test/servers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds real loopback coverage for a deterministic 256 KiB payload split across many negotiated-8 KiB frames, plus rejection of an oversized inbound frame at the negotiated `msize` while a second session remains healthy. Local syntax/diff checks and elevated isolated N-API execution passed. Exact SHA `c42030c1807f6504660892bf829137897e910c5e` passed Native 9P run `35687955065`, N-API job `106618714142` with wire-framing plus full lifecycle, shared-lock, teardown, and automatic/direct/structural mounted-I/O cleanup, and Rust job `106618713939` with the Linux probe plus all four ignored native lifecycle tests; remote-admission/interface qualification, process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P interface-qualified remote admission | `integrations/mount-rs-napi/test/servers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds the oracle-shaped external-IPv4 check: when an actual non-loopback IPv4 interface exists, a peer sourced from it is rejected by the default loopback-only policy, reports one peer-qualified transport error, and leaves zero live connections; no-interface hosts skip this environmental case explicitly. Local syntax/diff checks and elevated isolated N-API execution passed against the actual external interface. Exact SHA `87ccd68c8e2040c90037c6027eb4467b1a7bd42d` passed Native 9P run `35688474092`, N-API job `106620236951` with remote-admission plus full lifecycle, wire-framing, shared-lock, teardown, and automatic/direct/structural mounted-I/O cleanup, and Rust job `106620236776` with the Linux probe plus all four ignored native lifecycle tests; process-crash and arbitrary kernel-reset recovery, explicit `allowRemote: true` network admission, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P remote-admission opt-in | `integrations/mount-rs-napi/test/servers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds the interface-qualified `allowRemote: true` path: on a host with an external IPv4 interface, a peer sourced from that address completes 9P version/attach and serves `Tgetattr` with the expected peer and negotiated `msize`; no-interface hosts skip this environmental case explicitly. Local syntax/diff checks and elevated isolated N-API execution passed against the actual external interface. Exact SHA `2f0e23a4bc5ba79fef61426137da365cbcd55f42` passed Native 9P run `35688865494`, N-API job `106621392719` with opt-in/default admission, wire-framing, full lifecycle, shared-lock, teardown, and automatic/direct/structural mounted-I/O cleanup, and Rust job `106621392818` with the Linux probe plus all four ignored native lifecycle tests; process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P direct constructible session boundary | `integrations/mount-rs-napi/src/servers.rs`, `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/index.d.ts`, `integrations/mount-rs-napi/package.json`, `integrations/mount-rs-napi/test/p9-session.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds constructible `new P9Session(driver, options?)` over native `Filesystem` input, optional scalar policy/shared locks/request-error and assertion hooks, direct frame/error behavior, serializable effective options, and callback-handle release on `destroy()`. Local addon rebuild, 36 focused `mount-rs-9p` tests, strict Clippy, generated typecheck, direct-session/metadata/observability checks, syntax/diff checks, and real-socket 9P selector passed. Exact SHA `1e9fffe0f2765e0be17c1a2e394b70c05dea112e` passed Native 9P run `35690887775`, N-API job `106627448717` with the Linux probe, addon build, isolated server/attach selector, direct session lifecycle, and automatic/direct/structural mounted-I/O cleanup, and Rust job `106627448526` with the Linux probe plus all four ignored native lifecycle tests; direct structural `FsDriver` adaptation, process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P structural direct session adaptation | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/postbuild.mjs`, `integrations/mount-rs-napi/index.d.ts`, `integrations/mount-rs-napi/test/p9-session.mjs`, `integrations/mount-rs-napi/test/types.test.ts`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Follow-up bounded packet extends constructible `new P9Session(driver, options?)` to structural `FsDriver` input through the existing adapter, retains the adapter until direct-session `destroy()`, releases it once, and keeps the caller-owned structural source alive; native `Filesystem` input remains supported without an owned adapter. Local addon rebuild, generated typecheck, direct-session/metadata/observability checks, syntax/diff checks, and real-socket 9P selector passed. Exact SHA `496ed42b3cfaca4a379f7e061d24bd27b5e23372` passed Native 9P run `35691732267`, N-API job `106629973640` with the Linux probe, addon build, isolated server/attach selector, direct native/structural session lifecycle, and automatic/direct/structural mounted-I/O cleanup, and Rust job `106629973787` with the Linux probe plus all four ignored native lifecycle tests; process-crash and arbitrary kernel-reset recovery, broader parity, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P direct-session state-machine cancellation | `transports/mount-rs-9p/src/session.rs`, `integrations/mount-rs-napi/test/p9-session.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet fixes stale-generation precedence over late provider errors and proves direct `Tflush` wakeup, `Tversion` reset with stale `EIO`, and `destroy()` with stale `ENODEV`, including fid/user reset and caller-owned structural-source survival. Local addon rebuild, direct session/metadata/observability/fid/mount-helper checks, generated typecheck, syntax, elevated server selector, full `mount-rs-9p` target (36 tests), strict Clippy, formatting, and diff checks passed. Exact SHA `c627761721b982f78fd33942f7287753fad972f8` passed Native 9P run `35693518562`, N-API job `106635345169` with the direct state-machine and automatic/direct/structural mounted-I/O cleanup gates, and Rust job `106635344863` with the Linux probe plus all four ignored native lifecycle tests; process-crash and arbitrary kernel-reset recovery, broader upstream parity, automatic cross-transport signal ownership, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P direct-session member-surface audit | `integrations/mount-rs-napi/test/p9-session.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds an oracle-backed runtime audit for the native direct `P9Session` semantic member set: `driver`, `options`, `fids`, `locks`, `stats`, `assertions`, `msize`, `version`, `generation`, `inflight`, `destroyed`, `userFor`, `handleCall`, and `destroy`. Local direct/oracle execution plus adjacent metadata/observability/fid/mount-helper checks, generated typecheck, syntax, and diff checks passed. Historical SHA `fb532b46fd8b6c5af66dc9b771e84116b2997ca3` had the Rust external-umount failure in run `35694841984`; current published SHA `86b88c329d64bcc2a8e7b9d97993fca657458986` reran the direct session/member path in N-API job `106667799214` and passed it, while Rust job `106667799016` passed all four ignored native lifecycle tests; the historical failure is superseded for the current supported slice. Broader protocol/session parity, process-crash and arbitrary kernel-reset recovery, automatic cross-transport signal ownership, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P server/connection member-surface audit | `integrations/mount-rs-napi/test/p9-server-members.mjs`, `integrations/mount-rs-napi/package.json`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds an oracle-backed runtime audit for the ten semantic `P9Server` members and the declared attached-connection surface, including `waitClosed()`; the oracle's non-interface `drop()` helper is explicitly excluded. Local direct/oracle execution, adjacent metadata/typecheck checks, syntax, and diff checks passed. Exact SHA `75c149f857f6056a7435a80a1493a9dfc8e53f59` passed Native 9P run `35697338227`: N-API job `106647016617` passed the new member-surface step plus all hosted N-API lifecycle gates, and Rust job `106647016767` passed the Linux probe plus all four ignored native lifecycle tests. Broader protocol/session parity, process-crash and arbitrary kernel-reset recovery, automatic cross-transport signal ownership, and W01 acceptance remain open, so production remains NO-GO |
| Main | W01 N-API 9P native server client identity | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/test/p9-server-identity.mjs`, `integrations/mount-rs-napi/package.json`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Exact SHA `15cb940988913c666d8d592a583e7eb3d2d82241` caches native `P9Connection` wrappers by stable transport id, preserving repeated `P9Server.clients` connection/session/closed identity and pruning wrappers after transport removal. The real-TCP regression covers registration, stable views, `close()`/`waitClosed()`, and cleanup. [Native 9P run `35698924766`](https://github.com/andymac4182/mount-rs/actions/runs/35698924766) passed N-API job `106652302954` with the new identity step and all hosted N-API lifecycle gates, and Rust job `106652303250` with the Linux probe plus all four ignored native lifecycle tests; broader parity and production remain NO-GO |
| Main | W01 N-API 9P mixed native/attached client arrival order | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/test/p9-server-order.mjs`, `integrations/mount-rs-napi/package.json`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Exact SHA `a3554b105be58cc5ff6cacc03de53f09c0461419` adds one JS arrival ledger for native and attached `P9Server.clients`, observes accepted native clients before `attach()`, preserves stable native wrappers, and prunes closed entries. The real-TCP regression covers native-first and attached-first order plus cleanup. Local syntax, focused order/identity/member checks, metadata/session/observability/type checks, and the elevated `p9` selector passed. Published SHA `86b88c329d64bcc2a8e7b9d97993fca657458986` passed [Native 9P run `35703805373`](https://github.com/andymac4182/mount-rs/actions/runs/35703805373): N-API job `106667799214` passed the mixed arrival-order check and all adjacent lifecycle gates, and Rust job `106667799016` passed the Linux probe plus all four ignored native lifecycle tests; broader parity and production remain NO-GO |
| Main | W01 N-API 9P native connection close idempotence | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/test/p9-server-connection-lifecycle.mjs`, `integrations/mount-rs-napi/package.json`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Exact SHA `3260f84e26c2a78e9d10d66c7eb997477130f695` memoizes the native `P9Connection.close()` promise at the JavaScript boundary, preserving concurrent/repeated/post-closure idempotence. The real-TCP regression covers concurrent calls, `closed`/`waitClosed()`, terminal `isClosed`, client removal, and cleanup. Local syntax, diff, focused close/order/identity/member checks, metadata/session/observability/type checks, and the elevated `p9` selector passed. Published SHA `86b88c329d64bcc2a8e7b9d97993fca657458986` passed [Native 9P run `35703805373`](https://github.com/andymac4182/mount-rs/actions/runs/35703805373): N-API job `106667799214` passed the native close-idempotence check and all adjacent lifecycle gates, and Rust job `106667799016` passed the Linux probe plus all four ignored native lifecycle tests; broader parity and production remain NO-GO |
| Main | W01 N-API 9P mounted view identity | `integrations/mount-rs-napi/postlude-servers.cjs`, `integrations/mount-rs-napi/test/p9-native.mjs`, `.github/workflows/native-9p.yml`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Exact SHA `1a18c7b82285ea557956cb35d15f1af189803d4d` caches `Mounted.server` and `Mounted.connection` wrappers and reuses the matching `P9Server.clients` wrapper by stable transport id. The direct native-mount regression covers repeated getter identity, cross-view connection identity, native stream/peer/session views, and cleanup. Local syntax, focused lifecycle checks, metadata/session/observability/type checks, and the elevated 9P selector passed; published SHA `86b88c329d64bcc2a8e7b9d97993fca657458986` passed [Native 9P run `35703805373`](https://github.com/andymac4182/mount-rs/actions/runs/35703805373): N-API job `106667799214` passed direct mounted-I/O/cleanup and all adjacent lifecycle gates, and Rust job `106667799016` passed the Linux probe plus all four ignored native lifecycle tests; broader parity and production remain NO-GO |
| Main | W01 9P supported-scope closure audit | `docs/W01_9P_PROGRESS.md`, `docs/W01_PROGRESS.md`, `docs/public-api-parity.md`, `transports/mount-rs-9p/README.md` | Current audit classifies the advertised codec/session/server/connection/attach/mount slice as qualified by local evidence and published SHA `86b88c329d64bcc2a8e7b9d97993fca657458986` / [Native 9P run `35703805373`](https://github.com/andymac4182/mount-rs/actions/runs/35703805373), N-API job `106667799214`, and Rust job `106667799016`. Legacy/auth/xattr families, unadvertised upstream members, native-listener Node-stream identity, root automatic cross-transport signals, process-crash/arbitrary kernel-reset recovery, and non-Linux native mounts are explicit scope boundaries; broader oracle parity remains partial by design and overall W01/release remains NO-GO |
| Main | W01 dedicated hosted upstream 9P conformance gate | `.github/workflows/native-9p.yml`, `tests/upstream/p9-conformance.test.mjs`, `examples/p9_oracle.rs`, `docs/W01_9P_PROGRESS.md`, `docs/W01_PROGRESS.md` | Published SHA `a6b3e2aa10cfdb3ee730d41c5886436b02c260de` adds the revision-matched Linux `upstream-9p` job and routes the fixture through `scripts/cargo-shared`. [Native 9P run `35707546973`](https://github.com/andymac4182/mount-rs/actions/runs/35707546973) passed upstream job `106680012604` with `144 passed` and `2 skipped` root-gated ownership cases out of `146`; N-API job `106680012485` and Rust job `106680013203` also passed. Local YAML/syntax, focused oracle/public-surface, and diff checks passed; local full-suite execution is blocked before tests by the Mac's unaccepted Xcode license. The documented legacy/auth/xattr, broader upstream member, supervisor-owned crash/reset, and non-Linux native-mount boundaries remain explicit, so production remains NO-GO |
| Main | W01 root-gated upstream 9P ownership coverage | `.github/workflows/native-9p.yml`, `tests/upstream/p9-conformance.test.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/W01_PROGRESS.md` | Follow-up exact head SHA `d11f458d7f6ef1923091fbca84a93e63f05e9455` adds the privileged `upstream-9p-root` companion job while preserving `CI`, Rustup, Cargo, and the explicit temporary target under `sudo`. [Native 9P run `35711056768`](https://github.com/andymac4182/mount-rs/actions/runs/35711056768) passed baseline job `106691472428` with `144 passed` and `2 skipped`, root job `106691917345` with `146/146`, N-API job `106691472270`, and Rust job `106691472165`; both previously root-gated symlink-ownership cases are now exercised. The explicit protocol/member/supervisor/non-Linux boundaries and overall production NO-GO remain unchanged |
| Main | W01 9P platform scope gate | `transports/mount-rs-9p/README.md`, `transports/mount-rs-9p/src/mount.rs`, `docs/W01_9P_PROGRESS.md`, `docs/W01_PROGRESS.md` | The host `aarch64-apple-darwin` rootless `mount-rs-9p` all-target suite passed `37` tests with zero failures, and the Rust crate passed the all-target `x86_64-pc-windows-gnu` compile check. This qualifies macOS rootless execution and Windows Rust compile portability only; Windows runtime/N-API/Unix-listener/native-mount acceptance is not claimed. Linux native mounts remain qualified only by hosted kernel-client gates, so production scope stays Linux native plus tested macOS/Linux rootless wire/TCP and overall W01 remains NO-GO |
| Main | W01 current-head hosted 9P rerun after shared N-API build change | `.github/workflows/native-9p.yml`, `integrations/mount-rs-napi/package.json`, `integrations/mount-rs-napi/scripts/build-native.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/W01_PROGRESS.md` | Exact head SHA `38346fea42b7950e2599103a477d804808262e2b` passed [Native 9P run `35712873612`](https://github.com/andymac4182/mount-rs/actions/runs/35712873612): baseline job `106697612285` passed `144` with `2` skips out of `146`, root job `106698277708` passed `146/146`, N-API job `106697612716` passed the complete Linux 9P lifecycle, and Rust job `106697612506` passed the native lifecycle. This verifies current source after the shared macOS N-API addon-build change; the explicit 9P scope boundaries and overall production NO-GO remain unchanged |
| Main | W01 9P supported-scope completion audit | `docs/W01_9P_PROGRESS.md`, `docs/W01_PROGRESS.md`, `WORK_TRACKER.md`, `transports/mount-rs-9p/README.md` | Current-head evidence closes the documented 9P slice: public/session/connection/attach/native/concurrency/cancellation gates are PASS, while crash/reset recovery, broader upstream members, automatic cross-transport signal ownership, Windows runtime/N-API, and non-Linux native mounts are explicit boundaries. No 9P-scoped implementation, test, or workflow files changed after exact head `38346fea42b7950e2599103a477d804808262e2b` and [Native 9P run `35712873612`](https://github.com/andymac4182/mount-rs/actions/runs/35712873612). The supported 9P scope is qualified; repository production/release remains NO-GO for the other W01 gates |
| Main | W01 N-API 9P Unix listener policy and lifecycle | `.github/workflows/native-9p.yml`, `integrations/mount-rs-napi/test/servers.mjs`, `docs/W01_9P_PROGRESS.md`, `docs/public-api-parity.md`, `docs/W01_PROGRESS.md` | Current bounded packet adds the Unix-domain listener phase to `MOUNT_RS_SERVER_PHASE=p9`, independently exercising private-directory refusal, explicit `allowSharedDirectory` opt-in, `0600` socket mode, protocol handshake, native Unix peer/path and `stream: undefined` representation, socket removal on close, and path/port exclusivity. Local syntax/diff checks and elevated isolated N-API execution passed. Exact test commit `dd10ac0564446c9143f8b5f68b2fed51c7eaf57f` was included in descendant head `d43f5ea4e4334912de86ac0db818392531a7d4ec`, whose Native 9P run `35683716217` passed N-API job `106606580352` with Unix policy, server/attach, and automatic/direct/structural mounted I/O/cleanup, and Rust job `106606580326` with the Linux probe plus all four ignored native lifecycle tests. The direct run at the test commit was cancelled before jobs materialized and is not evidence; production remains NO-GO |

Closed packets already integrated this cycle include Mendel (FUSE), Lagrange
(Windows host), Epicurus (CLI), Maxwell (FoundationDB), Newton/Astra (R2
design review), Raman (scoped napi-rs package distribution), Aristotle (the
unsigned FSKit bridge checkpoint), Mill (the TiDB provider/harness checkpoint),
Peirce (the SQLite VFS/WAL checkpoint), Russell (CLI/HTTP edge coverage),
Aquinas (Windows CI parity), Cicero (parity audit), and Ohm (storage-dispatch
review). Main
rotates those slots rather than assigning multiple workers to the same files.

### Narrow-band completion order

To reduce half-finished breadth, new worker packets are not opened outside the
current vertical slice until its acceptance gate is green:

1. **P0 runtime path:** core mountx parity, independent metadata/block stores,
   fixed-size chunking, and the shared contract through memfs, SQLite, PGlite
   and authenticated Cloudflare R2.
2. **P0 consumer path:** the same configured drives through napi-rs, the
   config-file CLI and the multi-drive HTTP API, with reopen, range, partial
   write, truncate, concurrency, fault-injection and cleanup evidence.
3. **P0 reliability/platform path:** SQLite hosting/VFS/WAL matrix and
   macOS/Linux qualification, while Windows CI remains a required parallel
   signal and is never treated as passed from Unix evidence.
4. **P1 integration path:** TiDB and FoundationDB metadata with RustFS blocks,
   then FSKit and remaining native acceptance. Existing bounded workers may
   finish their current packets, but they must not grow scope.
5. **P2 evidence/product path:** versioning, benchmarks/compression, AWS/Ozone,
   reference reviews, site/domain and publication hardening.

Distributed cache (W22), physical copy-on-write (W23), lifecycle hooks (W29)
and OTel (W30) remain deferred discussions/features until the P0/P1 gates are
complete.

## Workstream dashboard

| ID | Stream | Status | Current owner |
| --- | --- | --- | --- |
| W01 | Core and mountx parity | Active simple-first; parallel sidecars are integrated as bounded packets; full parity remains open | Main (integration/acceptance) |
| W02 | Metadata/block split and chunking | Verifying; persisted chunker metadata and partial-write/reopen gates landed | Main |
| W03 | Memory and SQLite stores | Landed; extending | Main |
| W04 | PGlite | W04.2 closed; production rollout NO-GO pending external gates | Main |
| W05 | Cloudflare R2 | Immutable candidate `7efded54` is locally green through the current Rust workspace/NFS/S3/Clippy suite, dyld-safe macOS N-API package load, full Node SDK/CLI/N-API, PGlite, provider-matrix, and CLI paths. W04 policy `35714141926` and W08 policy `35714144646` passed; Fault `35714146067` is terminal-successful on all three OSes, Native 9P `35714145176` is terminal-successful including root conformance 146/146, W07 has its compile/durable jobs green with assembly queued, W08 has Linux/macOS builds green with asset verification queued, and CI `35714144497` has a classified Ozone/TiDB hard-capacity failure at 84.01/1000 IOPS plus the isolated macOS artifact finalization timeout and incomplete provider/native jobs. Live R2/AWS remain held behind cap/security gates, and hosted/provider/native/package/provenance/scope/W20.6 closure remains required; production is NO-GO | Main |
| W06 | RustFS integration service | Landed; extending | Lagrange (complete slice) / Main |
| W07 | FoundationDB | Provider/composition and hosted durable RustFS acceptance passed; the latest current-public-main exact-tip terminal cross-platform qualification packet is green at [run `35712676265`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35712676265) / exact source `ff5cc58188d9783a80de96698a187e1f6416b83e`, Linux job `106696766067`, macOS job `106696766203`, aggregate job `106702286990`; all rollout-ledger, production-evidence, workload-artifact and configuration preflights passed, as did Linux durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, authority republish, fresh-client reopen, RustFS integration, authority heartbeat/stats and ten-round soak; macOS emitted `W07_MACOS_FOUNDATIONDB_COMPILE_PASS` plus run-bound provenance on its distinct platform runner; the aggregate emitted `W07_PLATFORM_QUALIFICATION_PASS` with `provenance=bound`; base composition was p50 2,311µs, p95/p99 37,919µs and 164.77 ops/s, ten-round soak p95/p99 was 9,841–17,582µs at 269.41–330.32 ops/s, and the corrected 400-lifecycle/64-concurrency/4KiB workload measured 716.30 lifecycle IOPS with all 1,200 operations successful and zero timeouts/cleanup failures; Linux artifact ID `10688222438`, macOS artifact ID `10688396103` and aggregate artifact ID `10688622703` were retained and independently revalidated. The seven-gate packet remains NO-GO with zero production evidence records. A follow-up cross-platform refresh, run [35716095938](https://github.com/andymacclenaghan/mount-rs/actions/runs/35716095938), was dispatched from exact source `e984c2321317e9d93b8db1ec98dc428662b17d34`; Linux job `106707845243` and macOS job `106707844852` remain queued with no terminal result, so it contributes no evidence yet. Later mainline commits have advanced beyond that tested revision and any result must be evaluated against its exact source binding. This remains hosted qualification only, not live macOS service/cluster/mount, clean-install, signing/package, production capacity, identity/ACL, backup/restore, failover, observability or owner evidence; W07.3, W07.5 and W07.7 remain open. | Maxwell (complete slice) / Main |
| W08 | TiDB | Functional hosted acceptance complete for the defined scope: durable 3PD/3TiKV restart, provider fencing/ambiguous commit, live TiDB/RustFS Node/CLI/FUSE, ARM and macOS/Ubuntu native rows passed; production rollout remains NO-GO with P01–P09 open | Mill (functional checkpoint) / Main; production ownership TBD |
| W09 | Node / napi-rs and public API | Verifying; public Rust SDK, Rust-backed FUSE state, and Node SDK CLI landed; platform/package gaps remain | Main (packets integrated) |
| W10 | FUSE, NFS, 9P, WebDAV, S3 | FUSE codec, lifecycle, ACCESS, INIT and session packets landed; native and cross-platform transport acceptance remains open | Main (packets integrated) |
| W11 | Config-driven CLI | Rust CLI now consumes the Rust SDK; Node CLI consumes the Node SDK; provider/remote/hosted gates remain | Main |
| W12 | Safely hosting SQLite files | Local journal/WAL matrix, fail-closed bridge, and recovery gates landed; hosted/Windows gates pending | Peirce (checkpoint) / Main |
| W13 | macOS FSKit | Unsigned bridge checkpoint passed; activation/signing pending | Aristotle (checkpoint) / Main |
| W14 | Versioned filesystems | Local foundation landed; integration pending | Main |
| W15 | Mount-free SQLite VFS | Rollback, process-local and host-local WAL gates landed; remote/Node/Windows acceptance pending | Peirce (checkpoint) / Main |
| W16 | just-bash / Mastra adapters | Landed locally; hosted verification pending | Main |
| W17 | Multi-drive HTTP server | Server/CLI and local edge-case coverage landed; RustFS remote passed, Cloudflare acceptance pending | Russell (complete slice) / Main |
| W18 | Benchmarks and dependency budget | Partial implementation | Ohm / Main |
| W19 | Compression | Design review recorded | Main |
| W20 | CI, packaging and final acceptance | Verifying | Main |
| W21 | Reference review and learnings | Ongoing | Main |
| W22 | Distributed caching | Deferred for discussion | User / Main |
| W23 | Physical copy-on-write | Future requirement | Unassigned |
| W24 | Domain and marketing site | TanStack Start site deployed; `mount-rs.com` and `www.mount-rs.com` live on Vercel | Meitner (complete slice) / Main |
| W25 | Actual AWS S3 integration | Library/runtime AWS S3 qualification complete for the myroot test bucket and scoped live Rust gate; adopter/reference deployment remains a separate NO-GO track with W25.5-W25.9 open | Main |
| W26 | Apache Ozone S3 backend | W26 qualification harness and local controls are implemented; current published `origin/main` tip is `83e0d3b7` and includes publication-barrier `c7f0e6d0`, bounded mutation-window `183660a4`, lease-renewal `f10dbf22`, lazy-atime/EOF reduction, R2 content-addressing/cache, single-flight upload coalescing `d05548e8`, metadata batching, queue cancellation safety, same-revision concurrent-create inode rebasing, the corrected Ozone content-addressed contract and deduplicated cleanup, Ozone test lockfile fix `0cef5d44`, FoundationDB test lockfile fix `8428a5ef`, SQL publication fast path `214b9a6b` and TiDB session setup optimization `e41bed05`. The customer-deployed Ozone integration track remains **NO-GO**: terminal run `35683158821` on exact SHA `14dbf2c6` measured SQLite/R2 754.59, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17 IOPS against 1,000; all rows completed 1,200/1,200 lifecycle operations with zero timeout/cleanup failures, but aggregate `106606577835` failed closed on missing `OZONE_IOPS_PASS`. Retained artifacts are composition `10676355562`, TiDB `10675867797`, FoundationDB `10675443241` and base Ozone `10675672094`; the next code chunk must preserve the hard threshold and fail-closed packet. Security scans `e44de27d-96d8-4330-a831-b995d6458a6c`, `47641152-6539-48e1-96b6-bf2201033486`, `739f4f5b-8cc4-4154-93f7-9e486745eab1`, `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` and `c4fc0012-b0e2-421c-9db8-ca3edfce730c` have zero reportable findings with complete local coverage; secure customer topology, 99.99%/5-minute objective evidence, native/end-to-end coverage and Ozone-owned DR/release gates remain explicit | Main |
| W27 | Native Windows support and CI | HostFs symlink, read-only create/unlink and hard-link packets landed; hosted runtime and mount qualification pending | Main |
| W28 | Deterministic fault injection | Implementing | Main integration |
| W29 | User-configurable lifecycle hooks | Deferred for later | Unassigned |
| W30 | OpenTelemetry traces, metrics and logs | Implementing: opt-in facade, boundary wiring, local collector/failure tests, benchmark and macOS qualification packet landed; external collector/Linux/Windows evidence pending | Main |
| W31 | Per-drive mounts from one backing datastore | Deferred for future design | Unassigned |

> W26 current-status note: the row above is historical summary text. The
> authoritative current tip, exact hosted packet, every W26 work-item status
> and production NO-GO boundary are in the latest W26 authority override
> below, which records the published FoundationDB transaction-sharing chunk
> and the retained manual qualification boundary.

Current W26 authority override (2026-09-22, FoundationDB transaction-sharing
chunk): this ledger was reconciled against `origin/main=07a6b501648120039b2f0ec8c83e549a64a84033`
before publication, and contains implementation commit
`5e3680117c26f5b7bb1b9280eec65a350b3dd2f7`. Commit `5e368011`
(`perf(w26): share fdb lease authority transaction`) adds the
source-compatible `LeaseOracle::now_ms_in_transaction` hook, makes the shared
FoundationDB oracle read its protected authority key inside the already-open
metadata transaction, and routes acquire/renew/release/publish through that
hook. Custom clocks and the persisted/development path retain the default
fallback; authority reads remain read-only and missing/malformed samples fail
closed. Feature-enabled FoundationDB check and strict Clippy, full locked
workspace tests, strict workspace Clippy, formatting and diff checks pass.
The feature-enabled FoundationDB test command reaches link but is blocked
locally by missing native `fdb_c`; this is an external native gate. Security
diff scan `2eb14a81-ce6f-4c30-bf98-a3e65479a8cd` has complete changed-file
coverage over one surface and zero reportable findings.

The latest terminal hosted packet remains run `35691451007` on earlier SHA
`dccd8351`: base Ozone passed, PGlite/R2 reached `2,065.669446` IOPS, while
SQLite/R2 reached `213.947686`, TiDB/R2 `333.356025` and FoundationDB/R2
`363.254472`; all rows completed 1,200/1,200 operations with zero timeouts and
cleanup failures, but aggregate `106631474430` correctly failed closed on
missing `OZONE_IOPS_PASS`. Targeted run `35693778762` selected
`b3fb7988bce1edaafbd44f2adf22bb217ca99671` and has now completed all four
producers: base `106636116163` passed, compositions `106636116105` failed,
TiDB `106636116197` failed and FoundationDB `106636116201` failed. Its
aggregate `106641631860` is queued at latest capture. The targeted rows
measured SQLite/R2 `580.6091135`, PGlite/R2 `963.6041014`, TiDB/R2
`385.2624172` and FoundationDB/R2 `337.4773006` IOPS; all completed
1,200/1,200 operations with zero timeouts and cleanup failures, but all missed
the hard target. W26 remains **NO-GO**. Customer Ozone deployment, secure
topology, 99.99% availability, five-minute RPO/RTO, backup/DR and the release
stream remain external ownership boundaries. Manual retained qualification
run `35695427227` selected exact SHA `e7850fb41775351503e5aa685484906b3a3cbbe4`
with base `106641134190`, compositions `106641134304`, TiDB `106641134221`
and FoundationDB `106641134132` still queued at latest capture; its aggregate
was not yet created. The earlier automatic current-code run `35694753908` was
canceled by ordinary-push concurrency. Treat the manual run as the hosted
qualification boundary and retain the detailed ledger in
`docs/w26-progress-ledger.md`.

Current W26 hosted dispatch boundary: poll retained manual run `35695427227`
for exact `e7850fb4` provider and aggregate results, and poll targeted aggregate
`106641631860` to close its diagnostic record; retain all artifacts/logs and do
not promote queued, pending, failed, skipped, canceled, incomplete or missing
marker results. The detailed per-item ledger, provisional estimates, external
gates and session log are in `docs/w26-progress-ledger.md`.

Current W26 authority override (2026-09-22, TiDB publication chunk): the
verified shared code tip is `origin/main=4098c7df04a03f65269ef932cb921b96d6368297`.
Commit `4098c7df` changes only the successful TiDB metadata publication path:
it uses one parameterized autocommit conditional UPDATE acknowledgement and
keeps the explicit pessimistic locked transaction for zero-row stale-versus-
revision classification. Retryable statement conflicts remain `EAGAIN`; lost
or otherwise ambiguous acknowledgements remain fail-closed `EIO`. Focused
TiDB tests (8 passed, 5 service-gated ignored), full locked workspace tests,
strict provider/workspace Clippy, formatting and diff checks all pass. The
sealed security diff scan `c36f104e-965b-4e85-bfde-2d3d222f0de2` reviewed two
surfaces with zero reportable findings.

The latest terminal hosted packet is still run `35691451007` on earlier SHA
`dccd8351`: base Ozone passed, PGlite/R2 reached `2,065.669446` IOPS, while
SQLite/R2 reached `213.947686`, TiDB/R2 `333.356025` and FoundationDB/R2
`363.254472`; every provider completed 1,200/1,200 operations with zero
timeouts and cleanup failures, but the aggregate `106631474430` correctly
failed closed on missing `OZONE_IOPS_PASS`. W26 remains **NO-GO**. Publish the
ledger/tracker chunk, dispatch a fresh exact-SHA CI matrix, and promote only a
terminal all-provider/end-to-end aggregate pass. Customer Ozone deployment,
secure topology, 99.99% availability, five-minute RPO/RTO, backup/DR and the
release stream remain external ownership boundaries.

Current W26 hosted dispatch override: targeted run `35693778762` selected the
exact ledger revision `b3fb7988bce1edaafbd44f2adf22bb217ca99671`, containing
TiDB code `4098c7df`. Its W26 producer jobs are base `106636116163`,
compositions `106636116105`, TiDB `106636116197`, and FoundationDB
`106636116201`; the aggregate was not created at capture. These jobs are
queued/in progress state only and are not acceptance evidence. Concurrent
mainline activity later advanced `origin/main` to `8e76aa44` and queued broad
run `35693835337`; that newer run is recorded as a separate pending boundary.
Poll the targeted run, retain all exact-SHA artifacts and logs, and keep W26
**NO-GO** until every configured provider and the one-revision aggregate pass.

Historical W26 TiDB session-setup chunk (published 2026-09-22): the TiDB provider now configures and
verifies `tidb_txn_mode='pessimistic'` once for each newly created private pool
session, disables redundant pool-reset round trips, and retains the
`RepeatableRead` transaction guard, fail-closed mode check, parameterized SQL,
rollback handling and ambiguous-commit semantics. Focused TiDB tests, strict
provider/workspace Clippy, full locked workspace tests, formatting and diff
checks pass locally. Security diff scan `c4fc0012-b0e2-421c-9db8-ca3edfce730c`
completed with full changed-file coverage and zero reportable findings. This
is included in the current published tree; the exact hosted qualification
recorded below selected an earlier revision and the 1,000-IOPS production gate
remains **NO-GO**.

Current W26 SQLite follow-up (2026-09-22): the SQLite metadata publisher now
uses a parameterized fenced conditional CAS update on the successful path and
performs the lease/revision read only when the update affects zero rows. It
retains missing-row errors, stale-lease classification, revision conflicts,
unexplained-zero-row fail-closed behavior and the existing transaction commit
boundary. SQLite provider tests, strict provider/workspace Clippy, full locked
workspace tests, formatting and diff checks pass locally. Security diff scan
`ea882470-0ef4-4f7a-8d91-03c8d85d7fb7` completed with full changed-file
coverage and zero reportable findings. The implementation is published as
`ba4e89d0` in the current shared tree, but the last hosted packet selected an
earlier revision; production remains **NO-GO**.

Current W26 published TiDB isolation follow-up (2026-09-22): the TiDB private pool now
sets and verifies both pessimistic transaction mode and `REPEATABLE-READ`
session isolation once for each newly created connection. Per-transaction
isolation negotiation is removed while the transaction guard, rollback on
cancellation/early return, fail-closed mode/isolation validation,
parameterized SQL and ambiguous-commit semantics remain intact. The focused
TiDB package has 7 passing unit tests; full locked workspace tests, strict
workspace Clippy, formatting and diff checks pass locally. Security diff scan
`5975446d-8bb7-457e-92d3-74c5c6ccf671` completed with full changed-file
coverage and zero reportable findings. Commit `0f95cb7d` is included in
`origin/main` `0ca59c85`; the last hosted packet selected an earlier revision
and did not qualify this change. Production remains **NO-GO**.

Current W26 authority override (2026-09-22): `origin/main` is
`0ca59c85a36227a07730f0282194ef7af8650fbf`. Manual hosted run
`35686340751` selected `1e64bc250b5e863620f069aad173945dc474c5b4`: base Ozone
job `106613849779` passed; compositions `106613849813` failed with
SQLite/R2 `997.06` and PGlite/R2 `592.43` IOPS; TiDB `106613849794` failed
with `277.59` IOPS; FoundationDB `106613849672` failed before benchmarking
because `tests/foundationdb/Cargo.lock` rejected `--locked`; aggregate
`106616062969` failed closed on missing `OZONE_IOPS_PASS`. SQLite/PGlite/TiDB
completed 1,200/1,200 lifecycle operations with zero timeout/cleanup failures;
FoundationDB produced no IOPS artifact. This run predates `ba4e89d0` and
`8428a5ef`, so it is diagnostic only. The next matrix must run on the exact
current published SHA, preserve the 1,000-IOPS hard target and keep production
**NO-GO** until every configured provider and the complete end-to-end aggregate
pass.

Current W26 hosted dispatch override (2026-09-22): concurrent mainline work
advanced the shared tip to `06fc70612b9387a281ab050f711fc878713177ea` after
the ledger publication. Manual run `35688061634` selected that exact SHA.
Producer jobs are queued/in progress: base Ozone `106619031921`, compositions
`106619031684`, TiDB `106619031746` and FoundationDB `106619031804`; the
aggregate job has not yet been created at capture. This run is not evidence
until every producer and the aggregate are terminal. It includes the SQLite
CAS `ba4e89d0`, TiDB isolation `0f95cb7d` and FoundationDB lockfile correction
`8428a5ef`; production remains **NO-GO** pending all hard markers and the
1,000-IOPS target.

## Decisions and external prerequisites

- After the app restart, the nine prior worker handles were missing. Their
  checkout edits were preserved. Seven replacement Luna Max workers were
  started and verified running; current ownership is in the dashboard above.
  Historical worker names in individual evidence entries identify earlier
  work, not live sessions. Main owns integration, root manifests, and commits.

- [x] **D01 — Live R2 credentials:** security-provisioned bucket-scoped Object
  Read & Write credentials for `mount-rs-integration-tests` are stored as
  encrypted GitHub `r2-ci` environment secrets, using a one-week Cloudflare
  token TTL through 2026-09-28. The final hosted run `35579757447` passed the
  budget guard, Rust/Node SDK and CLI matrices, live R2 trace/CLI/N-API,
  service evidence and benchmark artifact. Credential values are not committed
  or recorded; rotation remains scheduled maintenance.
- [x] **D02 — License:** user selected Apache-2.0 for mount-rs on 2026-09-20.
  First-party package declarations use Apache-2.0; preserve third-party notices,
  including MIT attribution for upstream-derived portions.
- [ ] **D03 — FSKit:** obtain signing/install/activation authorization and host
  prerequisites when executable extension testing is ready.
- [ ] **D04 — Distributed cache:** discuss topology, consistency and service
  choice with the user after the primary implementation is accepted.
- [x] **D05 — Launch:** user authorized the existing Vercel Hobby plan and the
  `mount-rs.com` launch. Route 53 and Vercel access are verified; no duplicate
  purchase, paid upgrade or additional paid resources were used.
- [x] **D06 — Rust crate publication authorization:** user authorized publication
  from CI on `main` when ready. npm publication must use `@mount-rs`, with secure
  release controls for both registries. Verify namespace ownership, OIDC trust,
  protected main-only release jobs, provenance/package integrity and readiness
  before publishing. This authorization is not release-readiness evidence.

## W01 — Core and mountx behavioral parity

Current priority is simple-to-complex execution. Close the mount-free
in-memory Rust/Node/oracle contract first, then add persistence, remote
providers, transports, and native mounts only as separate later gates. A
passing remote/provider or native test cannot close a simpler parity gap, and
the W01 stream must remain open until its skipped behavior is classified and
the applicable oracle-backed cases are covered.

For this execution turn, W04, W05, W07, W08, W25, and W26 are allocated to
other threads. Main is therefore keeping this checkout scoped to W01
implementation and acceptance until the W01 ledger is genuinely closed.

W01 is now split into transport-owned coordination tracks. Each owner has an
independent tracker and worktree; the parent W01 ledger remains the production
roll-up, and every finished chunk must commit its implementation, tests, and
transport tracker together.

| Track | Independent tracker | Owner task |
| --- | --- | --- |
| W01-FUSE | [`docs/W01_FUSE_PROGRESS.md`](docs/W01_FUSE_PROGRESS.md) | Delegated task; thread id to be recorded after dispatch |
| W01-9P | [`docs/W01_9P_PROGRESS.md`](docs/W01_9P_PROGRESS.md) | Delegated task; thread id to be recorded after dispatch |
| W01-NFS | [`docs/W01_NFS_PROGRESS.md`](docs/W01_NFS_PROGRESS.md) | Delegated task; thread `01a0c456-a28e-7cb3-9b48-a3d23e7ec8c0` |
| W01-S3 | [`docs/W01_S3_PROGRESS.md`](docs/W01_S3_PROGRESS.md) | Delegated task; thread id to be recorded after dispatch |
| W01-WebDAV | [`docs/W01_WEBDAV_PROGRESS.md`](docs/W01_WEBDAV_PROGRESS.md) | Delegated task; thread id to be recorded after dispatch |

Current W01-WebDAV packet (2026-09-22): the Rust HTTP server now serializes
`listen()`/`close()` lifecycle transitions, guards the accept loop against an
immediate-close shutdown lost wakeup, and retains a timed-out drain state so a
second `close()` cannot report false success or rebind while a stalled
connection remains active, and aborts tracked connection tasks when the
drain deadline expires. The preceding implementation packet `4bc10ad1`
passed the focused Rust target 18/18 with strict
Clippy and formatting. The later exact packet `22f9169bbc90c6887bb1bddafcde5795cd7098d1`
also passed 18/18, strict warning-denied Clippy, and formatting using the shared
Cargo target; the native mount probe remains ignored. The N-API WebDAV wrapper serializes its closed-state
check with the transport lifecycle; the shared postbuild server facade now
clears a failed close promise so a timed-out WebDAV close can be retried after
the peer exits. The focused wrapper race test passes 40 alternating
real-loopback iterations, its stalled-request timeout/retry regression passes,
and the focused direct-session concurrency probe passes 256 concurrent PUT/GET
requests, while the network-concurrency/auth test passes 256 concurrent HTTP
PUT/GET pairs, a chunked streamed PUT/GET, live Basic-auth
challenge/acceptance, and one exact-once live request-error callback. The
provider-backed direct-session matrix also passes 128 concurrent NodeFs and
SQLite PUT/GET pairs in three repetitions with exact byte readback. The
host-enabled provider-backed network matrix also passes 64 concurrent NodeFs
and SQLite HTTP PUT/GET pairs in three repetitions, including streamed bodies.
The focused Rust concurrent lock regression also passes: two simultaneous
writes without the submitted token both return `423` against one exclusive
lock.
The current-tip Rust WebDAV target passes 28/28 with the shared Cargo wrapper,
and warning-denied WebDAV Clippy passes; the target now includes a durable
driver barrier regression covering successful PUT, MKCOL, PROPPATCH, COPY,
MOVE, DELETE, resource creation by LOCK, and injected barrier failure/retry.
This is transport-level barrier evidence and does not close external hosted,
provider-lifecycle or power-loss gates. Durable lock persistence is explicitly
outside the supported WebDAV scope: the NodeFs and SQLite forced-process-loss
probes recreate the provider in a replacement session and observe
`lockCount === 0` after the killed child held an exclusive lock.
The current shell has no AWS/R2/Cloudflare credential names available; live
provider acceptance remains externally gated and no credential values were
read or persisted.
The latest protected audit remains non-qualifying: on current `origin/main`
`a7ff32f00e597dbdc37e0baefe3f37321354bb58`, CI run `35690677984` was cancelled
after Ubuntu native WebDAV job `106626916000` passed and macOS native WebDAV
job `106626916081` was cancelled; protected AWS run `35690334807` failed at
`AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, and protected R2 run `35690334795`
failed at `count=317 limit=20` before its live integration job. No hosted
WebDAV aggregate PASS or live-provider acceptance is promoted from those
results.
The structural N-API `FsDriver` now also has an optional
`readdirBounded(path, maxEntries)` callback. The rebuilt addon and focused
structural WebDAV regression pass bounded `Depth: 1` PROPFIND, recursive
collection COPY/DELETE, provider-reported overflow, adapter rejection of an
over-large callback result, and the absent-callback `ENOTSUP`/`501` boundary;
the provider callback remains responsible for enforcing the ceiling before it
materializes its listing.
The Rust WebDAV session now treats unread request-body faults as a framing
boundary: known 413 limit faults are drained for keep-alive reuse, while any
other drain fault is reported once and adds `Connection: close`, even when the
request dispatch itself had already produced a response. The focused WebDAV
target passes 31/31, and the rebuilt N-API/WebDAV-only host phase remains green.
The pinned WebDAV authority audit confirms Rust `Url::host_str()` retains
bracketed IPv6, so the existing literal `Destination`/tagged-`If` comparison
already matches the oracle; focused fixtures cover `[::1]` and `[::1]:8080`,
and the full WebDAV target remains 28/28 with strict Clippy and formatting
green.
The pinned `CARGO=./scripts/cargo-shared MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node scripts/check-http-parity.mjs`
HTTP differential also passes all 40 paired S3+WebDAV cases, including its 16
WebDAV cases, after the authority audit.
The current conditional-header packet keeps entity-tag parsing aligned with the
pinned oracle: optional whitespace around `If-Match`/`If-None-Match` list
members is accepted, while whitespace between `W/` and the quoted value is
preserved so malformed input cannot become a false weak match. The focused
regression and full WebDAV target pass 41/41; workspace warning-denied Clippy,
formatting, diff checks, and the pinned 40-case differential also pass.
Because the shared Cargo target lost artifacts during a concurrent rebuild, the
final exact-head focused/full/Clippy proof used the fresh explicit
`CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-final-40c79a5b` through
`./scripts/cargo-shared`; the shared cache was not cleaned or deleted.
The public `DavLockTableOptions` finite-timeout path now caps before applying
the one-second minimum, so even `max_timeout_seconds: 0` cannot reach Rust's
invalid `clamp(1, 0)` panic; the deterministic regression and the full 29/29
WebDAV target pass with strict Clippy, formatting, and diff checks green.
Public WebDAV lock snapshots now preserve grant order like the pinned oracle's
insertion-ordered lock map, while expiry/removal cleanup and duplicate-token
replacement retain that order; the focused ordering regression and the full
30/30 WebDAV target pass with strict Clippy, formatting, and diff checks green.
Lock-root cleanup now removes a lock only after `stat` confirms `ENOENT`; a
provider I/O error during DELETE/MOVE cleanup retains the lock rather than
collapsing unknown namespace state into absence. The fault-injected regression
and full 31/31 WebDAV target pass with strict Clippy, formatting, and diff
checks green.
Native streamed file responses now observe the shared shutdown signal during
provider reads and channel sends, count as background work during server drain,
and use a cancellation-safe close guard with a tracked fallback close. The
stalled provider-read loopback regression passed, the full WebDAV target passed
32/32, warning-denied Clippy passed, and formatting/diff checks passed. This is
local response-task lifecycle evidence; hosted/provider, power-loss,
durable-lock, crash/restart, and stronger same-resource ordering remain open.
The transport-neutral `WebdavBody::into_bytes()` path now also retains a
cancellation-safe close guard and schedules provider-handle cleanup when a
direct session consumer aborts during a pending read. Its stalled-read
regression passed, the full WebDAV target passed 33/33, warning-denied Clippy
passed, and formatting/diff checks passed. This is separate from native server
shutdown and the N-API body's explicit close seam; hosted/provider,
power-loss, durable-lock, crash/restart, and stronger same-resource ordering
remain open.
Mutation-side provider handles now use the same cancellation-safe close
boundary: streamed `PUT` and shared file-transfer helpers schedule `close` if
their request future is abandoned during body polling or provider I/O. The
stalled-write regression passed, the full WebDAV target passed 35/35, and
warning-denied Clippy passed. This is separate from response-body cancellation
and native server shutdown; hosted/provider, power-loss, durable-lock,
crash/restart, and stronger same-resource ordering remain open.
The shared WebDAV lock table now fails closed on mutex poisoning across request
paths that inspect, create, refresh, unlock, or enforce locks; a poisoned table
returns a `500` server error instead of appearing empty. Public lock snapshots
recover the poisoned guard for observability. The deliberate-poison regression
confirmed that a locked `PUT` does not mutate the existing resource; the full
WebDAV target passed 34/34, warning-denied Clippy passed, and formatting/diff
checks passed. Hosted/provider, power-loss, durable-lock, crash/restart, and
stronger same-resource ordering remain open.
Declared Content-Length overflows now route through session rejection
bookkeeping before the HTTP adapter closes the connection, so request/reply/error
stats and the request-level on_error hook remain exact-once; a real-loopback
regression verifies the 413 response, original PUT request head, counters,
and bodyless HEAD handling in the shared helper. The full WebDAV target passes
36/36, warning-denied workspace Clippy passes, formatting and diff checks pass,
and the pinned TypeScript/Rust differential passes all 40 paired cases.
Hosted/provider, authentication-ordering, power-loss, durable-lock,
crash/restart, and stronger same-resource ordering remain open.
Public lock coverage now iterates the grant-order index for covering and within
lookups instead of HashMap values, so lock-discovery lists, locked-member
multistatus results, and first-conflict selection remain deterministic like the
pinned insertion-ordered Map. The focused regression covers ancestor/direct
coverage, subtree roots, and the selected 423 conflict; the full WebDAV target
passes 37/37, warning-denied workspace Clippy passes, formatting and diff checks
pass, and the pinned TypeScript/Rust differential passes all 40 paired cases.
Hosted/provider, power-loss, durable-lock, crash/restart, and stronger
same-resource ordering remain open.
The public `parse_lock_token` boundary now uses delimiter-safe prefix/suffix
handling rather than byte-offset slicing, so a valid Unicode token such as
`<urn:uuid:é>` is accepted without a UTF-8 panic while nested angle brackets
remain rejected. The focused protocol regression and full 37/37 WebDAV target
pass, warning-denied workspace Clippy, formatting, diff checks, and the pinned
40-case TypeScript/Rust differential all pass. Hosted/provider, power-loss,
durable-lock, crash/restart, and stronger same-resource ordering remain open.
The public XML serializer now follows the pinned codec for hostile text and
namespace values: carriage returns become `&#13;`, XML-invalid controls become
U+FFFD, and ordinary markup escaping remains intact. The focused serializer
regression matches the pinned bytes for both text and `xmlns`; the full 37/37
WebDAV target, warning-denied workspace Clippy, formatting, diff checks, and
40-case TypeScript/Rust differential pass. Hosted/provider, power-loss,
durable-lock, crash/restart, and stronger same-resource ordering remain open.
The public XML parser now validates UTF-8 and raw XML characters before
`quick-xml` builds a tree, rejecting controls such as NUL instead of exposing
them in `XmlNode` text. The regression reproduced the prior acceptance and now
matches the pinned parser's `invalid-character` refusal; the full 38/38 WebDAV
target, warning-denied workspace Clippy, formatting, diff checks, and 40-case
TypeScript/Rust differential pass. Hosted/provider, power-loss, durable-lock,
crash/restart, and stronger same-resource ordering remain open.
The public `If` parser now probes the `Not` keyword with UTF-8-safe string
access rather than byte-offset slicing. Malformed non-ASCII grammar such as
`(éé)` now returns the ordinary invalid-header result instead of panicking at a
code-point boundary; the regression reproduced the prior panic before the fix.
The full 39/39 WebDAV target, warning-denied workspace Clippy, formatting, diff
checks, and 40-case TypeScript/Rust differential pass. Hosted/provider,
power-loss, durable-lock, crash/restart, and stronger same-resource ordering
remain open.
The public HTTP-date parser now accepts RFC 9110 leap seconds in the same way
as the pinned oracle: a valid seconds field of `:60` is read as `:59`, while
invalid minutes and other malformed dates remain rejected. The focused
regression covers IMF-fixdate, RFC 850, asctime, leap-second, and invalid-minute
forms; the full 40/40 WebDAV target, warning-denied workspace Clippy,
formatting, diff checks, and the 40-case TypeScript/Rust differential pass.
Hosted/provider, power-loss, durable-lock, crash/restart, and stronger
same-resource ordering remain open.
The response stream has a native loopback fault regression as well: a short
driver read fails the client body after `200` headers and produces one
peer-qualified `Connection` transport report.
The public `WebdavRequestBody` documentation now states the corresponding
contract: only known 413 limit faults are drainable for keep-alive reuse;
non-recoverable body faults close the connection boundary.
The exact-tip audit for `c42030c1807f6504660892bf829137897e910c5e` then found
CI run `35687955166` cancelled with no jobs. Live Cloudflare R2 run
`35687955189` failed the usage-admission job at `count=309 limit=20`, so its
live integration job was skipped; no hosted WebDAV or live-provider PASS is
claimable from that tip, and mainline subsequently advanced to
`594ad797a67c77daa5d504042d15353da925688a`.
The current hosted provider audit confirms the external boundary: Live AWS S3
run `35679010203` failed its protected preflight with
`AWS_S3_CI_CONFIG_BLOCKED missing_bucket` and empty bucket/region/account/role
inputs, while Live Cloudflare R2 run `35680542993` failed its bounded-usage
admission with `count=285 limit=20`. The exact-tip CI for this WebDAV scope
chunk was cancelled by successor mainline publication; replacement CI
`35680709436` at origin tip `1bdf8846` had no jobs at the audit snapshot.
The following exact-tip CI run `35681063238` at published WebDAV tip
`7282bce8` began, but native-WebDAV jobs `106597988170` (macOS) and
`106597988268` (Ubuntu) were cancelled by successor tip `2bcd9aa4`; replacement
CI `35681127696` was pending at the audit snapshot. The earlier terminal
hosted native-WebDAV run remains the latest claimable hosted WebDAV result.
The published provider packet `fb9caec81a7e3fa183f5fa51871117e62fa35036` had
exact-SHA CI/Fault injection/W08 workflows queued or pending, W04 policy
succeeded, Live Cloudflare R2 failed, and unrelated Native 9P in progress; no
hosted WebDAV PASS is claimable from that packet. The
published provider-network packet `41bd16f08a4ba065046f5da4bd54d90d2a16f028`
had no associated GitHub Actions workflow runs at the exact-SHA snapshot, so
no hosted WebDAV PASS is claimable from that packet. The
opt-in
`MOUNT_RS_SERVER_PHASE=webdav node test/servers.mjs` phase also passes the
host-enabled WebDAV network/fault/restart matrix, while the package-wide
server harness remains blocked in its unrelated NFS phase before WebDAV.
Hosted network concurrency, power-loss/live-provider durability, and broader
hosted session/member lifecycle remain open; local NodeFs/SQLite process-crash
recovery, including in-flight streamed-PUT prefix recovery after independent
readback, is covered by the dedicated N-API probes. The callable/member packet
`8f0e74138286a678cbc5868d3cc4a528fb1b9fe9` has exact-SHA CI and release lanes
pending or queued, so no hosted WebDAV acceptance is claimable from that packet.
The subsequent 64-pair packet `d391f9b798df455311177f462ab160736ed3ba4c`
had its exact-SHA CI, W08, Fault injection, and W04 runs cancelled by later
mainline publication while its Live R2 run remained queued; its local
concurrency evidence is not promoted to hosted acceptance.
The pinned WebDAV oracle deliberately has no `PathLock` for this HTTP session;
the transport therefore supports concurrent independent resources and
WebDAV lock/`If` coordination, but does not claim linearizable same-resource
ordering or atomic same-target `PUT` publication. Power-loss durability and
live-provider behavior remain separate gates; durable lock persistence is
outside the supported WebDAV scope.

- [x] Land Rust filesystem contract and implementations, with separate crates.
- [x] Pin mountx oracle to `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.
- [x] Latest local upstream-suite stage: 1,194 passed, 88 skipped; not full parity
  acceptance and not evidence that skipped behavior works.
- [ ] W01.1 Complete every applicable export/behavior in the API parity ledger.
- [ ] W01.2 Classify all upstream skips and add coverage for required behavior.
- [ ] W01.3 Finish seeded cross-engine traces and retain reproducible failures,
  seeds and revision-matched results for every required backend.
- [ ] W01.4 Verify errors, paths, binary data, links, timestamps, handles,
  concurrency and lifecycle on supported macOS and Linux configurations.
- [x] W01.5 Add a bounded deterministic concurrency packet: six scenarios,
  five explicit unsupported classifications, zero oracle mismatches, and five
  consecutive pinned-oracle runs. Cross-process/crash, exact append ordering,
  cancellation/close races, durability/restart and transport/native concurrency
  remain open by classification.

WebDAV-owned W01 tracking is maintained in
[`docs/W01_WEBDAV_PROGRESS.md`](docs/W01_WEBDAV_PROGRESS.md). The current
bounded slices add the low-level `./webdav` barrel and generated declarations,
the N-API `WebdavSession`/server session view, buffered and pull-based streamed
request handling, positional response chunks, serializable session and lock
policy, driver access, Basic-auth challenge and acceptance, Rust transport
hooks, and request-level WebDAV error callbacks. The Rust WebDAV target passed
14/14 tests; the
isolated locked N-API check, release addon, generated declarations, and direct
N-API stream probe passed three-chunk PUT, multi-chunk GET, early iterator
return, and deliberate body failure. The oracle differential is explicitly
skipped without `MOUNTX_SOURCE`; with the pinned source at
`/private/tmp/mountx-source-w01-20260921` (oracle
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`), the pure WebDAV barrel/protocol
differential and source-backed host-enabled server phase pass. The full supported
N-API server/session prototype-member differential also passes with explicit
string/public-symbol prototype audits plus buffered/streamed session and server
lifecycle member assertions, using
`MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node
test/webdav-session-parity.mjs`, covering effective credentials/lock/session
options, driver and lifecycle members, Map-shaped method counters, the full
supported WebDAV class 1/2/3 direct-method set, recursive owner lock snapshots,
and request/reply/error/assertion counters. Native-only `handleRequestStream` and
`lockCount` are explicit supported additions, while internal wrapper symbols are
filtered from the oracle comparison; three repeated parity runs pass. Its XML
comparisons normalize dynamic HTTP/ISO timestamps and opaque ETags, and COPY/MOVE
destination reads plus DELETE-missing readback verify side effects. The sandbox
blocks the live N-API loopback bind with
`Operation not permitted`, while provider, hosted, network-client concurrency,
and restart/durability gates remain open. The explicit ignored native WebDAV
round-trip now passes locally on this macOS arm64 host using
`/sbin/mount_webdav`; this does not substitute for hosted macOS/Linux
acceptance. The host-enabled WebDAV session
packet also completes 256 parallel unique-file PUTs and GETs through one
direct session with exact byte-for-byte readback; that is same-process
same-driver evidence only. The configured 256-request direct-session and
256-pair network probes each passed three repeated times with exact byte
readback; this remains same-driver/loopback evidence only. The active lock view now preserves a recursive
namespaced owner XML tree, and bounded predefined/numeric XML references are
accepted while DTD/custom entities remain refused. W01 and production status
remain **NO-GO**. The pinned
`CARGO=./scripts/cargo-shared MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node --experimental-strip-types scripts/check-http-parity.mjs`
also passes all 40 paired TypeScript/Rust S3+WebDAV loopback cases, including
16 authenticated WebDAV cases for streaming PUT, XML property updates,
GET/HEAD/range/conditional behavior, PROPFIND, COPY/MOVE, refusal,
missing-resource, and DELETE. This is local pinned-oracle HTTP evidence, not
hosted/native acceptance. At exact scope-packet SHA
`e13c52bea3485fada8be03ed62fba9a107255507`, CI run `35640746296` also reported
successful `native-webdav (macos-latest)` job `106469172312` and
`native-webdav (ubuntu-latest)` job `106469172419`; this qualifies hosted
native WebDAV I/O for that packet only, not the overall CI run or production
acceptance. The focused N-API NodeFs and SQLite provider/reopen probes now also
preserve exact file bytes across orderly provider/server recreation and observe
zero replacement-session locks, classifying bytes as durable and locks as
process-local for those local providers; the NodeFs and SQLite process-crash
probes add abrupt-death recovery, but none of these local results qualify
power-loss or live remote-provider durability. A read-only status check for the published tip
`9e8e4592cd8d4fe5b42c2734621ac1cd1bce02b5` found [CI run
35631845088](https://github.com/andymac4182/mount-rs/actions/runs/35631845088)
and [fault-injection run
35631845044](https://github.com/andymac4182/mount-rs/actions/runs/35631845044)
cancelled, while [Live Cloudflare R2 run
35631845090](https://github.com/andymac4182/mount-rs/actions/runs/35631845090)
failed; no hosted WebDAV acceptance is claimable from that tip.
The remaining N-API session member boundary is also explicit: scalar options,
snapshot lock records, assertion readback, Map-shaped method counters, and
request-level `onError(error, head)` are implemented. The oracle's injectable
`now`, `onAssertion`, and live `DavLockTable` methods are explicitly outside
the supported N-API scope: the Rust transport retains deterministic clock
injection, current Rust request paths have no externally triggerable assertion
site, and the N-API lock view is intentionally expiry-aware but read-only so
request token/ownership checks remain authoritative. The injectable `now`,
`onAssertion`, and live `DavLockTable` remain explicit outside-scope controls.
Hosted/native lifecycle, provider, restart/durability, and broader concurrency
remain open rather than being silently accepted.
At the exact published WebDAV implementation packet
`5c6716a201cdcbc1feeac9d2c245d113250f3bc3`, read-only status showed [CI run
35667576642](https://github.com/andymac4182/mount-rs/actions/runs/35667576642),
[W08 release targets run 35667576687](https://github.com/andymac4182/mount-rs/actions/runs/35667576687),
[Fault injection run 35667576587](https://github.com/andymac4182/mount-rs/actions/runs/35667576587),
and [W08 release policy run 35667576738](https://github.com/andymac4182/mount-rs/actions/runs/35667576738)
pending, while [Live Cloudflare R2 run 35667576677](https://github.com/andymac4182/mount-rs/actions/runs/35667576677)
and [W04 production policy run 35667576848](https://github.com/andymac4182/mount-rs/actions/runs/35667576848)
were queued; no hosted WebDAV PASS is claimable from this implementation packet.
The current docs-only tip `f76a637fdc6d62f400b75505579628facb3cc871` also has
[CI run 35633305914](https://github.com/andymac4182/mount-rs/actions/runs/35633305914)
and [fault-injection run
35633305962](https://github.com/andymac4182/mount-rs/actions/runs/35633305962)
cancelled; [W04 production policy
35633305932](https://github.com/andymac4182/mount-rs/actions/runs/35633305932)
succeeded, and no fresh Live Cloudflare R2 run was listed. No hosted WebDAV
PASS is claimable from the current tip.
The immediately preceding tracker tip `4b103b1ab9be142b15638a9679999bfd43d3bd80`
had [CI run 35633547228](https://github.com/andymac4182/mount-rs/actions/runs/35633547228)
pending and [fault-injection run
35633547086](https://github.com/andymac4182/mount-rs/actions/runs/35633547086)
in progress at the read-only check; W04 policy succeeded, but no hosted
WebDAV PASS was claimable.

The WebDAV streamed `PUT` atomicity boundary is now explicit and tested:
matching the pinned oracle, the destination opens at the first non-empty body
chunk, so a body failure returns `500` while preserving the exact written
prefix. The Rust focused regression and host-enabled N-API WebDAV phase both
assert `partial` remains after the deliberate failure. This is an accepted
oracle-compatible protocol scope decision, not atomic publication or
power-loss/live-provider durability evidence; those gates remain open.

WebDAV now awaits `FsDriver::syncfs()` before acknowledging successful
filesystem mutations whenever `Capabilities::durable_writes` is advertised;
barrier errors return request failures, while volatile drivers remain no-op.
Lock-table state remains process-local and is not presented as durable lock
state. The focused 20-test Rust target exercises this contract across the
mutation methods and an injected failure/retry path; provider-specific
power-loss ordering, live remote-provider behavior, hosted lifecycle, and
durable-lock acceptance remain open.
The narrow structural N-API `FsDriver` seam now forwards an optional
`syncfs()` callback as well; the WebDAV regression reaches it through
`createWebdavServer` and confirms acknowledgement after success, a 500
response after callback failure, and a 501 Not Implemented response when the
callback is absent. Durable structural drivers without the callback continue
to fail closed with `ENOSYS`.
At exact published packet `4e19f22648dc8f6fa622b70e76f70ca4540a3483`, the
read-only hosted snapshot found CI `35672738319` and W08 release targets/policy
`35672738309`/`35672738333` pending, Fault injection `35672738370` and Live AWS
S3 `35672738306` in progress, and Live Cloudflare R2 `35672738332` failed; W04
policy `35672738331` succeeded and unrelated Native 9P `35672738401` was in
progress. No hosted WebDAV PASS is claimable from that snapshot.
The barrier-enabled tree was then requalified through the rebuilt release
N-API addon: generated typecheck, the isolated host-enabled WebDAV server
phase, lifecycle, 64-pair direct-session and network/auth/streaming probes,
NodeFs/SQLite orderly reopen, provider direct/network concurrency, and
NodeFs/SQLite crash plus in-flight streamed-PUT recovery all passed. This is
local rebuilt N-API/provider evidence only and does not close hosted
session/lifecycle/concurrency, live remote-provider, power-loss, or durable-lock
gates.
At exact current tip `e5ae05d07bbc73184952def0437e58be9efef790`, the locked
Rust WebDAV target passed 20 tests with the privileged native mount probe
explicitly ignored; warning-denied `mount-rs-napi` Clippy, the host-enabled
release N-API build, generated typecheck, the structural durable-driver
success/500-error/501-missing-callback regression, lifecycle, 64-pair direct-
session concurrency, 64-pair live HTTP network/auth/streaming concurrency,
formatting, and diff checks all passed. Manual exact-tip CI run
`35674823787` subsequently completed its macOS native-WebDAV job
`106579011909` and Ubuntu native-WebDAV job `106579012075` successfully; the
overall run remains in progress, so no full hosted PASS is claimable from this
requalification. Hosted session/lifecycle/concurrency, live-provider behavior,
power-loss ordering, durable locks, and wider ordering remain open.
At current checkout `d761deb23513ec78b61d7b627f67b46606ee4956`, the refreshed
NodeFs/SQLite orderly-reopen and crash probes, 128-pair provider direct-session
matrix, 64-pair provider loopback network matrix, in-flight streamed-`PUT`
prefix recovery, pinned WebDAV barrel/session-member differentials, and the
40-case TypeScript/Rust S3+WebDAV HTTP differential all passed. This remains
local provider/crash, pinned-oracle, and loopback evidence only; hosted
session/lifecycle/concurrency, live remote-provider behavior, power-loss
ordering, durable locks, and wider ordering remain open.
The exact-tip hosted CI run `35674823787` also passed all four N-API `node`
jobs (Ubuntu x64/arm64 and macOS arm64/Intel), all three Rust jobs, and both
native WebDAV jobs at `e5ae05d07bbc73184952def0437e58be9efef790`. Its `node`
workflow builds the locked addon and runs the full pinned-oracle N-API package
script, providing hosted cross-platform WebDAV session, streaming, lifecycle,
concurrency, provider, crash, and parity evidence. The workflow remains
nonterminal on an unrelated native-FUSE job, so mounted-host concurrency,
live-provider, power-loss, and durable-lock acceptance remained open at that
snapshot.
The updated exact-tip CI run `35676711711` at
`87b2e2f0f95ff58830f04c1bd80e5f49e14b7fea` passed both revised native WebDAV
jobs, `106584655866` on macOS and `106584656036` on Ubuntu, including the
eight concurrent native-client write/read pairs. Mounted-I/O concurrency is
now hosted evidence; adverse mounted-host teardown/restart, live-provider,
power-loss, durable-lock, and wider ordering acceptance remain open.
The current ignored native WebDAV harness now performs eight concurrent
native-client write/read pairs after the basic round trip, then closes the
server while mounted, unmounts, relistens, remounts the same driver, and
verifies a post-restart round trip. Its host-enabled macOS run passed 1/1 with
the shared Cargo wrapper, and corrected hosted run `35678488755` passed both
native-WebDAV jobs for the same flow; the aggregate workflow remained
nonterminal on unrelated jobs.
The first exact-tip hosted rerun, CI `35677973511` at
`6ccea74f0df6001e2627bf33e3c8b4ffb2d9d58b`, reached macOS native WebDAV but
the harness treated the server-induced `not currently mounted` cleanup result
as failure; the corrected cleanup now accepts an absent mount and still fails
if the mount remains active. A fresh hosted rerun is required.
At the pre-completion snapshot, corrected manual run `35678488755` at
`ed29016e82c46e23e372f0615916ed3c1608370b` was aggregate-queued with neither
native-WebDAV job started; its subsequent macOS and Ubuntu jobs both passed.
At final ledger tip `3148aa5a95e5cdcf328014fac7e007db0e16adfe`, exact-SHA CI
`35673381803` and W08 policy/targets `35673381898`/`35673381797` were pending,
Fault injection `35673381853` and W04 policy `35673381814` were queued, and no
Live AWS S3 or Live Cloudflare R2 run was listed. No hosted WebDAV PASS is
claimable from the final published tip.

For the published 256-request packet `efd6ed33cf33e65fd1c86cd6fe3cec6783d610e6`,
the exact-SHA CI run `35669390058`, W08 release targets `35669390013`, and W08
release policy `35669389968` were pending; Fault injection `35669390039` and
W04 production policy `35669390028` were queued, and no Live Cloudflare R2
run was listed. No hosted WebDAV PASS is claimable from this tip.

For the published in-flight-crash packet
`4719eb50a6e2d48e4539c10f0638fa2f493301c1`, exact-SHA CI `35670254510`, Fault
injection `35670254549`, W08 release targets `35670254488`, and W08 release
policy `35670254485` were cancelled; W04 production policy `35670254554`
succeeded, while Live Cloudflare R2 `35670254473` and unrelated Native 9P
`35670254618` were in progress. No hosted WebDAV PASS is claimable from that
packet.

Evidence landed without closing the remaining W01 acceptance gates:

- [x] `0de1832` plus `6ba3d62` now provide a mount-free core parity harness:
  101 operations, 79 successful results, 22 stable expected errors and zero
  mismatches/skips against the pinned oracle. The added `lchown` symlink
  lifecycle row closes a concrete core-ledger gap. This still does not claim
  concurrency, persistence, providers, transports or native mounts.
- [x] `ae2f12d` adds the complete 88-row upstream skip inventory and a
  revision-checked trace-evidence runner. Five pinned seeds passed on the
  memory/SQLite/object-store/chunked six-backend lane, and the dedicated
  PGlite lifecycle extended the same five-seed, 621-operation trace to
  `pglite` and `chunked-pglite`; live R2 remains credential-gated.
- [x] `dd65770` adds the Rust-backed N-API FUSE codec subpath and declarations;
  it is mount-free protocol coverage, not native FUSE session acceptance.
- [x] `0d7f1f4` proves the Rust CLI's real macOS NFS mount path with independent
  Rust and Node filesystem clients; `0dca1d1` adds a separate Node SDK CLI and
  its opt-in real macOS NFS self-test. Linux and provider-backed SDK matrices
  remain unverified here.
- [x] The isolated provider/consumer matrix exercises Rust SDK, Node SDK and
  both real CLI consumers with machine-readable PASS/SKIP/FAIL output. The
  current offline Rust matrix is 4/4, the process-level CLI matrix is 8/8
  with two explicit provider-gated skips, and the Node CLI includes a durable
  SQLite shutdown/reopen row. The local PGlite lifecycle remains the next
  rerun target for the new CLI rows; R2 rows remain explicit skips without
  credentials and do not count as live-provider acceptance.
- [x] `29337f7` makes the Rust CLI construct all local/provider drivers through
  the public `mount-rs-sdk` facade and adds a public Node CLI SDK self-test. The
  Rust SDK example, Rust CLI, Node CLI and provider matrix now exercise the same
  SDK contract; native mount and live remote-provider acceptance remain separate.
- [x] `71e826f` pins the parity checker to the oracle revision, requires locked
  Cargo execution, and records the next simple-first W01 closure queue. The
  pinned 101-step core trace and six-scenario concurrency trace still pass with
  zero mismatches; the listed lifecycle, capability and native gaps remain open.
- [x] `7236e0a`, `4422cd0`, `57632c6`, `e213856`, `1af1986`, `8ac77f3`,
  `9215ba3` and `a1ec0c5` integrate parallel W01 capability, structural native,
  Windows SQLite and NFS held-handle sidecars. Their focused evidence is listed
  above; these commits do not close the remaining end-to-end parity gates.
- [x] `21803fd` adds the SDK-backed Node CLI provider-config/reopen path, extends
  the Rust and Node provider matrices to public-SDK PGlite/R2 compositions, adds
  the memory durability guard, and wires structural-driver coverage into the
  Windows/macOS Node CI jobs. Focused local gates passed; live R2 and hosted CI
  remain open.
- [x] The current SDK CLI packet adds the actual Rust binary `sdk-self-test`
  command and process-level memory/SQLite reopen integration checks, extends
  the Node CLI with the matching SQLite reopen example, and adds public
  `READDIRPLUS` codec differential coverage. The focused CLI/N-API/provider
  gates pass; PGlite, R2, native and hosted platform gates remain open.
- [x] `655cf19` records the SDK-backed Rust and Node CLI consumers as runnable
  examples and process-level integration tests. The Rust CLI constructs its
  filesystem through the public SDK, the Node CLI does the same through the
  napi-rs package, and both exercise durable SQLite shutdown/reopen paths.
- [x] `fd5eb04` forwards decoded numeric `open_flags` through the structural
  N-API adapter and changes the opt-in native lifecycle from an expected write
  refusal to mounted write/readback. The local macOS NFS run passed; other
  transport/platform acceptance remains open.
- [x] `31d62df` wires the Linux structural-driver FUSE mount/read/write/unmount
  acceptance job with explicit `/dev/fuse` prerequisites. The local harness and
  YAML validation passed; hosted Linux execution remains unverified.
- [x] `3355cc1` adds Unstorage timestamp metadata parity for atime, mtime, ctime
  and birthtime. Focused Rust, direct oracle and N-API tests passed; the other
  capability-limited rows remain open.
- [x] `323231f` exposes the FUSE `READDIR` body pack/unpack codec with UTF-8,
  alignment, bounded-packing and malformed-input coverage against the pinned
  oracle.
- [x] The current FUSE follow-up exposes and differentially tests the public
  `READDIRPLUS` body codec, including legacy/default layouts, integer coercion,
  truncation, padding and missing-entry errors. FUSE session and native-mount
  surfaces remain open.
- [x] The next disjoint W01 packets are also integrated: `750484d` adds 11
  oracle-classified Unstorage edge rows with zero skips; `26427f8` adds a
  72-step core handle/lifecycle trace with 63 successes, 9 expected errors and
  zero mismatches; and `8eac5cb` exposes the N-API FUSE `READ` request/raw-reply
  codec with protocol 7.41/7.8 differential and malformed-input coverage.
  The combined N-API suite passed; native FUSE session/device/mount and the
  remaining capability-limited rows remain open.
- [x] The latest W01 rotation is also integrated: `6ba3d62` adds `lchown`
  symlink lifecycle parity (101 steps, 79 successes, 22 expected errors,
  zero mismatches/skips); `58144fd`/`7238c829` classify the remaining
  Unstorage inventory (14 rows, 4 PASS, 10 ENOSYS, zero skips); and
  `63d9585`/`5c16164` expose the Rust-backed FUSE `CREATE` request/reply
  codecs across protocol 7.41/7.39/7.12/7.8. The full locked Rust and
  oracle-enabled N-API gates passed; native FUSE session/mount and live
  provider gates remain separate.
- [x] The next W01 rotation is integrated: `80f2efd` / `e5902bed` fixes
  Windows HostFs symlink path handling with a long-path fallback; `16b2180` /
  `9832b60` adds Rust FUSE `ACCESS` session dispatch and permission checks; and
  `7dd9a60` / `83466d7` exposes the Rust-backed N-API FUSE `LOOKUP` codecs.
  The combined locked Rust and oracle-enabled N-API gates passed. Hosted
  Windows, live R2, FSKit activation and privileged native mount evidence
  remain open.
- [x] The current implementation rotation is integrated: `72d570f` adds
  Windows read-only create/unlink and hard-link lifetime parity; `13c3acb`
  exposes the Rust-backed N-API `RELEASE`/`RELEASEDIR`, `FLUSH`, and
  `FSYNC`/`FSYNCDIR` codecs; and `b2040f6` hardens pure Rust FUSE INIT
  negotiation with six integration tests. The combined locked Rust workspace
  and pinned-oracle N-API gates passed. The N-API scoped Clippy run retains
  the pre-existing `js_driver.rs` type-complexity exclusion; hosted Windows,
  live providers, FSKit and privileged native mount evidence remain open.
- [x] The next three-way W01 rotation is integrated: `08731ed` adds typed
  Rust FUSE `READLINK`/`STATFS` wire/session behavior; `054fb95` exposes the
  matching napi-rs codecs, generated declarations/artifacts and pinned-oracle
  differentials; and `8e1f218` adds the opt-in Node SDK CLI native mount
  integration. The current-tree locked Rust workspace, oracle-enabled N-API
  suite, and macOS NFS Node CLI mount/read/write/unmount/persistence test all
  passed. Hosted Linux FUSE, hosted Windows, FSKit, live R2/PGlite and the
  remaining FUSE session/mount surfaces remain open.
- [x] The second W01 rotation is integrated: `7934062` validates FUSE
  `BATCH_FORGET` and fail-closed `INTERRUPT` sessions; `4dc90d5` exposes the
  matching napi-rs codecs and pinned-oracle tests; and `b13350f` replaces the
  remaining Unstorage capability skips with exact supported/`ENOSYS` assertions.
  The full locked Rust workspace, oracle-enabled N-API suite, 25-row Unstorage
  packet and previous native Node CLI acceptance all passed. Hosted Linux and
  Windows, FSKit, live R2/PGlite, and kernel-level cancellation remain open.
- [x] The third W01 rotation is integrated and published sequentially: `15026f2`
  validates FUSE `POLL` frames at the Rust session boundary; `5b7f982` exposes
  the matching napi-rs request/reply codecs and pinned-oracle tests; and
  `d28f31a` adds the opt-in Linux/macOS Node SDK CLI native gate to CI. The
  locked all-feature workspace, 50-test FUSE package gate, scoped Clippy,
  build/typecheck and full oracle-enabled N-API suite passed. The verified
  remote ref after publication is
  `10666e8dcca35c0cb39483317272de6832acb984`. Hosted CI and kernel poll
  semantics remain explicit acceptance boundaries.
- [x] The fourth W01 Rust FUSE packet is integrated locally as `1a8b112`:
  advanced-operation request framing (`FALLOCATE`, `RENAME2`, `LSEEK`, and
  `COPY_FILE_RANGE`) rejects malformed/trailing input with `EINVAL`, returns
  explicit `ENOSYS` for valid-but-unsupported operations, and preserves
  session state without mutation. Focused FUSE tests and strict scoped Clippy
  passed; sequential remote publication is pending.
- [x] The second-rotation implementation and documentation files were published
  sequentially to `origin/main`; the verified remote ref after that packet was
  `9c5f910489741c169031f2f737c3eb51ed427c89`. The local checkout remains
  intentionally divergent because the Contents API creates one remote commit
  per file.
- [x] Current-tree demo rerun after the second rotation also exited 0:
  `bash scripts/demo-end-to-end.sh` mounted the Rust CLI through macOS NFS,
  exercised independent Rust and Node clients on one mount, verified the
  cross-process bytes after unmount, and retained the backing data; the Node
  SDK CLI native self-test independently passed its NFS mount/read/write,
  unmount and persistence checks.
- [x] The three implementation packets were published sequentially to
  `origin/main`; their verified implementation ref before the tracker-only
  publication was `bcf465334273c8701161c0fb35be7bfc4ddfb538`, followed by
  tracker publication `b04f6ca4979cf632d74d31d9f6b55792f3f86f98`. The local
  checkout remains intentionally divergent because Contents-API publication
  creates one remote commit per file; no force-push or destructive
  synchronization was used.
- [x] `f1872f8` adds the Rust FUSE IOCTL session packet: exact 32-byte header/input-size framing, `EINVAL` for truncated/declared-size/trailing payloads, explicit `ENOSYS` for valid requests, and no state mutation. The isolated 12-test FUSE gate and strict scoped Clippy passed. The elevated macOS N-API regression suite also passed, including native NFS server integration and the pinned-oracle/Unstorage/distribution gates; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] `32ddee3` adds the N-API FUSE IOCTL codec packet: 32-byte request and 16-byte reply layouts, declared input-size framing, protocol-context handling, signed results, malformed/trailing rejection, and pinned-oracle differential coverage. The full oracle-enabled N-API suite passed after publication; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] `387940b` adds the N-API FUSE BMAP codec packet: typed request/reply layouts, protocol-minor coverage, all truncation boundaries, trailing-byte rejection and wrong-shape errors against the pinned oracle. The post-publication full oracle-enabled N-API suite passed; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] Final combined N-API verification on 2026-09-21 passed after rebuilding the release native binding: elevated `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX pnpm test` reported both IOCTL and BMAP pinned-oracle differentials, plus the full harness/server/CLI/NFS/9P/Unstorage/chunked/distribution gates. The first rerun correctly exposed a stale local native artifact missing the new exports; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] Current-tree W01/PGlite verification on 2026-09-21 passed: `cargo test --workspace --all-targets --all-features --locked --offline` exited 0; `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX sh scripts/test-pglite.sh` exited 0 with real provider parity, reconnect, version history, SQLite VFS reconnect, N-API/chunked/FUSE rows, Rust SDK 6 pass/3 R2 skips, Node SDK 5 pass/1 R2 skip, CLI 11 pass/1 R2 skip, and 40/40 PGlite-inclusive 621-operation trace lanes. The upstream suite reported 1,200 passed/82 skipped; skipped behavior and hosted platform/native gates remain open.
- [x] The current working-tree N-API packet exposes Rust-backed FUSE `IOCTL`
  request/reply codecs, typed declarations and public FUSE exports, while
  preserving the existing `READDIR`/`READDIRPLUS` declarations. The pinned
  oracle differential passed for all codec families, including fixed-width
  `IOCTL` request/reply framing, truncation, trailing-byte and wrong-shape
  checks; the Rust N-API crate passed 15 tests and the full N-API package gate
  passed. PGlite/R2 credentials and opt-in native mount rows remain explicit
  external gates.
- [x] The same working-tree packet extends the public FUSE barrel with typed
  namespace-mutation codecs for `SYMLINK`, `MKNOD`, `MKDIR`, `UNLINK`, `RMDIR`,
  `RENAME`, `RENAME2`, and `LINK`, including entry replies where applicable.
  The pinned oracle passed protocol 7.41/7.39/7.12/7.8/7.3 byte, round-trip,
  truncation and trailing-input checks; generated declarations, CommonJS
  named exports, the Rust N-API crate and the full package gate passed.
- [x] The current working-tree packet extends the public FUSE barrel with
  fixed-layout codecs for `FORGET`, `GETLK`, `SETLK`, `SETLKW`, `ACCESS`,
  `FALLOCATE`, `LSEEK`, and `COPY_FILE_RANGE`, plus explicit empty `DESTROY`
  and copy status replies, including typed lock and `LSEEK` replies. The
  pinned oracle passed protocol 7.41/7.39/7.12/7.8/7.3 request/reply bytes,
  raw framing for its intentionally unimplemented `COPY_FILE_RANGE`,
  round-trip, truncation and trailing-input checks; generated declarations,
  CommonJS named exports, the Rust N-API crate (15 tests) and the full N-API
  package gate passed. PGlite/R2 credentials and opt-in native-mount rows
  remain explicit external gates, so this packet does not close W01.1-W01.4.
- [x] The current working-tree packet also adds oracle-shaped whole-message
  FUSE framing helpers (`decodeRequest`, `encodeRequest`, `decodeReply`, and
  `encodeReplyFor`). Typed bodies, request extensions, unknown/raw opcode
  payloads, empty/status replies, error replies and malformed framing were
  differentially checked against the pinned oracle; this remains mount-free
  evidence and does not close W01.1-W01.4 while PGlite/R2, hosted platform,
  FSKit and privileged native-session gates remain unresolved.
- [x] The current working-tree packet exposes a mount-free Rust-backed
  `FuseSession` through `./fuse`. The focused lifecycle test negotiates INIT,
  serves root `GETATTR`, returns the N-API null no-reply shape for `FORGET`,
  covers negative lookup caching, callback-safe error/assertion reporting and
  post-destroy failure; the full oracle-enabled N-API suite and generated
  type/export checks pass. The
  mount-free session now covers INIT preferences, cache, negative-lookup, flush,
  debug and callback controls plus public counters; native mount/device
  lifecycle remains open, so W01.1-W01.4 stay unchecked.
- [x] The same session packet exposes the Rust-owned `maxRequest` and
  `useDriverIno` constructor controls, rejects zero/over-limit frames, keeps
  driver-identity versus path-identity hardlinks distinct as configured, and
  revives asynchronous protocol-limit failures as `ProtocolError`. The
  focused Rust session suite passes 14 tests, the sync-barrier suite passes 3
  tests, and FUSE-package strict Clippy passes; `attrTimeout`, `entryTimeout`,
  `negativeTimeout`, `keepCache`, `flushMechanism`, INIT preferences,
  debug/callbacks and session counters are covered. Native mount/device
  lifecycle and hosted platform evidence remain open.
- [x] The current W01 follow-up closes the next mount-free session behavior:
  Rust-backed lifecycle readback now reports negotiated protocol, destroyed
  state, live open-handle count, and a synchronized `session.inodes` view;
  `RENAME2` executes the oracle-supported `flags == 0` form while preserving
  the explicit `ENOSYS` fallback for unsupported flag extensions.
  `CARGO_INCREMENTAL=0 ./scripts/cargo-shared test -p mount-rs-fuse --test
  session --locked` passed 14/14; strict scoped FUSE Clippy, the release N-API
  build, `node test/fuse-codec.mjs` (including lookup/rename/forget/destroy
  inode readback) and `node test/typecheck.mjs` passed. Native device/mount
  lifecycle, hosted platforms and the remaining unsupported FUSE operations
  remain open.
- [x] The current-tree opt-in N-API native lane also passed with host mount
  permissions: `MOUNT_RS_NAPI_NATIVE_MOUNT=1
  MOUNT_RS_NAPI_TRANSPORT=nfs node integrations/mount-rs-napi/test/native.mjs`
  completed the actual macOS NFS structural-driver mount, mounted write/read
  rewrite, unmount, `unmountAll`, and live-mount cleanup checks. This is
  supported macOS NFS evidence only; Linux FUSE, FSKit and live-provider
  acceptance remain separate.
- [x] The current W01 native-option follow-up threads shared `useDriverIno`
  through the automatic N-API facade into the FUSE, 9P and NFS session option
  objects and maps focused native `fuse`, `9p`, and `nfs` option bags. The
  auto-facade unit tests, locked FUSE/9P/NFS scoped tests, release N-API build,
  generated typecheck, and the authorized macOS NFS native lane passed with
  the nested NFS bag and `useDriverIno: false`. The auto facade now also
  adopts an existing NFS server handle and verifies matching port identity in
  the same native lane; the generated 9P bag accepts an already-listened
  server handle. The automatic FUSE, 9P, and NFS option bags now own their
  transport-specific `onTransportError` hooks through the native session or
  mount-created server, with callback construction and teardown covered by the
  locked/build lanes and direct NFS/9P fault packets retaining event-delivery
  evidence. The authorized macOS NFS lane now configures both root and focused
  callbacks, exits cleanly with status 0, and releases the ignored mount-level
  hook when an already-created shared server is adopted. Package-level signal teardown also passed its opt-in child-process
  NFS test; automatic FUSE request callbacks, native/root callback event
  delivery, and the complete transport-specific option/session surface remain
  open; the generated
  `Mounted[Symbol.asyncDispose]()` contract and positive NFS
  `Mounted.port` readback are now exercised by the same authorized macOS NFS
  lifecycle lane. The configured FUSE `Mounted.source` mapping passes the
  Linux-target transport compile check; hosted Linux FUSE runtime evidence and
  FSKit remain open.
- [x] The host-enabled pinned-oracle N-API package harness was re-run after the
  transport-specific callback packet and passed through distribution
  aggregation. PGlite, R2, and opt-in native-mount rows remained explicit
  skips; this aggregate package result does not close hosted Linux FUSE/9P,
  FSKit, or native root/cross-transport callback event delivery.
- [x] The current HTTP-server option slice maps WebDAV `onTransportError`
  through the Rust and N-API server wrappers, and its malformed-HTTP Rust
  integration test receives a typed connection event. S3 now maps
  `drainTimeout` and callback construction/close ownership through the Axum
  gateway; its tracked TCP wrapper now reports a peer-aware connection event
  in the reset-on-close Rust test, and direct Node socket-reset delivery now
  produces one typed peer-aware callback event for both S3 and WebDAV.
  WebDAV/S3 Rust tests, N-API tests, strict transport Clippy, release build, generated
  typecheck, loopback server integration, and the host-enabled package harness
  passed.
- [x] The embedded server-object follow-up exposes read-only `S3Session` and
  `WebdavSession` views from their N-API server objects. Real loopback
  PUT/GET/404 traffic verified S3 bucket names and request/reply/error/operation
  counters, plus WebDAV method counters, lock count and assertion readback;
  the release addon and generated declarations passed. The S3 view now also
  exposes effective serializable session options, and the WebDAV view exposes
  active lock records with expiry cleanup; session-owned S3 bucket and WebDAV
  driver wrappers are also reachable without changing server-owned lifetimes.
  Direct session request methods,
  complete session/member parity, NFS session parity,
  and the broader transport object surface remain open.
- [x] The NFS server-object follow-up exposes a read-only `NfsSession` view from
  the N-API server object. Real loopback NULL/MOUNT/GETATTR traffic verified
  synchronized request/reply/error/drop/procedure counters, mount records, and
  destroyed-state readback, live `connections` while open and zero after close;
  malformed-record transport reporting and cleanup, NFS package integration
  targets, NFS 30/30 and N-API 16/16 library tests, release declarations,
  formatting, and focused cross-transport Clippy passed. NFS direct session,
  connection-object/handle parity, and the remaining production gates stay open,
  so this packet does not close W01.
- [x] The NFS codec subpath now re-exports the root `NfsServer`, `NfsSession`,
  `NfsSessionStats`, and `createNfsServer` identities without mutating root
  exports. The pinned NFS differential and generated typecheck passed; direct
  request methods and complete NFS v3/v4 parity remain open.
- [x] The NFS session view now exposes direct `handleCall` for raw unframed
  NFSv3 and NFSv4 RPC records, plus a read-only `Nfs4Session` view. The N-API
  server integration verified direct v3/v4 NULL replies and v4 teardown state;
  malformed-record, cleanup, package integration, generated typecheck/build,
  and lint gates stayed green. The Rust server now shares the v3/v4 handle
  table, path lock, and counters; connection-object, N-API shared-state view,
  handle parity, and complete direct-session parity remain open.
- [x] The NFS transport now reports active TCP connection tasks through
  `NfsServer::connections()`, uses an abort-safe guard, and awaits aborted
  connection tasks during close. The N-API server exposes that live count, and
  both `NfsSession.handles` and `Nfs4Session.handles` expose deterministic
  BigInt-backed snapshots of the shared table. Rust NFS tests (31 unit,
  rootless wire 1, transport errors 4, v4 barrier 1, v4 wire 2), the release
  addon, generated typecheck, and live N-API server integration passed; native
  mount, hosted/provider, full v4 state, connection-object, crash, and
  durability gates remain open.
- [x] The current NFS parity packet adds the shared `maxHandles` option with
  LRU eviction, root/current-entry protection, and NFSv4 open-state pins so a
  bounded table cannot silently split share reservations or byte-range state.
  The pinned oracle upstream NFS gate passed 266 cases with 18 explicit
  capability/root skips, the N-API NFS codec differential passed, and the
  focused NFS target passed 33 unit, rootless wire 1, pipelined concurrency 1,
  transport errors 4, v4 barrier 1, and v4 wire 4 tests; upstream `onError`,
  callback ID maps, N-API clock injection, and native/hosted/crash gates remain
  explicitly open.
- [x] The NFSv4 lock-cap follow-up applies `maxLocksPerFile` to extensions of
  existing lock state, preserves conflict-before-cap ordering, and adds a
  rootless wire assertion for the additional-range rejection. The complete
  NFS target and scoped Clippy pass; native Linux/hosted/crash gates remain
  open.
- [x] The NFSv4 channel follow-up aligns `CREATE_SESSION` with the pinned
  negotiation boundary: undersized fore responses return `NFS4ERR_TOOSMALL`,
  per-client exhaustion returns `NFS4ERR_NOSPC`, and back-channel count offers
  are preserved. The complete NFS target and scoped Clippy pass.
- [x] The NFSv4 `CREATE_SESSION` sequence slot now caches refusal replies for
  retransmission and advances to the next sequence for a retry; the focused
  wire test proves the refusal replay and successful next-sequence creation.
- [x] The NFSv4 reclaim policy now gates both `OPEN` and `LOCK` state
  establishment with `NFS4ERR_GRACE` until `RECLAIM_COMPLETE`; the affected
  rootless v4.1 round-trip and same-owner/cross-client share tests pass.
- [x] The NFSv4 owner translation boundary now supports deterministic static
  Rust/N-API maps plus synchronous panic-isolated Rust/N-API `nameOf`/`idOf`
  callbacks with domain-qualified user/group names, numeric fallback, and
  `NFS4ERR_BADOWNER` rejection for other domains. The live N-API v4.1 sequence
  exercises owner `GETATTR` and reverse owner `SETATTR` translation; the
  complete locked NFS target (38 unit, rootless wire 1, transport concurrency
  1, transport errors 4, v4 barrier 1, v4 wire 6), release addon/typecheck,
  live server integration, pinned codec differential, and strict affected
  Clippy pass. Deterministic seeded identities are covered by the rootless
  wire test.
- [x] The NFSv4 lease packet adds deterministic Rust `Nfs4Clock` control,
  automatic expiry before COMPOUND dispatch, explicit `sweep_expired`, and
  release of expired sessions, locks, open states, and pinned backend handles;
  the v4 wire test covers automatic and explicit expiry. N-API `now` bridges
  JavaScript millisecond callbacks onto a monotonic Rust clock and are covered
  by the live v4.1 sequence; native Linux/hosted lifecycle and crash/durability
  gates remain open.
- [x] NFS request-level error reporting now follows the upstream callback
  boundary: Rust `NfsSessionHooks` and N-API `NfsServerOptions.onError` report
  status failures without a call and decoded XDR/dispatch failures with the
  `NfsRpcCall`, while callback panics are isolated. The focused Rust callback
  test, complete NFS target (38 unit, rootless wire 1, transport concurrency 1,
  transport errors 4, v4 barrier 1, v4 wire 6), release addon/typecheck, live
  N-API harness, and strict affected Clippy pass. Native Linux/hosted lifecycle
  and crash/durability remain open.
- [x] The NFS transport concurrency packet now directly proves completion-order
  replies: a blocked NFSv3 `GETATTR` does not hold a later fast `GETATTR` on the
  same TCP connection, and both XIDs arrive exactly once. The focused target
  passed 2/2, the complete locked NFS target passed 39 unit tests, process
  restart 2, rootless wire 1, transport concurrency 2, transport errors 4,
  lifecycle 4, v4 barrier 1, and v4 wire 7; strict Clippy, formatting, and diff
  checks pass. Native-client ordering, cross-process concurrency, and crash/
  durability remain open.
- [x] The NFS server close boundary is terminal in Rust: the lifecycle lock
  orders close against listen, and subsequent `listen()` returns `NotConnected`
  instead of the stale former address. The focused lifecycle target passed
  5/5; the complete locked NFS target passed 39 unit tests, process restart 2,
  rootless wire 1, transport concurrency 2, transport errors 4, lifecycle 5,
  v4 barrier 1, and v4 wire 7; strict Clippy, formatting, and diff checks pass.
  Native-client ordering, cross-process concurrency, and crash/durability
  qualification remain open.
- [x] NFS connection close waiters now register with `Notify` before checking
  the completion flag, eliminating the lost-wakeup interval. A focused unit
  test passed 32 concurrent waiters plus a late waiter; the complete locked
  NFS target passed 40 unit tests and all applicable integration targets, with
  warning-denied NFS Clippy, formatting, and diff checks green. Native-client
  ordering, cross-process concurrency, and crash/durability remain open.
- [x] The real-TCP NFSv3 flow-control test now holds one `GETATTR` in the
  backend with `max_in_flight=1`, observes the second call remains undispatched,
  then verifies both XIDs return exactly once after release. The focused
  concurrency target passed 3/3; the complete locked NFS target passed 40 unit
  tests and all applicable integration targets, with strict Clippy, formatting,
  and diff checks green. Socket write backpressure and native-client ordering
  remain separate gates.
- [x] The host-backed NFSv4.1 forced-crash lane now writes a file with
  `FILE_SYNC4`, kills the seed server, observes `NFS4ERR_BADSESSION` for the
  old session and `NFS4ERR_STALE` for both old root/file handles, then opens
  the same path and reads the exact bytes through a replacement session.
  The persisted host file also has those bytes after the replacement exits.
  Both process-restart tests, the complete locked NFS target (40 unit, 1
  rootless native mountpoint claim with 1 mount ignored, 2 restart, 1 rootless
  wire, 3 concurrency, 4 errors, 5 lifecycle, 1 v4 barrier, 7 v4 wire), and
  warning-denied NFS Clippy pass; the process target also passed 10 consecutive
  reruns, with formatting and diff checks clean. This qualifies local one-host
  process-crash data recovery, not power-loss or durable v4 lease/replay/handle recovery;
  native-client ordering, exact-tip hosted qualification, and W01-NFS
  production acceptance remain open.
- [x] The same forced-child-process NFSv4.1 lane now also verifies host-backed
  namespace deletion. A file seeded in the host root is removed through a
  successful wire `REMOVE`; after the seed process is killed, a replacement
  session's wire `LOOKUP` returns `NFS4ERR_NOENT`, and the host path remains
  absent. The direct process-restart target passes 2/2; the full locked NFS
  target passes 41 unit and all applicable integrations (including 20 v4
  wire), and warning-denied Clippy, formatting, and diff checks pass. This
  is one-host process-crash namespace evidence, not a directory-fsync or
  power-loss guarantee, durable v4 session/lease/replay/handle recovery,
  native-client ordering, or exact-tip hosted acceptance; W01-NFS is NO-GO.
- [x] The forced-crash NFSv4.1 lane also verifies same-directory rename across
  a server-process replacement. A seeded host file is renamed by a successful
  wire `RENAME`; the replacement session sees `NFS4ERR_NOENT` for the old name
  and success for the new name through wire `LOOKUP`, while the host bytes at
  the new path remain exact. Direct process-restart passes 2/2, the full
  locked NFS target passes 41 unit and all applicable integrations (including
  20 v4 wire), and strict Clippy, formatting, and diff checks pass. This is
  process-crash namespace recovery, not directory-fsync/power-loss durability,
  durable v4 state, native-client ordering, or hosted acceptance; W01-NFS
  remains NO-GO.
- [x] The shared NFS RPC version router now advertises the actually served
  NFSv3..v4 range for unsupported NFS versions, rather than falling into the
  standalone v3 session's v3-only refusal. A real-TCP regression reproduced
  the old `3..3` response, then passed `3..4` for versions 2 and 5 while
  retaining MOUNTv3's `3..3`, unknown-program, RPC-version, auth, and valid
  v3/v4 behavior. TCP and direct N-API dispatch use one Rust router. The full
  locked NFS target, 266 pinned parity cases (18 explicit skips), affected
  strict Clippy, N-API release compilation/typecheck, formatting, and diff
  checks pass. The macOS 27 loader blocker is now fixed by a package-scoped
  no-strip release profile: the rebuilt addon loads, and direct NFS N-API
  server integration (including unsupported-version routing) passes with
  loopback permission. Native/hosted ordering,
  crash/power-loss durability, and W01-NFS production acceptance remain NO-GO.
- [x] The NFS transport now has a bounded stalled-reply close regression. An
  in-memory peer stops reading from a 16-byte reply buffer while two MOUNT
  NULL calls are pipelined with `max_in_flight=1`: the first write holds the
  only permit, the second call stays undispatched, and connection stop
  cancels the writer and retires the task without a false error callback.
  Focused and full locked NFS tests (42 unit tests plus applicable integration
  targets), strict Clippy, formatting, and diff checks pass. This is local
  userspace backpressure evidence, not native-client ordering, cross-process
  concurrency, crash/power-loss durability, hosted acceptance, or W01 GO.
- [x] The rootless NFSv4.1 replay-reconnect lane now completes a mutating
  `REMOVE`, disconnects, and retries its cached slot/sequence with a changed
  target. The exact old COMPOUND body returns without removing the second
  file; a fresh sequence removes it. The focused test passed 20 reruns, the
  complete locked NFS target passed (40 unit, 1 mountpoint claim with 1
  native mount ignored, 2 restart, 1 rootless wire, 3 concurrency, 4 errors,
  5 lifecycle, 1 v4 barrier, 8 v4 wire), and strict Clippy, formatting, and
  diff checks passed. This is completed-request replay in one live process,
  not in-flight same-slot ordering, crash-durable replay, native-client
  ordering, or production acceptance; W01-NFS remains NO-GO.
- [x] A controlled NFSv4.1 `GETATTR` backend stall now proves an in-flight
  same-slot retry reaches the server, remains pending, and then receives the
  exact cached original reply after the first operation finishes; the next
  sequence succeeds. The focused test passed 10 reruns, the complete locked
  NFS target passed (40 unit, 1 mountpoint claim with 1 native mount ignored,
  2 restart, 1 rootless wire, 3 concurrency, 4 errors, 5 lifecycle, 1 v4
  barrier, 9 v4 wire), and strict Clippy, formatting, and diff checks passed.
  The retry waits at the per-RPC global lease-sweep write lock, so prompt
  RFC-recommended `NFS4ERR_DELAY` and independent-slot overlap remain open;
  this is not crash-durable replay or production acceptance. W01-NFS is NO-GO.
- [x] The v4.1 slot admission path now records an active sequence and answers
  a fully decoded same-slot retry before the global lease-sweep lock: the
  blocked-backend real-TCP test receives bounded `NFS4ERR_DELAY`, a premature
  next sequence receives `NFS4ERR_SEQ_MISORDERED`, and the original reply is
  cached after release. A unit test covers slot-sequence wrap from `u32::MAX`
  to zero. The focused case passed 20 reruns; the complete locked NFS target
  passed (41 unit, 1 mountpoint claim with 1 native mount ignored, 2 restart,
  1 rootless wire, 3 concurrency, 4 errors, 5 lifecycle, 1 v4 barrier, 9 v4
  wire), with strict Clippy green. Independent-slot overlap, canceled-operation
  reply recovery, crash-durable replay, native-client ordering, power-loss
  durability, and exact-tip hosted acceptance remain open; W01-NFS is NO-GO.
- [x] A same-session, two-slot real-TCP v4.1 regression now proves slot 1
  completes while slot 0 remains blocked in backend `stat`. The initial
  non-escalated shared-target attempt could not open Cargo's `.cargo-lock`;
  the elevated pre-fix run compiled and timed out at the global per-request
  lease-sweep write lock. After the fix, the independent slot completes;
  v4 now takes that exclusive lock only when an expired client actually needs
  cleanup. An injected-clock race test confirms that an expired slot-1 request
  waits behind a blocked slot-0 call, then receives `NFS4ERR_BADSESSION` after
  the sweep. The prior expired-session wire test also passes. The overlap test
  passes both in the shared target and in an isolated `/private/tmp` target
  with loopback socket permission; the first non-escalated isolated run failed
  at bind with `EPERM`, not a code failure.
  The exact 283 MB disposable target was removed and verified absent. The
  complete locked NFS target passes (41 unit, 1 mountpoint claim with 1 native
  mount ignored, 2 restart, 1 rootless wire, 3 concurrency, 4 errors, 5
  lifecycle, 1 v4 barrier, 11 v4 wire), with strict Clippy and formatting
  green. This is bounded same-process overlap, not canceled-operation
  recovery, crash-durable replay/lease/handles, native-client ordering,
  power-loss durability, or exact-tip hosted acceptance; W01-NFS is NO-GO.
- [x] A canceled NFSv4.1 `REMOVE` now fences its session when the backend has
  deleted a file but the request is aborted before COMPOUND completion and
  reply caching. The controlled real-TCP test closes that connection after
  backend deletion;
  before the fix a changed-target retry returned `NFS4ERR_SEQ_MISORDERED`,
  while now it receives `NFS4ERR_BADSESSION` and cannot delete the second
  file. The same client creates a replacement session with request sequence 2;
  the corrected `CREATE_SESSION` response echoes 2, and a fresh `REMOVE`
  succeeds. The complete locked NFS target passes (41 unit, 1 mountpoint
  claim with 1 native mount ignored, 2 restart, 1 rootless wire, 3 concurrency,
  4 errors, 5 lifecycle, 1 v4 barrier, 12 v4 wire), with strict Clippy green.
  This is fail-closed replacement-session progress, not the canceled request's
  exact reply, uncached completed-reply handling, durable replay/lease/handle
  state, all partial-mutation outcomes, native-client ordering, power-loss
  durability, or exact-tip hosted
  acceptance; W01-NFS is NO-GO.
- [x] A completed cached NFSv4.1 `REMOVE` reply now records its decoded RPC
  credentials. A real-TCP retry from a different `AUTH_SYS` UID previously
  received the original cached body; it now gets `NFS4ERR_SEQ_FALSE_RETRY`
  without changing the cache or deleting the other file. The original UID
  still receives the cached body on a changed-target and changed-machine-name
  retry, then succeeds on a fresh sequence. The full locked NFS target passes
  (41 unit, 1 mountpoint
  claim with 1 native mount ignored, 2 restart, 1 rootless wire, 3 concurrency,
  4 errors, 5 lifecycle, 1 v4 barrier, 12 v4 wire); strict Clippy, formatting,
  and diff checks also pass. This checks effective-user replay consistency,
  not cryptographic `AUTH_SYS` identity, uncached reply
  recovery, crash-durable replay, native-client ordering, power-loss
  durability, or exact-tip hosted acceptance; W01-NFS is NO-GO.
- [x] A bounded completed NFSv4.1 reply is now cached even when
  `SEQUENCE.cachethis=false`, as permitted by RFC 8881. A new real-TCP
  `REMOVE` regression initially got `NFS4ERR_SEQ_MISORDERED` on a retry;
  after the fix it receives the original reply byte-for-byte and leaves a
  changed target intact until the next sequence. The full locked NFS target
  passes (41 unit, 1 mountpoint claim with 1 native mount ignored, 2 restart,
  1 rootless wire, 3 concurrency, 4 errors, 5 lifecycle, 1 v4 barrier, 13 v4
  wire), and the direct v4 wire target, warning-denied Clippy, formatting, and
  diff checks pass. This does not establish oversized-reply safety,
  crash-durable replay/lease/handle
  state, native-client ordering, power-loss durability, or exact-tip hosted
  acceptance; W01-NFS remains NO-GO.
- [x] An oversized completed NFSv4.1 reply with `SEQUENCE.cachethis=false`
  now stores a compact retry marker when it fits the negotiated cache limit.
  A real-TCP `REMOVE` plus large `READDIR` exceeded a 128-byte limit; before
  the fix a changed-target retry got `NFS4ERR_SEQ_MISORDERED`, but now it
  receives successful `SEQUENCE` plus `NFS4ERR_RETRY_UNCACHED_REP` on the
  original second operation. A repeated retry returns identical marker bytes
  without deleting the changed target, and the next sequence progresses.
  The full locked NFS target passes (41 unit, 1 mountpoint claim with 1 native
  mount ignored, 2 restart, 1 rootless wire, 3 concurrency, 4 errors,
  5 lifecycle, 1 v4 barrier, 14 v4 wire); direct v4 wire, warning-denied
  Clippy, formatting, and diff checks pass. `cachethis=true` oversized
  replies, limits below the marker size, crash-durable replay/lease/handle
  state, native-client ordering, power-loss durability, and exact-tip hosted
  acceptance remain NO-GO.
- [x] A cache-required (`SEQUENCE.cachethis=true`) oversized read-only
  `READDIR` tail now yields `NFS4ERR_REP_TOO_BIG_TO_CACHE` when the preceding
  successful results and error fit the negotiated bound. A real-TCP
  `REMOVE` + large `READDIR` regression failed before the fix because the
  reply exceeded a 128-byte cache; it now keeps the successful mutation
  result, caches the bounded error reply, and returns it exactly on a
  changed-target retry without a second deletion. The full locked NFS target
  passes (41 unit, 1 mountpoint claim with 1 native mount ignored, 2 restart,
  1 rootless wire, 3 concurrency, 4 errors, 5 lifecycle, 1 v4 barrier, 15 v4
  wire); direct v4 wire, warning-denied Clippy, formatting, and diff checks
  pass. Other oversized result types, prefixes too large for the error,
  crash-durable replay, native-client ordering, power-loss durability, and
  exact-tip hosted acceptance remain NO-GO.
- [x] The same bounded cache-required protection now covers an oversized
  `READ` tail. A real-TCP `REMOVE` + `LOOKUP` + 512-byte `READ` under a
  256-byte reply-cache bound returns a cacheable
  `NFS4ERR_REP_TOO_BIG_TO_CACHE` on `READ`, retains the successful `REMOVE`,
  replays the exact reply on a changed-target retry without deleting it, and
  progresses on the next sequence. The full locked NFS target passes (41
  unit, 1 mountpoint claim with 1 native mount ignored, 2 restart, 1 rootless
  wire, 3 concurrency, 4 errors, 5 lifecycle, 1 v4 barrier, 16 v4 wire);
  direct v4 wire 16/16, warning-denied Clippy, formatting, and diff checks
  pass. Other oversized results, prefixes too large for even the error,
  crash-durable replay, native-client ordering, power-loss durability, and
  exact-tip hosted acceptance remain open; W01-NFS is NO-GO.
- [x] A distinct oversized `READLINK` tail is now covered by that bounded
  cache-required error path. The new real-TCP regression failed before the
  fix because a 512-byte symlink target made the response exceed a 256-byte
  cache; after the fix the successful `REMOVE` prefix and
  `NFS4ERR_REP_TOO_BIG_TO_CACHE` on `READLINK` fit. A changed-target retry
  returns identical bytes without a second deletion, and the next sequence
  succeeds. The full locked NFS target passes (41 unit, 1 mountpoint claim
  with 1 native mount ignored, 2 restart, 1 rootless wire, 3 concurrency,
  4 errors, 5 lifecycle, 1 v4 barrier, 17 v4 wire); direct v4 wire 17/17,
  warning-denied Clippy, formatting, and diff checks pass. Other oversized
  results, prefixes too large for even the error, crash-durable replay,
  native-client ordering, power-loss durability, and exact-tip hosted
  acceptance remain open; W01-NFS is NO-GO.
- [x] An oversized read-only `GETATTR` tail now uses the same bounded
  cache-required error path. A real-TCP `REMOVE` plus broad supported
  attribute request exceeded a 160-byte cache before the fix; afterward
  the successful mutation prefix plus `NFS4ERR_REP_TOO_BIG_TO_CACHE` fits.
  A changed-target retry returns identical bytes without deleting the
  second file, then the next sequence succeeds. The full locked NFS target
  passes (41 unit, 1 mountpoint claim with 1 native mount ignored, 2 restart,
  1 rootless wire, 3 concurrency, 4 errors, 5 lifecycle, 1 v4 barrier,
  18 v4 wire); direct v4 wire 18/18, warning-denied Clippy, formatting, and
  diff checks pass. Other oversized replies, prefixes too large even for
  the error, crash-durable replay, native-client ordering, power-loss
  durability, and exact-tip hosted acceptance remain open; W01-NFS is NO-GO.
- [x] A fixed-size `GETFH` tail can also exceed a small required reply cache
  after a successful mutation. The new real-TCP `REMOVE` + `GETFH` test
  failed before the fix with a response over the 112-byte bound; now it
  returns a cacheable `NFS4ERR_REP_TOO_BIG_TO_CACHE` on `GETFH` while
  retaining the successful mutation prefix. A changed-target retry is
  identical and does not delete the second file; the next sequence succeeds.
  The full locked NFS target passes (41 unit, 1 mountpoint claim with 1
  native mount ignored, 2 restart, 1 rootless wire, 3 concurrency, 4 errors,
  5 lifecycle, 1 v4 barrier, 19 v4 wire); direct v4 wire 19/19,
  warning-denied Clippy, formatting, and diff checks pass. Other oversized
  replies, prefixes too large even for the error, crash-durable replay,
  native-client ordering, power-loss durability, and exact-tip hosted
  acceptance remain open; W01-NFS is NO-GO.
- [x] A completed `SEQUENCE(cachethis=true)` mutation whose reply cannot fit
  the negotiated cache now fences its session rather than advancing an
  uncached slot. A real-TCP `REMOVE` under a 96-byte cache previously
  returned an oversized success then `NFS4ERR_SEQ_MISORDERED` on a
  changed-target retry; the retry now receives `NFS4ERR_BADSESSION` and
  cannot delete the second file. Read-only oversized results retain their
  existing sequence behavior, as the state-limit wire case verifies. The
  full locked NFS target passes (41 unit, 1 mountpoint claim with 1 native
  mount ignored, 2 restart, 1 rootless wire, 3 concurrency, 4 errors,
  5 lifecycle, 1 v4 barrier, 20 v4 wire); direct v4 wire 20/20 and strict
  Clippy pass. The pinned NFS parity gate passes 266 with 18 explicit
  capability/root skips. This is same-process fail-closed replay, not exact-reply
  recovery, durable session/handle state, native-client ordering, power-loss
  durability, or exact-tip hosted acceptance; W01-NFS is NO-GO.
- [x] The manual hosted NFS run `35670927787` at `fb9caec8` passed its macOS
  native job, while Ubuntu passed native v4.1 and then failed before its v3
  mount because parallel tests collided on a timestamp-only mountpoint.
  Native mountpoints now use an atomic per-process claim with collision retry;
  a 32-way rootless claim test and the macOS native v3 mount pass. A separate
  local full-suite run exposed a `server.close()` worker-drain race, now
  addressed by a shared 200 ms graceful drain before abort fallback. The
  complete locked NFS target, 20 focused lifecycle reruns, warning-denied
  Clippy, formatting, and diff checks pass. The failed Ubuntu job does not
  count as native-v3 acceptance; a corrected exact-SHA hosted rerun is needed,
  and W01-NFS remains production NO-GO.
- [x] W01-NFS rejects malformed `AUTH_SYS` credential bodies
  before shared-router or direct v3/v4 dispatch, returning RPC `AUTH_BADCRED`
  instead of silently treating a truncated `AUTH_SYS` body as an absent UID/GID.
  The pre-fix-failing real-TCP test now passes five malformed and three valid
  cases, including a nonempty `AUTH_NONE` body allowed by RFC 5531; the
  rebuilt release addon passes direct unified/v3/v4 assertions.
  Full locked NFS, pinned 266-pass/18-skip upstream parity, complete N-API
  server integration, generated typecheck, strict affected Clippy, formatting,
  and diff checks pass locally. `AUTH_SYS` remains client-asserted identity;
  native-client ordering, crash/power-loss durability, exact-tip hosted
  acceptance, and W01-NFS production readiness remain open.
- [x] W01-NFS now has bidirectional real-TCP shared-handle lifetime evidence:
  v3 MOUNT/CREATE handles match v4.1 PUTFH/LOOKUP/GETFH; v4.1 REMOVE makes
  the old v3 handle stale; and v4.1 OPEN/CREATE matches v3 LOOKUP. The full
  locked NFS target passes 42 unit and all applicable integrations including
  21 v4 wire; pinned upstream parity passes 266 with 18 explicit skips, and
  strict NFS Clippy plus formatting/diff checks pass. The opt-in native macOS
  NFSv3 round trip passed on refreshed base `4376c07d`. Cross-process handle
  persistence, native-client ordering, crash/power-loss durability, exact-tip
  hosted acceptance, and W01-NFS production readiness remain open.
- [x] W01-NFS cross-version open-unlink state now has real-wire coverage:
  v4.1 OPEN/WRITE followed by v3 REMOVE makes v3 LOOKUP return
  `NFS3ERR_NOENT`, while the original v4.1 stateid still reads the exact
  payload and then closes. Full locked NFS (42 unit and 21 v4 wire), strict
  Clippy, pinned 266-pass/18-skip upstream parity, and opt-in native macOS
  NFSv3 mount pass locally. This does not establish cross-process open-state
  recovery, native-client ordering, power-loss durability, exact-tip hosted
  acceptance, or W01-NFS production readiness.
- [x] W01-NFS unlinked-open CLOSE now has explicit wire state retirement:
  `TEST_STATEID` reports `NFS4_OK` after v3 removes the name but before v4.1
  CLOSE, then `NFS4ERR_BAD_STATEID` for the same session/stateid afterward.
  Full locked NFS (42 unit, 21 v4 wire), strict Clippy, pinned
  266-pass/18-skip parity, and opt-in macOS native NFSv3 mount pass locally.
  Durable/cross-process v4 state, native-client ordering, power-loss, exact-tip
  hosted acceptance, and W01-NFS production readiness remain open.
- [x] W01-NFS actual macOS CLI-to-folder NFS smoke passed with fresh distinct
  host-backed source and mount directories under
  `/private/tmp/mount-rs-nfs-cli-smoke.Vggoo5`. The kernel reported the NFS
  mount; create, append, read, stat, and list through the mounted folder
  matched the 34-byte backing file. Ctrl-C exited cleanly with `unmounted`,
  the mount-table entry disappeared, and backing bytes persisted. Exact CLI
  command and prerequisites are recorded in `docs/W01_NFS_PROGRESS.md`.
  Linux/native v4.1 ordering, exact-tip hosted acceptance, durable v4 state,
  physical power-loss durability, and production readiness remain open.
- [x] The 9P session view now exposes direct `handleCall` for raw complete
  frames. The N-API loopback integration verified a direct Rversion reply on a
  live connection session; the full Rust 9P integration target, pinned 44-case
  differential, generated typecheck/build, and combined Clippy passed. Stream,
  attach, and complete 9P server-object parity remain open.
- [x] The S3 session view now exposes typed buffered `handleRequest` with
  header/response mapping. The N-API loopback integration verified a direct PUT
  with its required content-length independently of socket HTTP; loopback
  PUT/GET/404, S3 gateway 18/18, N-API tests/typecheck, release build, and
  scoped Clippy passed. The S3 session now retains debug-gated assertion
  messages/counters, with concurrent direct replies and the loopback lane
  staying clean. The S3 server also exposes live TCP
  `connections`, verified through idle-connection and disconnect cleanup in the
  Rust gateway and N-API loopback tests. The Rust gateway also verifies one
  peer-aware connection transport event for a reset-on-close fault. The S3
  session now also exposes true streamed `handleRequestStream` request and
  response bodies at the N-API boundary: the host-enabled loopback packet
  verifies content-length enforcement, three request chunks, multi-chunk
  response iteration, early iterator cancellation, and deliberate request-body
  failure mapping. The isolated compile, release addon/declaration build,
  generated typecheck, S3 transport target (4 unit, 6 chunked, 15 gateway, and
  5 public-API tests), scoped warning-denied Clippy, and full host-enabled
  package regression passed. Direct Node socket-reset tests now produce exactly
  one typed peer-aware callback event for both S3 and WebDAV, while complete
  S3 session/member parity remains unqualified. The effective S3 session
  options view and session-owned bucket wrappers are also verified by the
  loopback packet.
- [x] The S3 structural-factory packet now differentially covers empty maps,
  valid structural-driver maps, and empty/dot/dot-dot/slash/backslash/control/
  overlong bucket-name refusal at construction time against the pinned oracle;
  the N-API facade preserves the oracle's `TypeError` class/message and the
  native preflight matches its UTF-16 length/control-character boundary;
  hosted/native lifecycle and complete S3 session parity remain separate.
- [x] The S3 multipart lifecycle packet now proves staged state survives a
  replacement session, close sweeps every bucket idempotently while the
  session remains usable, and Complete/Abort has one terminal winner across
  concurrent session calls; the filesystem-visible exclusive finalization
  marker returns `NoSuchUpload` to the loser and late part writes, while a
  validation-failing Complete releases the marker so a correct retry succeeds.
- [x] The S3 multipart fault packet now injects one `EIO` while Complete reads
  a staged part, verifies the filesystem-visible finalization marker is
  released, and completes the same upload on retry; the full S3 target passes
  4 unit, 6 chunked, 23 gateway, and 5 public-API tests. This is bounded local
  fault-injection evidence, not power-loss durability, provider failure,
  broader ordering/concurrency, or native/hosted acceptance.
- [x] The follow-up S3 cancellation/concurrency packet stages streaming PUT and
  multipart part replacement behind unique private files and atomic rename;
  cancelling a request removes abandoned staging, preserves an existing part,
  and releases the debug in-flight ticket, while two concurrent multipart parts
  complete in numeric order. The loopback-enabled target passes 4 unit, 6
  chunked, 26 gateway, and 5 public-API tests; provider-backed failure,
  power-loss/torn-write durability, live AWS/R2, and native/hosted acceptance
  remain open.
- [x] The same multipart replacement flow is exercised through the generated
  N-API S3 facade: release build/declarations, direct session create/part/list/
  complete/GET, streamed traffic, cancellation, bucket isolation, connection
  cleanup, and a typed peer-fault callback all pass in the host-enabled server
  integration; this remains non-native and non-provider evidence.
- [x] The S3 N-API process-restart packet now runs a child process that leaves
  staged multipart state without calling `S3Server.close()`; a fresh native
  filesystem driver/session lists, completes, and reads the object through
  `node test/s3-restart.mjs`. This is restart evidence, not power-loss,
  provider, hosted-native, or crash-consistency acceptance.
- [x] The S3 Node `./s3` public-boundary packet now runs the pinned runtime
  audit in `node test/s3-barrel-scope.mjs`: 155 oracle exports and 255 package
  exports are compared, the exact 153 oracle-only codec/helper names are
  pinned as Rust-owned/out of the Node subpath, and the supported N-API S3
  server/session/streaming classes plus factory identity are retained. This is
  an explicit scope decision, not complete JavaScript codec parity; live
  provider, crash/power-loss, broader concurrency, and native/hosted evidence
  remain open.
- [x] The WebDAV session view now exposes typed buffered `handleRequest` and
  true streamed `handleRequestStream` with normalized headers, positional file
  response chunks, cancellation cleanup, and body-error propagation. The N-API
  host-enabled integration verified direct OPTIONS/MKCOL/PUT/HEAD/GET,
  PROPFIND/PROPPATCH, COPY/MOVE, LOCK/UNLOCK, DELETE, and PATCH refusal, plus
  chunked PUT,
  multi-chunk GET, early iterator return, and deliberate request-stream
  failure; WebDAV integration 13/13, isolated N-API compile, release build,
  generated typecheck, server integration, and scoped warning-denied Clippy
  passed. The same direct session packet exposes active `WebdavLockView`
  records after LOCK and observes zero records after UNLOCK. Direct Node
  socket-reset tests now produce exactly one typed
  peer-aware callback event for both S3 and WebDAV. The supported WebDAV
  full supported server/session prototype-member differential and full direct
  class 1/2/3 method matrix now pass; the oracle-only controls remain outside
  scope. Active lock-record readback and
  post-UNLOCK cleanup, 256 parallel unique-file direct-session PUT/GET
  requests, recursive owner XML readback, plus the session-owned driver
  wrapper, are verified. The parallel packet is limited to in-process
  same-driver concurrency.
- [x] The WebDAV concurrency boundary is explicit: the pinned HTTP oracle has
  no `PathLock`, so independent resources may run concurrently but no
  linearizable same-resource ordering or atomic same-target `PUT` publication
  is claimed. The focused Rust regression dispatches two simultaneous writes
  against one exclusive lock and both receive `423` without its submitted
  token; clients coordinate shared-resource writes through WebDAV lock/`If`
  state. Provider, power-loss, and durable-lock acceptance remain separate.
- [x] The WebDAV remote-response and recursive-mutation failure boundary is
  now fail-closed: `Depth: 1` `PROPFIND`, recursive `COPY`, and recursive
  `DELETE` ask `FsDriver::readdir_bounded` for at most 4,096 child resources
  per visited directory. `PROPFIND` maps provider `EOVERFLOW` to `413` with
  `Connection: close`; recursive mutations surface overflow or unsupported
  enumeration as per-resource `207` failures without traversing that
  directory. COPY also reports child-stat failures and rejects premature or
  over-reported source reads instead of silently returning incomplete success.
  The focused WebDAV target, package targets, warning-denied Clippy, and
  formatting pass; the native mount test remains explicitly ignored.
  Provider/native/hosted qualification, power-loss durability, durable locks,
  and broader ordering remain open.
- [x] The exact published-tip WebDAV audit remains externally gated: CI run
  `35685409287` for `baf19664` was cancelled with no jobs, while protected
  Live Cloudflare R2 run `35685409328` failed usage admission at
  `R2 CI monthly run cap already exceeded: count=297 limit=20` and skipped
  its integration job. No hosted WebDAV PASS is promoted; the R2 usage
  envelope, live provider configuration, power-loss durability, durable
  locks, and broader ordering remain open.
- [x] The WebDAV Basic parser now requires the oracle/RFC `Basic +<base64>`
  separator instead of accepting a scheme concatenated directly with the
  payload. The live authenticated HTTP regression rejects `Basic<base64>` and
  still accepts the configured credentials; provider/native/hosted lifecycle,
  power-loss durability, durable locks, and broader ordering remain open.
- [x] The WebDAV listener lifecycle now distinguishes a finished accept-loop
  task from a running server, allowing a serialized relisten to recover after
  an accept failure instead of returning false success. Focused Tokio task-state
  regressions cover finished and pending handles; deterministic socket-level
  accept-failure injection and hosted/provider/power-loss qualification remain
  open.
- [x] The WebDAV HTTP and N-API request-head adapters now preserve repeated
  header fields instead of silently taking the last value. Duplicate `If`
  fields join with grammar-safe whitespace, and a raw loopback regression proves
  one true plus one false state list still authorizes the request; hosted and
  provider qualification remain open.
- [x] The WebDAV N-API scope decision now records the oracle-only clock,
  assertion-callback, and live-lock-table boundaries explicitly. The focused
  Rust test `./scripts/cargo-shared test -p mount-rs-webdav --test webdav
  --locked injected_session_clock_controls_lock_expiry_deterministically`
  passed 1/1 and proves the native injected clock expires a lock at the exact
  millisecond boundary. N-API keeps serializable options, empty assertion
  readback, and expiry-aware `WebdavLockView[]` snapshots; broader session/
  server parity and the external W01 gates remain open.
- [x] The focused N-API SQLite WebDAV provider/reopen probe is now part of the
  package test sequence: `node test/typecheck.mjs && node
  test/webdav-sqlite.mjs` passed exact PUT-byte readback after orderly
  server/provider shutdown and a replacement-session zero-lock check. This
  classifies local SQLite byte persistence and process-local WebDAV locks only;
  crash/power-loss and live-provider durability remain open.
- [x] The focused N-API NodeFs WebDAV provider/reopen probe is now part of the
  package test sequence: `node test/typecheck.mjs && node
  test/webdav-node-fs.mjs` passed exact PUT-byte readback after orderly
  server/provider shutdown and a replacement-session zero-lock check. This
  classifies local NodeFs byte persistence and process-local WebDAV locks only;
  power-loss ordering and live-provider durability remain open.
- [x] The focused N-API NodeFs WebDAV process-crash probe is now part of the
  package test sequence: `node test/typecheck.mjs && node
  test/webdav-node-fs-crash.mjs` passed exact PUT-byte readback after forced
  child termination and a replacement-session zero-lock check. This classifies
  local NodeFs process-crash recovery and process-local WebDAV locks only;
  power-loss ordering, live-provider behavior, durable locks, and hosted
  lifecycle remain open.
- [x] The focused N-API in-flight WebDAV process-crash probe is now part of the
  package test sequence: three repeated `node
  test/webdav-inflight-crash.mjs` runs yielded and independently read back a
  streamed PUT prefix before forced child termination, then recovered the exact
  prefix through replacement NodeFs and SQLite providers with zero replacement
  locks. This classifies local in-flight process-crash recovery only; power-loss
  ordering, live-provider behavior, durable locks, hosted lifecycle, and hosted
  concurrency remain open.
- [x] The focused N-API provider-backed WebDAV concurrency probe is now part of
  the package test sequence: three repetitions at
  `MOUNT_RS_WEBDAV_PROVIDER_CONCURRENCY=128 node
  test/webdav-provider-concurrency.mjs` passed 128 concurrent direct-session
  PUT/GET pairs for both NodeFs and SQLite with exact bytes and matching method
  counters. This is local provider evidence only; hosted remote-provider,
  network, power-loss, durable-lock, and wider ordering gates remain open.
- [x] The focused host-enabled N-API provider-backed WebDAV network probe is now
  part of the package test sequence: three repetitions at
  `MOUNT_RS_WEBDAV_PROVIDER_NETWORK_CONCURRENCY=64 node
  test/webdav-provider-network-concurrency.mjs` passed 64 concurrent HTTP
  PUT/GET pairs for both NodeFs and SQLite, including streamed PUT/GET bodies
  and exact counters. This is local loopback provider evidence only; hosted
  remote-provider, hosted network, power-loss, durable-lock, and wider ordering
  gates remain open.
- [x] Direct JavaScript peer-fault qualification now drives abortive Node
  socket resets against both S3 and WebDAV after session-reply readiness. Each
  N-API callback delivered exactly once with the accepted peer, repeated
  pinned-oracle structural-driver runs passed, cleanup returned connections to
  zero, and the full host-enabled package regression passed against mountx
  `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`; complete session/member parity
  and broader W01 production gates remain open.
- [x] The `@mount-rs/core/webdav` subpath now preserves root server/class
  identity while exposing the WebDAV protocol literals, limits, status/errno
  tables, server defaults, and the pure path/header/If/XML document/parser primitives with `DavFault`
  refusal identity. Its pinned constants/path/header differential covers target
  normalization, href encoding, malformed targets, local/foreign destinations,
  invalid depth/overwrite/timeout/token values, tagged resources, entity tags,
  state-token submission, XML escaping, error/multistatus documents, and the
  supported-lock node. Bounded UTF-8/namespace/entity/DOCTYPE/depth handling,
  the `PROPFIND`/`PROPPATCH`/`LOCK` body grammars, deterministic lock-table
  lifecycle/coverage/conflict/expiry, and active-lock/discovery/response
  encoders also match the pinned oracle;
  generated typecheck and
  package distribution/export checks and the full host-enabled N-API package
  sequence passed after installing the pinned oracle's locked `unstorage`
  dependency. The pure WebDAV protocol export set is now complete, including
  bounded body collection, status mapping, XML response framing, and fault
  responses. Bind/refusal helpers, session property lists, and resource ETags
  now match the pinned server/session differential. The host-enabled direct
  session packet covers OPTIONS/MKCOL/PUT/HEAD/GET, PROPFIND/PROPPATCH,
  COPY/MOVE, LOCK/UNLOCK, DELETE, and unsupported-method refusal with XML,
  lock-token, and cleanup assertions. The same host-enabled integration now
  covers streamed request/response bodies with chunking, cancellation, and
  body-error assertions. The native session also exposes effective serializable
  options for realm,
  limits, lock policy, debug mode, and credential shape; the N-API factory
  maps lock-policy overrides and the same host-enabled integration verifies
  invalid zero-limit rejection plus Basic-auth challenge/acceptance. PGlite,
  R2, and native-mount rows remain explicit prerequisite skips.
- [x] The follow-up FUSE subpath export packet adds the oracle-shaped session
  defaults/factory, `handleMessage`, and invalidation helpers as direct
  CommonJS/TypeScript exports. The pinned-oracle FUSE suite, generated
  typecheck, and the full host-enabled N-API package harness passed; PGlite,
  R2 and opt-in native-mount rows remain explicit prerequisite skips.
- [x] The refreshed W01.3 trace lane passed all five pinned seeds across
  memory, SQLite, object-store, chunked-memory, chunked-SQLite,
  chunked-object-store, PGlite and chunked-PGlite (40 combinations, 621
  operations each) at oracle `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.
  Live R2 remains a separate prerequisite lane.

## W02 — Independent metadata, blocks and chunking

- [x] Land metadata/block contracts, fixed-size chunking, immutable blocks,
  fenced publication and ordered durability barriers.
- [x] Exercise local mixed-provider compositions.
- [x] `d6b80f4` persists chunker configuration/version metadata and covers partial
  writes, truncation, close/reopen and stale publication in the chunked store;
  the focused chunked suite passed 12/12 plus 7/7 concurrency cases locally.
- [ ] W02.1 Complete live mixed-provider matrix, including Node and CLI paths.
- [ ] W02.2 Verify stale writers, CAS conflicts, partial uploads, retry ambiguity,
  crash/reopen and provider capability failures across remote combinations.
- [ ] W02.3 Document format/version migration and chunk-size compatibility.
- [ ] W02.4 Define safe orphan cleanup and garbage collection before enabling it.
- [ ] W02.5 Preserve an extensible chunker interface. Fixed-size is initial scope;
  additional algorithms need explicit implementation and compatibility tests.

## W03 — Memory and SQLite persistence

- [x] The standalone SQLite reliability packet passes 8/8 deterministic cells
  across DELETE/WAL persistence reopen, supported single-writer contention,
  ENOSPC block-put injection, post-publish unknown-commit recovery, FULL
  synchronization and `integrity_check`.
- [x] Land memory and SQLite filesystem, metadata and block integrations.
- [x] Local provider run passed five memory and four SQLite library tests.
- [x] Local draft SQLite schema reopening fix uses explicit INSERT columns;
  this and additive versioning changes are **not yet committed**.
- [ ] W03.1 Review and land additive version storage without regressing old stores.
- [ ] W03.2 Test migrations, concurrent open, reopen, transaction rollback and
  uncertain commit behavior with revision-matched evidence.
- [ ] W03.3 Keep SQLite backend durability distinct from SQLite-hosting safety.

## W04 — PGlite

- [x] Land real socket-server integration and provider/factory coverage.
- [x] Fix transaction cleanup before releasing a connection slot (`29ffb3b`).
- [x] Add injected cleanup-failure regression (`cfce82a`): failed cleanup retains
  ownership and rejects unsafe reconnect rather than reusing the slot.
- [x] W04.1 Full `scripts/test-pglite.sh` run completed with exit 0. Passed stages
  include slot cleanup, injected failure, provider parity, reconnect, fencing,
  cancellation, disk restart, mixed stores, Node factories and userspace FUSE.
  Upstream suite passed with skips; all eight trace lanes passed five seeds of
  621 operations each. This local run does not replace hosted/live-R2 evidence.
- [x] Fresh current-tree rerun passed the same real-server lifecycle and then
  reported Rust SDK 4/4, Node SDK 5/5, CLI 6/6, upstream 1,194 passed/88
  skipped, and 40/40 PGlite-inclusive trace lanes. R2-only rows remained
  explicit skips without credentials.
- [x] The bounded test-server teardown race is covered by an exact PostgreSQL
  Terminate-frame cleanup path plus an I/O-turn barrier. The readiness slot test
  passed 10/10 and bounded close/reopen passed 5/5; the full PGlite and root
  gates then passed without the prior `Eio` reconnect failure.
- [x] W04.2 Confirm hosted macOS/Linux reruns close the previous reconnect failure.
  Isolated qualification run [35588994864](https://github.com/andymac4182/mount-rs/actions/runs/35588994864)
  on `andymac4182/c/w04-production-gate` contains the published W04 fix
  `bdfcb11`. Linux Node job
  [106298858958](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298858958),
  macOS-latest Node job
  [106298859119](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298859119),
  and macOS-15-intel Node job
  [106298859135](https://github.com/andymac4182/mount-rs/actions/runs/35588994864/job/106298859135)
  completed successfully. Direct logs and job metadata confirm the exact
  `Verify PGlite integration and restart recovery` step passed on all three
  platforms, as did fragmented HTTP early rejection; the prior Intel EPIPE
  failure did not recur. Current-tip qualification run
  [35635114595](https://github.com/andymac4182/mount-rs/actions/runs/35635114595)
  at published revision `d2db74dd` independently reconfirmed the Linux,
  ARM, macOS-latest, and macOS-15-intel exact PGlite/restart steps, with both
  macOS logs retaining the PGlite/chunked-PGlite trace, backup/restore/rollback,
  and zero-provider-failure markers. W04.2 is closed on this evidence.
  Production rollout
  remains separately tracked in
  [`docs/w04-progress-ledger.md`](docs/w04-progress-ledger.md) and remains
  NO-GO until artifact/package, persistence/rollback, provider, and
  operational gates also close.
- [x] W04.2 current-tip revalidation: replacement qualification run
  [35692153251](https://github.com/andymac4182/mount-rs/actions/runs/35692153251)
  ran from exact published `d870f900`. macOS-latest
  `106631238012`, macOS-15-intel `106631238033`, and Ubuntu
  `106631238088` completed successfully with both fragmented early-rejection
  and exact `Verify PGlite integration and restart recovery` steps green;
  ARM Node `106631237930` passed the same two exact steps, and Windows Node
  `106631238113` passed its package/distribution lane. The macOS, Ubuntu, and
  ARM logs include `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`; aggregate-native
  `106634465987` passed artifact aggregation, all five native package
  validations, and clean-consumer smoke. W04.2 remains closed and current-tip
  confirmed. The same run's provider/W26 failures are separate production
  blockers; rollout remains **NO-GO**.
- [x] W04.2 current-tip diagnostic boundary: fresh run
  [35708551380](https://github.com/andymac4182/mount-rs/actions/runs/35708551380)
  at pre-refresh source `b95fd6ad` does not supersede the historical W04.2
  closure. ARM, macOS-15-intel, Ubuntu, and macOS-latest Node all reached the
  exact PGlite/restart step and passed rollback, N-API, and mounted-PGlite
  markers, then failed only when the standalone provider-matrix lockfile was
  updated under `--locked`; shared `origin/main` now includes refresh
  `6797a2d8`. The same run retains W26/provider-capacity failures, native-FUSE
  rootless-operation failure without a retrievable direct log, and Ubuntu Rust
  timeout/test boundaries. Dispatch a fresh run from current `origin/main`
  after this ledger chunk; W04.2 remains historically closed, current-tip
  acceptance is pending, and production rollout remains **NO-GO**.
- [x] W04.2 current-main qualification evidence is in progress in run
  [35713659406](https://github.com/andymac4182/mount-rs/actions/runs/35713659406)
  at exact published head `c87adf7b`: ARM, macOS-latest, and macOS-15-intel
  Node jobs are terminal success with both exact recovery steps green and
  rollback/N-API/`providersFailed: 0` markers; Ubuntu Node `106699957273` is
  still queued. This is 3/4 current-tip Unix evidence only and does not change
  the historical W04.2 closure or production **NO-GO**. Inspect the Ubuntu,
  native/package/provider/W26, and Rust lanes before promoting current-main
  qualification.
- Production rollout packet refreshed in
  [`docs/W04-production-rollout.md`](docs/W04-production-rollout.md): exact
  candidate `d870f900`, run `35692153251`, aggregate-native package/consumer
  PASS, and the five retained native artifact IDs/digests are recorded as
  qualification provenance. The packet explicitly leaves production release,
  persistent-volume identity, encrypted backup/restore/RPO/RTO, provider
  scope, collector/pager, named owners, and GO approval open; provider IOPS
  and W26 failures remain current NO-GO evidence.
- A fresh W04 production-policy refresh is now tracked separately: run
  [35694364538](https://github.com/andymac4182/mount-rs/actions/runs/35694364538)
  at shared head `41032645` has queued `pglite-config` job `106638349819`.
  Prior run `35694245181` was cancelled before any job materialized during
  concurrent mainline pushes and is non-evidence. Queue state does not change
  the production NO-GO decision.
- Terminal W04 production-policy run
  [35694307118](https://github.com/andymac4182/mount-rs/actions/runs/35694307118),
  job `106637712246`, passed on shared head `25e275ab` with the explicit
  PGlite-only/durable/external-secret/bounded-TTL marker and four expected
  fail-closed negative markers. This closes the credential-free configuration
  shape check only; it does not close persistent deployment, backup/restore,
  provider, observability, ownership, or release approval gates.
- The rollout packet now has a structured seven-gate summary and a fail-closed
  repository check in `scripts/verify-w04-rollout-ledger.mjs`, with temporary-
  document coverage in `scripts/test-w04-rollout-ledger.mjs`. The existing
  policy workflow runs both checks so an implementation-only W04 closure cannot
  be promoted to production GO while P03–P07 remain open. Local policy and
  mutation cases pass; hosted execution and all deployment/provider/operator
  evidence remain external, so production stays **NO-GO**.
- Hosted run
  [35696523606](https://github.com/andymac4182/mount-rs/actions/runs/35696523606),
  job `106644703942`, completed successfully on head `5782aeee`. The exact log
  contains the existing PGlite policy PASS and four expected negative markers,
  followed by `W04_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO
  production_gates=7 open=5` and `W04_ROLLOUT_LEDGER_TEST_PASS cases=5`.
  This is terminal repository-control evidence only; the later queued
  current-head run `35696590337` is not promoted, and production remains
  **NO-GO**.
- Retained manual qualification run
  [35695427227](https://github.com/andymac4182/mount-rs/actions/runs/35695427227)
  targets exact SHA `e7850fb4`. ARM Node `106641134434` and macOS-15-intel
  Node `106641134367` passed the exact early-rejection and PGlite/restart
  steps. TiDB `106641134137` and TiDB/RustFS `106641134060` failed their
  ambiguous-commit functional boundary, while Ozone/FoundationDB
  `106641134132` failed at `396.44` lifecycle IOPS versus the hard `1000`
  target. macOS-latest and Ubuntu Node remain queued at this snapshot, so no
  current-tip full qualification or production approval is promoted; rollout
  remains **NO-GO**.
- The same retained run's RustFS `106641134074`, FoundationDB/RustFS
  `106641134166`, and base Ozone `106641134190` lanes passed their materialized
  block/metadata, restart, fault, and cleanup checks. Ozone/TiDB `106641134221`
  failed on TiDB optimistic write conflicts (`[kv:9007] ... Optimistic [try
  again later]`) and emitted `RUSTFS_COMBO_FAIL`; provider launch scope is
  still open and production remains **NO-GO**.
- Ozone-compositions `106641134304` then completed with SQLite/R2 lifecycle
  IOPS `127.46` (failed against the hard `1000` target) and PGlite/R2
  lifecycle IOPS `1287.12` (passed); cleanup passed, but the job is terminal
  failure. Its W26 evidence job `106652855215` is still queued and is not
  evidence, so provider capacity and production rollout remain **NO-GO**.
- W26 evidence job `106652855215` later downloaded the base, composition,
  TiDB, and FoundationDB artifacts but failed closed with
  `W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS`
  for the SQLite/R2 and PGlite/R2 composition scope. No W26 acceptance or
  production provider approval is promoted.
- Ubuntu Node `106641134421` has now completed successfully in the retained
  run: fragmented early rejection, exact PGlite/restart recovery,
  `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`, N-API integration, package/consumer
  smoke, and `providersFailed: 0` all passed. macOS-latest Node `106641134336`
  remains in progress, so the run is still partial and no new full-qualification
  or production claim is made.
- macOS-latest Node `106641134336` has now also completed successfully with
  the exact fragmented early-rejection and PGlite/restart steps,
  `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`, N-API integration, and
  `providersFailed: 0`. Together with ARM `106641134434`, macOS-15-intel
  `106641134367`, and Ubuntu `106641134421`, all four Unix Node recovery lanes
  pass for retained SHA `e7850fb4`; this does not replace latest-tip
  requalification.
- Native FUSE job `106641134269` failed `Exercise actual rootless kernel file
  operations`, skipped its downstream mounted checks, and was cancelled while
  its completion hook remained in progress; its direct log blob was unavailable
  (`BlobNotFound`). No native-FUSE acceptance is promoted, and production stays
  **NO-GO**.
- Aggregate-native `106655469380` completed successfully after the retained run
  materialized: `mount-rs N-API artifact aggregation: PASS`, all five native
  distribution-package validations, and `mount-rs clean consumer install/smoke:
  PASS` are terminally green. The five artifact IDs/digests are retained in
  [`docs/w04-progress-ledger.md`](docs/w04-progress-ledger.md) as qualification
  provenance only; this does not override native-FUSE, provider, W26, or
  production-release gates.
- Fresh manual exact-tip qualification run
  [35700938192](https://github.com/andymac4182/mount-rs/actions/runs/35700938192)
  was dispatched from the published `origin/main` SHA `33f52cda`; its four
  Unix Node jobs are `106658534854`, `106658534921`, `106658535116`, and
  `106658535160`, with Windows Node `106658534774` and native FUSE
  `106658535119`. All 23 displayed jobs were queued at the first snapshot, so
  no current-tip acceptance is claimed; production remains **NO-GO**.
- The same current-tip run has now produced one terminal result: ARM Node
  `106658534921` passed both exact `Verify fragmented request early rejection`
  and `Verify PGlite integration and restart recovery` steps, with
  `PGLITE_BACKUP_RESTORE_ROLLBACK_PASS`, N-API integration PASS, and
  `providersFailed: 0`. macOS-15-intel Node `106658534854` also passed both
  exact steps, and Ubuntu Node `106658535160` has now passed both exact steps
  as well. macOS-latest Node `106658535116` has now passed both exact steps
  and its mounted I/O cleanup. Native FUSE `106658535119` failed `Exercise actual rootless kernel
  file operations`, skipped mounted follow-on checks, and remained in its
  completion hook; package/provider/W26 gates are not terminal. This is
  partial current-tip evidence only;
  W04 and production rollout remain open/NO-GO until the full matrix is
  inspected and the seven production gates close.
- The same run then recorded a Windows Node timing failure and a provider
  failure: Windows Node `106658534774` built successfully and passed the N-API
  suite through WebDAV provider concurrency, but
  `webdav-provider-network-concurrency` hit `DOMException [TimeoutError]` at
  its 10-second fetch boundary before W04 recovery. The focused test passes
  locally and passed in retained Windows job `106641134169`, so this is a
  hosted rerun requirement rather than accepted current-tip evidence. TiDB
  `106658534683` failed `actual_tidb_commit_outcome_is_ambiguous_and_not_replayed`
  because a dropped publication response was reported as success, with a
  `[kv:9007]` optimistic write conflict; Windows Rust `106658535184` passed
  format, strict Clippy, and locked workspace tests. TLS compile/policy
  `106658534528` and Ubuntu native NFS `106658534834` also passed their
  terminal support gates. Production remains **NO-GO**.
- The same run was cancelled after Ubuntu Rust spent more than an hour in
  `cargo test --workspace --all-targets --locked` without a terminal result;
  comparable successful Rust jobs finish in roughly 1.5–3 minutes. W26 job
  `106670761354` independently failed closed with
  `W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-artifact-source-checkout-dirty`
  because the composition tee log was created inside the checkout before
  provenance capture. The follow-up workflow fix moves the W26 composition
  log/JSON to `$RUNNER_TEMP` and bounds the Rust matrix at 25 minutes;
  `benchmarks/storage/test.mjs`, the rollout policy checks, YAML parsing, and
  diff checks pass locally. A fresh hosted run is required; W04 current-tip
  acceptance and production remain **NO-GO**.
- [x] W04.3 Integrate versioning, mount-free VFS and native SQLite-hosting tests.
  The rebased packet (`43ded00`, `980cdd7`, `2d2ac5c`, `be2170b`, final
  rebased tip `7235fde`) adds durable PGlite version metadata, reconnect and
  version-history coverage, mount-free SQLite VFS tests and the PGlite gate;
  it was published sequentially through remote `90c33949`. Hosted
  macOS/Linux reconnect reruns remain the separate W04.2 gate.
- Production follow-up published at `68c087d1` (building on `113a1fbc`) adds
  optional `driver.storage.lease_ttl_ms` to the shared Rust/Node split-store
  config, preserves the 30-second runtime default, and requires an explicit
  positive safe-integer value no greater than 24 hours in an approved W04
  production config. Focused CLI/SDK/provider-matrix tests and positive/four
  negative credential-free policy fixtures pass. The manually dispatched
  current-main policy run `35642541696`, job `106475151000`, passed the
  explicit-TTL positive fixture and all four expected rejection markers;
  push-triggered run `35642363293` was cancelled before any job materialized
  and is excluded. The deployment-specific TTL, provider-scope,
  persistence/rollback, observability, ownership, and release decision remain
  tracked as **NO-GO** in
  [`docs/w04-progress-ledger.md`](docs/w04-progress-ledger.md).

- Current production-readiness checkpoint: the synchronized tree's clean N-API
  release build preserves `P9SessionStats.messages` as `Map<string, number>`
  after `d0a67b22`; generated typecheck, PGlite production-policy fixtures,
  the 2/2 disk-backed restart plus isolated backup/restore/rollback rehearsal,
  N-API artifact aggregation, focused FUSE 14/14 tests, formatting, and diff
  checks pass locally. The hosted run `35674630831` is excluded because its
  ARM job failed at the pre-`d0a67b22` generated declaration mismatch and was
  canceled. Replacement run `35675591961` is also explicitly excluded: its
  Unix and Windows Node jobs failed before W04 recovery on the generated
  `Record` versus `Map` declaration mismatch, and native FUSE exceeded its
  configured timeout. The current published tree `819c663e` retains the
  corrected P9 normalization and the WebDAV provider-network cleanup retry;
  a fresh exact-tip Node/native/package/provider run is still required.
  Production remains **NO-GO** for deployment persistence,
  backup/rollback, provider scope/performance, observability, ownership, and
  release approval.

- The production-facing Windows N-API server test was hardened after diagnostic
  run `35650347479`, job `106500944953`, reproduced the exact
  `S3 peer-fault callback (+19754ms)` timeout. Phase attribution was published
  in `3705ad0`; the deterministic incomplete-request plus
  `resetAndDestroy()` correction was published as `093565d9`. The rebuilt full
  pinned-oracle N-API package suite passes locally, and fixed-tip hosted
  requalification `35651055621`, Windows job `106503290630`, now passes the
  repaired server, restart, structural, package-distribution, clean-consumer,
  and artifact-aggregation checks. The overall qualification ended terminal
  failure because the ARM/Linux/macOS Node jobs hit the same
  `s3/multipart-complete-signed-trailer` ETag mismatch from the newer W01/S3
  finalization-marker path, and Ubuntu Rust/native-FUSE/provider gates also
  failed. This is recorded as a mixed-scope blocker; production remains
  NO-GO.
  Isolated current-main qualification `35654281185` independently reproduced
  the four-platform ETag mismatch before the recovery step, the Ubuntu Rust
  forced-unmount deadline failure, native-FUSE timeout, and sub-100 IOPS
  provider gates; it confirms the production NO-GO boundary on a terminal
  non-cancellable run.

- Current published production-candidate requalification: commit `d955042b`
  replaces the macOS-sensitive fragmented HTTP `ClientRequest` writer with an
  explicit TCP/HTTP writer and response parser, plus a narrowly scoped
  post-response EPIPE/reset guard. The focused regression passed 30 fail-fast
  local repetitions and the pinned-oracle HTTP differential passed 40/40 S3
  and WebDAV cases; `node --check`, shared Cargo formatting, and
  `git diff --check` passed. Full qualification run
  `35664315098` was dispatched from published head `27c96424`; ARM Node has
  already passed HTTP parity, early rejection, and exact PGlite/restart
  recovery, while macOS-15-intel is still building and macOS-latest has not
  started. This active run is not yet a current-tip W04 or production pass;
  queued, skipped, partial, or provider-failed evidence remains non-acceptance.
  The production rollout decision remains **NO-GO** pending the full current
  Node matrix, artifact/package provenance, deployment persistence and
  backup/rollback, provider scope/performance, observability, runbook,
  ownership, and release approval gates.

- Current W04 recovery chunks: WebDAV session parity now pins native/oracle
  directory and file mtimes before exact PROPFIND XML comparison; the focused
  parity test passed 30 repetitions, `node --check`, formatting, and
  `git diff --check`. Ozone cleanup now deduplicates the repeated block ID;
  the local `mount-rs-r2` suite passed 18/18 and the standalone Ozone workspace
  compiled with `--locked`. These changes were published as `9b2dabd7` and
  `23c0ba7e` respectively. The first full run `35664315098` remained queued
  after its macOS runner failed to materialize and was canceled after its
  pre-fix WebDAV `PROPFIND`, Ozone cleanup `block object` ENOENT, and
  pre-`bc292709` Ubuntu FUSE failures were diagnosed. Recovery run
  `35666527609` is active from `9b2dabd7`, before the Ozone fix; Windows and
  ARM Node have started while Linux/macOS/provider jobs remain queued at the
  ledger snapshot. No queued, partial, or pre-fix result is promoted to W04
  or production acceptance; production remains **NO-GO**.

- Recovery run `35666527609` has produced partial but useful evidence on its
  pre-Ozone-fix head: ARM Node job `106553420990` passed the exact PGlite
  integration/restart-recovery step and fragmented HTTP early-rejection step;
  Windows Node job `106553420495` passed package and consumer checks. Ozone /
  TiDB job `106553420645` reached the functional block contract but failed the
  hard 1,000-IOPS target at 30.34 IOPS, while Ozone/FoundationDB job
  `106553422646` passed durable chunk/reopen and cleanup markers but failed the
  same target at 56.72 IOPS. Intel macOS remains in progress and macOS-latest,
  Linux Node, and Ubuntu Rust remain queued at the current snapshot. This is
  not W04 or production acceptance; a current-main run containing `23c0ba7e`
  is still required for Ozone requalification and the complete Node matrix.

- Current published cleanup chunk: `integrations/mount-rs-napi/test/webdav-provider-network-concurrency.mjs`
  now retries only its temporary-tree removal after provider shutdown, covering
  the late SQLite journal/WAL directory transition observed locally. Five fresh
  concurrency-64 NodeFs/SQLite runs passed, the prior 20-run stress packet and
  full pinned-oracle N-API suite passed, and the focused implementation was
  rebased and pushed to `origin/main` as `819c663e`. A fresh hosted qualification
  from this exact published tip is required; this local test hardening does not
  close the hosted or production gates.

- Previous exact-tip qualification: full CI run `35678123095` was dispatched
  from published `0b6e8c4f01ebdd9254c7d6c61595628e0ce4a824` after the cleanup
  chunk and ledger publication, then completed with all four Node exact
  recovery steps green but native-FUSE and provider/composition failures. Its
  aggregate-native job later passed, while the run remained mixed and is
  retained as the diagnostic predecessor to current qualification
  `35680363083`; no queued, skipped, or partial state was promoted.

- Current failed-publication shutdown recovery: hosted native-FUSE job
  `106588864049` in run `35678123095` exposed a pending-atime flush attempting
  to publish after a metadata-publish fault had correctly fail-closed the
  filesystem. The fix now preserves that fail-closed state while releasing
  the provider lease during shutdown. Focused regression, full
  `mount-rs-chunked` library tests (21/21), package Clippy, formatting, and
  diff checks passed locally; rebased commit `3de49e33` is published to
  `origin/main`. Exact-tip Fault injection run `35679778367` passed all three
  operating-system jobs, and W04 policy run `35679778395` passed. Push CI run
  `35679778373` targets `3de49e33` but is pending with no jobs materialized;
  current hosted Node/native/package/provider evidence is still required and
  production remains **NO-GO**.

- Current integrated-main qualification: non-cancelling manual run
  `35680363083` at exact head `7cba89a5a1b4c03e7d52a2507ad8fa1deee71d0e`
  completed all four Node exact `Verify PGlite integration and restart
  recovery` steps, fragmented early-rejection checks, PGlite
  backup/restore/rollback markers, and `providersFailed: 0`. Native FUSE
  `106595824274`, native WebDAV/NFS/9P, Rust, observability, aggregate-native
  `106602050007`, all five native package validations, and clean-consumer smoke
  passed. The overall run remains terminal failure only on Ozone provider
  performance: Ozone/TiDB `132.07`, Ozone/FoundationDB `355.87`, and Ozone
  compositions `801.05` and `621.06` IOPS versus the hard `1000` target;
  dependent W26 evidence `106598856424` failed closed without
  `OZONE_IOPS_PASS`. The current `origin/main` tip later advanced to
  `8e23ca06`, so exact-tip requalification remains a release action. Production
  remains **NO-GO** pending provider scope/performance, deployment
  persistence/backup/rollback, observability/runbook, ownership, and release
  approval.

- Latest pre-fix exact-tip qualification: non-cancelling run `35683216317` was
  dispatched at `bdd87108e003a83a01cf9aa479a3e145dbd7cbe3`, before the
  deterministic WebDAV partial-request plus `resetAndDestroy()` correction
  `de78011a` reached `origin/main`. ARM `106604465303`, Ubuntu
  `106604465117`, macOS-latest `106604465439`, macOS-15-intel `106604465281`,
  and Windows `106604465193` all failed the same old WebDAV peer-reset
  assertion before the exact PGlite/restart step; no current-tip W04 Node
  acceptance is claimed. Native FUSE `106604465075` passed rootless,
  SQLite-fault, and mounted-PGlite checks, FoundationDB/RustFS `106604465204`
  passed its durable/restart markers, aggregate-native was skipped because the
  Node matrix failed, Ozone/TiDB measured `103.09`, Ozone/FoundationDB
  `354.51`, and Ozone compositions `779.82` IOPS against the hard `1000`
  target, and W26 `106607355160` failed closed without `OZONE_IOPS_PASS`.
  Current `origin/main` is `9563d2db`; a fresh exact-tip qualification is
  required and production remains **NO-GO**.

- Terminal diagnosis for current-tip qualification run `35684808019`: it was
  dispatched from exact published head `4cd57723` before later mainline WebDAV
  changes and ended **cancelled** after native-FUSE job `106609249088` failed
  `Exercise actual rootless kernel file operations` and its hosted `Complete
  job` hook remained in progress. ARM `106609248954`, Ubuntu `106609249118`,
  macOS-latest `106609248932`, and macOS-15-intel `106609248961` all failed
  `webdav/propfind-links mismatch` before the exact PGlite/restart step, with
  TypeScript HTTP 207 XML versus Rust HTTP 501 and an empty response. Ubuntu
  Rust `106609249086` failed the saturated-read timing assertion `Elapsed(())`.
  Ozone/TiDB `106609249034` measured `59.76` IOPS, Ozone/FoundationDB
  `106609249069` measured `339.24`, and Ozone compositions `106609249144`
  measured `837.80` and `789.33` against the hard `1000` target; W26
  `106611498619` failed closed without `OZONE_IOPS_PASS`, and aggregate-native
  was skipped. Windows Node `106609248992` passed package/N-API sub-gates only.
  The native-FUSE failure log was not retrievable because hosted cleanup never
  finalized. Current `origin/main` is `5e910e80`; this stale-tip diagnosis does
  not promote W04 or production acceptance. Dispatch a fresh non-cancelling
  qualification from the exact current tip and keep production **NO-GO**.

- Current-tip qualification boundary: run `35686870515` was dispatched from
  `6db7a3ca`, before the published structural-factory test correction
  `d34ccfee`. ARM `106615463353`, Ubuntu `106615463386`, macOS-latest
  `106615463377`, macOS-15-intel `106615463347`, and Windows `106615463597`
  all failed before PGlite/restart at `structural-factories.mjs:108`: the
  bounded native structural driver correctly returned `/tree/` `501 Not
  Implemented`, while the stale test still required `/tree/blocked`. The
  exact recovery steps were skipped. Native FUSE `106615463613` failed
  `Exercise actual rootless kernel file operations` and left its `Complete
  job` hook in progress; cancellation was requested after the substantive
  failure. Ubuntu Rust `106615463402` failed the saturated-read timing
  assertion. Ozone/TiDB `106615463375`, Ozone/FoundationDB `106615463216`,
  and Ozone compositions `106615463430` measured `275.84`, `125.39`, and
  `725.59/710.15` IOPS against the hard `1000` target; W26 `106617278027`
  failed closed without `OZONE_IOPS_PASS`, and aggregate-native was skipped.
  The local `d34ccfee` correction passed the release build, generated
  typecheck, structural/WebDAV integration, and runnable N-API suite. Publish
  this tracker/ledger update, then dispatch a fresh non-cancelling run from
  the resulting `origin/main`; production remains **NO-GO**.

- Fresh exact-tip qualification: manual run `35688367152` was dispatched from
  published `origin/main` `3c90ccb9` after the `d34ccfee` correction and its
  first snapshot contained all materialized Node, native, Rust,
  observability, and provider jobs in `queued` state. macOS-15-intel Node is
  `106619929088`, macOS-latest Node `106619929119`, ARM Node `106619929058`,
  Ubuntu Node `106619929260`, Windows Node `106619929057`, native FUSE
  `106619929163`, native WebDAV `106619929108`, native NFS `106619928997`,
  native 9P `106619929076`, Ubuntu Rust `106619929128`, and observability
  `106619929196`. Queue state is not acceptance; continue monitoring and
  classify exact PGlite/restart, native/package/provider/W26, and production
  gates only from terminal evidence. Production remains **NO-GO**.

- Pre-fix exact-tip qualification outcome: run `35688367152` later failed all
  four Unix Node jobs before PGlite/restart. macOS-latest `106619929119` and
  macOS-15-intel `106619929088` both failed `webdav/propfind-links mismatch`
  with TypeScript HTTP 207 XML versus Rust HTTP 501 and an empty body; ARM
  `106619929058` and Ubuntu `106619929260` failed the same parity boundary.
  Windows `106619929057` failed only because the correct WSAEADDRINUSE text
  was exposed as `GenericFailure`. Native FUSE `106619929163` failed rootless
  kernel operations and left its `Complete job` hook in progress; aggregate
  `106623776726` was skipped, provider composition jobs failed, and W26
  `106622558875` failed closed without `OZONE_IOPS_PASS`. This run is excluded
  as pre-fix evidence and production remains **NO-GO**.

- Published follow-up correction `140ede42` adds the missing bounded-directory
  delegation to `examples/http_oracle.rs` and normalizes Windows socket-bind
  wording in the N-API server facade to the public `EADDRINUSE` contract.
  Shared-target Rust compilation, 40-case local HTTP parity, N-API release
  build, JS syntax, and the 9P port-conflict boundary passed locally. The
  correction is included in current `origin/main` `48c9455a`; dispatch a new
  non-cancelling exact-tip qualification from that revision before promoting
  any current-tip recovery or production evidence.

- Active exact-tip qualification: non-cancelling manual run `35690190817` was
  dispatched from exact published head `48c9455a` after the correction and
  current documentation publication. At the first snapshot ARM Node
  `106625340801` was in progress; macOS-15-intel `106625340880`,
  macOS-latest `106625340836`, Ubuntu `106625340916`, Windows `106625340742`,
  native FUSE `106625340820`, native WebDAV `106625340733`, native NFS
  `106625340790`, native 9P `106625340829`, Rust `106625340784`, and
  observability `106625340718` were queued. Queue/in-progress state is not
  acceptance; keep production **NO-GO** until terminal exact recovery,
  package/native/provider/W26, persistence/rollback, operations, ownership,
  and release evidence is complete.

- Published S3 streaming-body response fix `d870f900370fe5a7b4235ac7e63f8c3b33efce99`:
  `transports/mount-rs-s3/src/server.rs` now drains an abandoned request body
  asynchronously so Hyper can send an early S3 error response without waiting
  for the full invalid upload. The local 20/20 fragmented early-rejection
  repetition, all 49 `mount-rs-s3` package tests, `http_oracle` compilation,
  40-case HTTP differential, formatting, and diff checks passed. This fixes the
  predecessor run's macOS-15-intel `Verify fragmented request early rejection`
  failure; it does not itself close hosted acceptance or production gates.

- Replacement exact-tip qualification: manual run `35692153251` targets
  published head `d870f900` and was fully queued at the 2026-09-22 15:49 AEST
  snapshot. Node jobs are `106631238012` (macOS-latest), `106631238033`
  (macOS-15-intel), `106631238088` (Ubuntu), `106631237930` (ARM), and
  `106631238113` (Windows); native FUSE is `106631238003`, native WebDAV is
  `106631237991`, native NFS is `106631238051`, native 9P is `106631238070`,
  Rust Ubuntu is `106631238143`, and observability is `106631238063`. Queue or
  in-progress state is not evidence; production remains **NO-GO** until the
  exact early-rejection and PGlite/restart steps, package/native/provider/W26,
  persistence/rollback, operations, ownership, and release gates are terminal
  and green.

## W05 — Cloudflare R2

- [x] Land object-store/R2 driver code and configurable endpoint support.
- [x] W05.1 Unblock D01 and run authenticated tests against actual Cloudflare R2.
  On 2026-09-20, main passed the explicit live filesystem contract test and
  SQLite-metadata/R2-chunk roundtrip/reopen test (two tests, no skips). The
  current CI credential path is the security-provisioned encrypted GitHub
  `r2-ci` environment, not a repository file or direct Keychain dependency;
  the final hosted acceptance is recorded in W05.7.
- [x] W05.2 Verify immutable writes, ranges, retries, reconnect, cleanup and
  concurrent publication with independently selected metadata providers.
  The 2026-09-21 isolated rerun at base revision `6f1ab93` passed the local
  signed-HTTP R2 adapter contract (`10/10` unit tests and `1/1` HTTP test):
  immutable create, ranged reads, stale conditional read/write rejection,
  fresh-client reopen, eight concurrent publications, prefix isolation and
  exact cleanup. The provider matrix reported `5` local Rust SDK passes,
  `4` skips and `0` failures; its R2 and PGlite rows skipped because
  `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`,
  `R2_SECRET_ACCESS_KEY`, and `PGLITE_DATABASE_URL` were unset. This is
  local S3-compatible evidence, not live Cloudflare R2 evidence; no dedicated
  transient-failure retry injection was run, so that isolated rerun did not
  close W05.2.
  The current live rerun passed both ignored Cloudflare R2 filesystem/CAS
  tests (`2/2`), the independent SQLite-metadata/R2-block composition
  (`1/1`), and the independent PGlite-metadata/R2-block composition (`1/1`),
  all with unique fixtures and exact cleanup. The signed-HTTP adapter test now
  injects one transient read failure and passed retry plus exact prefix cleanup
  (`2/2` HTTP tests); this is deterministic S3-compatible retry evidence,
  while the provider-side live R2 tests cover the remaining durability and CAS
  paths.
- [x] W05.3 Run Node, CLI/native, parity and benchmark lanes on live R2.
  Main executed actual R2 differential traces with seeds 4182, 1, 42, 65535,
  and 4294967295: 621 operations each, all 3,105 matched the pinned TypeScript
  oracle, with per-run snapshot cleanup. On 2026-09-20, the live N-API R2
  factory passed exact-key DELETE/HEAD cleanup, the configuration-driven HTTP
  CLI passed both split-store drives and owned-prefix cleanup, and the
  ComputeSDK-aligned smoke benchmark passed a 1 MiB fixed-64 KiB chunked
  PGlite/R2 run (write 2,902.11 ms, read 1,657.28 ms, 5.06 MiB/s, delete
  8.51 ms; one iteration, zero timeouts/failures). Native/hosted lanes and the
  full benchmark matrix remain open. The full `scripts/test-all.sh` rerun at
  `73c33e0` also passed the live R2 lane end-to-end; this does not close the
  native/hosted portions of this task.
  The 2026-09-21 isolated rerun reported `4` Node SDK passes, `2` skips and
  `0` failures; `9` CLI passes, `2` skips and `0` failures; and the storage
  benchmark unit gate passed. The Node R2 factory and PGlite rows skipped for
  missing configuration. The live Cloudflare CLI and service-evidence scripts
  stopped at credential preflight with `R2_ENDPOINT` unset; no remote request,
  write or cleanup ran. The full N-API suite reached native server checks only
  after elevated host access, then stopped on a stale local native binding
  (`binding[nativeName] is not a function`) while R2 remained an explicit
  credential skip. That isolated rerun therefore did not close W05.3.
  The current live rerun passed the complete Node/N-API suite with the pinned
  oracle, live R2 and isolated PGlite; the explicit macOS native NFS lane
  passed mounted read/write and teardown; the five-seed live R2 trace passed
  all `3,105/3,105` operations; the configuration-driven live R2 CLI passed;
  and the full public-NAPI benchmark passed all `8/8` iterations across 1,
  4, 10 and 16 MiB with fixed 64 KiB chunks, zero timeouts/failures and
  verified cleanup. Hosted CI receives only the encrypted `r2-ci` environment
  secrets; its live-R2 evidence remains a separate hosted acceptance boundary
  from local and native evidence.
- [x] W05.4 Record service identity and revision without recording credentials.
  `integrations/mount-rs-r2` now exposes a redacted `R2ServiceIdentity` and
  `scripts/r2-service-evidence.sh` records only endpoint authority, bucket,
  revision and owned-prefix object counts; embedded endpoint credentials are
  rejected. The focused R2 unit/Clippy gates and shell syntax check passed.
- [x] W05.6 Run the configuration-driven CLI gate against the canonical Cloudflare
  R2 endpoint with scoped S3 credentials. On 2026-09-20, the live gate passed
  both PGlite-metadata/R2-block and SQLite-metadata/R2-block drives, ranged
  reads, auth isolation, graceful reopen, object-presence checks and owned
  prefix cleanup. The runner now counts returned `Contents` because R2 may
  omit AWS's optional `KeyCount`; it does not weaken the cleanup gate.
- [x] W05.5 Fix live Node factory expected-byte assertion and guarantee unique
  cloud fixture keys with exact cleanup. Main reran the full PGlite/R2 script
  successfully: actual R2 Node factory and DELETE/HEAD cleanup, independent
  PGlite metadata + R2 blocks, provider lifecycle/restart/fencing, Node chunked
  factories and userspace FUSE. Upstream: 1,194 passed, 88 skipped; eight seeded
  lanes × five seeds × 621 operations passed. The object-store trace lanes are
  local, not live R2 traces. The subsequent full acceptance rerun passed with
  live R2 and PGlite enabled, while native privileged mounts and hosted CI remain
  separate evidence boundaries.
- [x] W05.7 Add usage-capped CI credentials, cost guard, and full hosted
  acceptance. Security-provisioned bucket-scoped Object Read & Write
  credentials are stored only as encrypted GitHub `r2-ci` environment secrets;
  the one-week token expires 2026-09-28 and no credential value is committed.
  The fail-closed budget job admits at most 20 accepted runs per UTC month at
  an assumed `$4` per run, for an `$80` envelope below the requested `$100`
  ceiling; the final run `35579757447` recorded `12/20` with eight slots
  remaining. A Cloudflare account-wide R2 alert is configured at `$80` as an
  early-warning notification. Hosted acceptance passed at `3db491e`: Rust SDK
  `9/0/0`, Node SDK `7/1/0`, CLI `14/1/0`, upstream `1200 passed/82 skipped`,
  five seeds × eight local/PGlite backends × 621 operations, live R2 trace
  `621/621`, live CLI, hosted N-API/service evidence, and artifact
  `10630468958`. An earlier hosted `ESTALE` shutdown failure in run
  `35575940720` was fixed by `c71c8ee`, which refreshes an expired unfenced
  lease during shutdown and includes a regression test; runs `35577687152` and
  `35579757447` then passed. Remaining actions are secret rotation before
  expiry and separate Linux FUSE, Windows, and signed/activated FSKit gates.
- [x] W05.8 Requalify the current successor's bounded WebDAV structural-driver
  boundary and local end-to-end packet. On 2026-09-22 at `63a969eb`, the
  focused structural-factory/oracle suite, complete Node CLI/SDK/N-API suite,
  real PGlite lifecycle/provider/CLI/SDK gate, upstream `1200 passed/82
  skipped`, and all `40 × 621` seeded differential lanes passed. R2,
  privileged native mounts, and hosted platform/provider gates remained
  explicit skips or separate acceptance boundaries; this local packet does
  not replace terminal same-SHA production evidence.
- [x] W05.9 Requalify the exact post-9P-admission release-candidate packet.
  Pushed `01f844c` passed `cargo fmt`, `git diff --check`, the full locked
  Rust workspace including the SQLite publication and HTTP/Windows parity
  successors, strict Clippy, optimized N-API build/postbuild, the complete
  elevated Node SDK/CLI suite, and `scripts/test-pglite.sh`. The packet
  reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`,
  and all `40 × 621` oracle traces. Current `origin/main` `7cc9c8c5` is a
  documentation-only successor over that tested implementation. R2,
  AWS/security/OIDC, privileged native, external-provider,
  package/provenance, scope, and final-audit gates remain explicit blockers;
  no credential value was read or stored and no Keychain access was attempted.
- [ ] W05.10 Close the production release path on one settled revision.
  Shared `origin/main` advanced to `05e320ad` while the W05.52 candidate was
  being qualified. Immutable candidate `7efded54` is locally green through
  the current Rust workspace/NFS/S3/Clippy, dyld-safe macOS N-API build and
  load, complete Node SDK/CLI/N-API suite, real PGlite lifecycle, and
  Rust/Node/CLI provider matrix. W04 policy `35714141926` and W08 policy
  `35714144646` passed; Fault `35714146067` is terminal-successful on all
  three OSes, Native 9P `35714145176` is terminal-successful including root
  conformance 146/146, W07 has compile/durable success with assembly queued,
  W08 has Linux/macOS build success with asset verification queued, and CI
  `35714144497` has a classified Ozone/TiDB hard-capacity failure at
  `84.01/1000` IOPS, the isolated macOS artifact finalization timeout, and
  remaining provider/native jobs incomplete. The prior immutable candidate
  `25e275ab` has terminal CI `failure`: TiDB/TiDB-RustFS stale harness
  assertions, Ozone/TiDB and Ozone/FoundationDB hard-IOPS misses, W26 dirty
  provenance, and cancelled native FUSE; the exact evidence and estimates are
  in `docs/w05-progress-ledger.md`. No production release is authorized.
  Remaining actions are to let the exact packet reach terminal status, repair
  any actionable failure on a new immutable candidate, close Ozone capacity
  and native-FUSE or record approved scope exclusions, obtain AWS protected
  inputs and OIDC trust through security, rotate R2 credentials after the
  UTC-month reset, close package/provenance/provider/scope gates, and run
  W20.6 for a written GO/NO-GO decision. No credential value was read,
  stored, printed, or placed in Keychain.

## W06 — RustFS integration service

- [x] Land isolated real-service harness and Linux CI job (`f5759cc`).
- [x] Main verified actual pinned RustFS contract tests, concurrent CAS, ranges,
  restart/fresh reads and guarded container/data cleanup; baseline run passed.
- [x] W06.1 Land real split SQLite/PGlite metadata and Node-factory gates,
  bounded Docker/process-group cleanup, and isolated combo orchestration.
  Main reran both timeout regressions and the full real-service harness after
  the final script changes: exit 0, block/split-provider/Node/restart gates
  passed. CLI integration beyond existing configuration tests remains open.
- [x] W06.2 Verify the hosted RustFS CI job, not just its configuration.
  Hosted runs `35493800880` / `35493696795` passed block/restart tests but
  failed cleanup of container-owned `.rustfs.sys` bind-mount files with permission
  denied. Ownership-validated cleanup is implemented and locally passed;
  hosted Linux confirmation passed at `37e9ba1`, job `106049192404`.
- [ ] W06.3 Add fault and benchmark workloads with reproducible service settings.
- [ ] W06.4 Provide isolated RustFS service orchestration for W07.6 and W08.5;
  test actual composed filesystems rather than unrelated backend smoke tests.

## W07 — FoundationDB

- [x] W07.1 Review and register the FoundationDB integration crate and its
  provider/test harness as a target-gated root workspace member. The feature-off
  package remains portable for client-free workspace and Windows all-features
  checks; native FDB compilation remains opt-in on supported targets.
- [x] W07.2 Finish isolated real FoundationDB client/server harness. Main ran
  the real pinned 7.4.7 Linux ARM64 server/client in Docker; the provider
  contract passed. The container supplies `fdb_c`; no host install or mock.
- [ ] W07.3 Resolve production lease/time semantics: default unsupported clock
  behavior and a development clock do not establish safe distributed fencing.
  The safety slice adds `with_production_lease_oracle` plus the
  `LeaseAuthorityKind::SharedProvider` declaration gate: unverified,
  development and single-authority clocks fail closed. The provider now also
  exposes a FoundationDB-hosted write-side `FoundationDbLeaseAuthority` and a
  read-only `FoundationDbSharedLeaseOracle`; the real-cluster integration test
  exercises two independent readers, missing-authority fail-closed behavior,
  backward-sample clamping, forward recovery and stale-writer fencing. Hosted
  runtime evidence now also exercises the process-owned authority, reader and
  storage `connect` paths. The hosted native CLI lane now publishes a current
  authority sample in a separate process before running the consumer with the
  shared-provider/read-only configuration. The authority publisher now also
  exposes a validated `LeasePublicationPolicy`: publication cadence must be
  shorter than the lease TTL and the accepted forward-jump bound cannot exceed
  that TTL. Its policy and bounded-forward-jump APIs fail closed before an
  unsafe wall-clock sample is written; unit coverage and the real-cluster
  authority/composition paths use that guard. Hosted run
  [35606741719](https://github.com/andymacclenaghan/mount-rs/actions/runs/35606741719)
  at revision `4aadbb1` passed the guarded authority/composition, restart and
  consumer paths on `ubuntu-24.04`; deployment-level authority credentials,
  clock monitoring/cadence and failover evidence remain pending, so this item
  is not yet marked complete.
  The latest policy-bearing hosted run
  [35655579378](https://github.com/andymacclenaghan/mount-rs/actions/runs/35655579378)
  (job
  [106518268071](https://github.com/andymacclenaghan/mount-rs/actions/runs/35655579378/job/106518268071))
  tested revision `03541161311983f497983ef0de1bcb09b958b2e4` on
  `ubuntu-24.04` and completed green in 11m06s. Its real-cluster authority,
  consumer, restart and native Linux paths passed, and its configuration
  fixtures passed with explicit bounded TTL while inline secrets and unsafe
  TTL failed closed. The retained artifact is
  `foundationdb-production-qualification-35655579378-1` with SHA-256
  `dff8fb664a62d70e7736098d842cc9d976e942358c203c75cdb028e9ea42c177`;
  provenance records runner `GitHub Actions 1000022432`. This remains hosted
  implementation qualification only; deployment-level authority credentials,
  monitored clock/cadence telemetry and failover evidence remain pending, so
  this item is not yet marked complete.
  The follow-up marker-enforcement run
  [35657842924](https://github.com/andymac4182/mount-rs/actions/runs/35657842924)
  (job
  [106525768365](https://github.com/andymac4182/mount-rs/actions/runs/35657842924/job/106525768365))
  tested revision `87a500b13bba305e7a7a5c80c328395d80eb772b` on
  `ubuntu-24.04` and completed green in 11m50s. The required
  `FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=120000
  publication_interval_ms=30000 max_forward_jump_ms=120000` marker was
  emitted by the policy test itself; the real-cluster authority, consumer,
  restart and native Linux paths passed, and the positive configuration
  fixture accepted explicit bounded TTL while inline secrets and unsafe TTL
  failed closed. The retained artifact is
  `foundationdb-production-qualification-35657842924-1` with SHA-256
  `dc37b10f82ecd1c4c4513f4cf3ecc9a95e2ef37ccdadf0f4d69e56d30620846b`;
  provenance records runner `GitHub Actions 1000022644`. This remains hosted
  implementation qualification only; deployment-level authority credentials,
  monitored clock/cadence telemetry and failover evidence remain pending, so
  this item is not yet marked complete.
  Hosted attempt
  [35662679910](https://github.com/andymac4182/mount-rs/actions/runs/35662679910)
  (job `106541321674`, revision `8c16299e`) passed all policy and build
  preconditions but stopped before provider execution because the standalone
  FoundationDB qualification lockfile omitted the `sha2` edge required by
  `mount-rs-r2`; this was a reproducibility failure, not runtime evidence.
  The one-line lock repair was published as `ccd3f671`. The terminal rerun
  [35663914822](https://github.com/andymac4182/mount-rs/actions/runs/35663914822)
  (job `106545260051`, revision `147c53a8`) completed green in 11m43s with
  the lease policy, authority/consumer, restart and native Linux paths passed;
  deployment-level authority credentials, monitored clock/cadence telemetry
  and failover evidence remain pending, so this item is not yet marked
  complete.
  Fresh mainline requalification
  [35665547966](https://github.com/andymac4182/mount-rs/actions/runs/35665547966)
  (job `106550374750`, revision `1b98ed04`) completed green in 9m47s from the
  published `origin/main` tip and re-ran the lease-publication policy,
  authority/consumer, service-restart and fresh-client paths. Deployment-level
  authority credentials, monitored clock/cadence telemetry and failover
  evidence remain pending, so this item is not yet marked complete.
  Current-tip requalification
  [35666991514](https://github.com/andymac4182/mount-rs/actions/runs/35666991514)
  (job `106554849953`, revision `5b9af323`) completed green in 11m23s after
  concurrent mainline reintegration and re-ran the lease-publication policy,
  authority/consumer, service-restart and fresh-client paths. Deployment-level
  authority credentials, monitored clock/cadence telemetry and failover
  evidence remain pending, so this item is not yet marked complete.
  Latest packet-guard requalification
  [35668646531](https://github.com/andymac4182/mount-rs/actions/runs/35668646531)
  (job `106559918401`, revision `564d0949`) completed green in 10m27s and
  re-ran the credential-free NO-GO packet admission checks plus the real
  authority/consumer, service-restart and fresh-client paths. Deployment-level
  authority credentials, monitored clock/cadence telemetry and failover
  evidence remain pending, so this item is not yet marked complete.
  Exact published-tip requalification
  [35669850643](https://github.com/andymac4182/mount-rs/actions/runs/35669850643)
  (job `106563637562`, revision `618ee5fe`) completed green in 11m37s and
  re-ran the credential-free NO-GO packet admission checks plus the real
  authority/consumer, service-restart and fresh-client paths. Deployment-level
  authority credentials, monitored clock/cadence telemetry and failover
  evidence remain pending, so this item is not yet marked complete.
  Workflow-artifact retention requalification
  [35671492720](https://github.com/andymac4182/mount-rs/actions/runs/35671492720)
  (job `106568742563`, revision `9460a62`) completed green in 12m21s and
  re-ran the credential-free NO-GO packet admission checks plus the real
  authority/consumer, service-restart and fresh-client paths. Deployment-level
  authority credentials, monitored clock/cadence telemetry and failover
  evidence remain pending, so this item is not yet marked complete.
  Terminal corrected-profile requalification
  [35674506961](https://github.com/andymac4182/mount-rs/actions/runs/35674506961)
  (job `106578027627`, exact revision
  `a7eb3be47e3e5edfd4b3e8a516d1b8cbceaab199`) completed green in 12m46s on
  Ubuntu 24.04/Linux `amd64`. It re-ran the credential-free production
  policy/negative fixtures, lease-publication guard, NO-GO packet guard and
  regression cases, then passed the live durable FoundationDB/RustFS,
  Node/N-API, Linux CLI/FUSE, service-restart/authority-republish,
  fresh-client and RustFS integration paths. The retained workload artifact
  records the fixed 400-iteration, 64-concurrency, 4 KiB profile with 400
  successful writes, reads and deletes, 1,200 successful lifecycle operations,
  measured 70.83 IOPS, zero timeouts and zero cleanup failures. The retained
  artifact is `foundationdb-production-qualification-35674506961-1` (ID
  `10672253304`) with SHA-256
  `e558dea65ac63d87873d36e6bff09daa70f824310309c1c1e6dd87c109b7b5b1`;
  provenance records runner `GitHub Actions 1000024242`, Node 24.21.0,
  RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`,
  FoundationDB image
  `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`,
  and the `mount-rs-split-foundationdb-r2` split-store topology. The retained
  packet is still `NO-GO` with seven open gates and zero evidence records;
  deployment-level authority credentials, monitored clock/cadence telemetry
  and failover evidence remain pending, so W07.3 is not marked complete.
  Fresh published-tip requalification
  [35675987457](https://github.com/andymac4182/mount-rs/actions/runs/35675987457)
  (job `106582524993`, exact revision
  `1670ceba81b24c1ed39b8ab396671324c7f28193`) completed green in 11m57s on
  Ubuntu 24.04/Linux `amd64`. It re-ran the policy/negative fixtures,
  lease-publication guard, NO-GO packet guard and regression cases, then
  passed the live durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE,
  service-restart/authority-republish, fresh-client and RustFS integration
  paths. The retained artifact is
  `foundationdb-production-qualification-35675987457-1` (ID `10673640911`)
  with SHA-256
  `0cb38b97b121ef8b6b69a1c4ef76d112bb24ba716b19eb6153ee52b13a3cc694`;
  the packet is still `NO-GO` with seven open gates and zero evidence records.
  Deployment-level authority credentials, monitored clock/cadence telemetry
  and failover evidence remain pending, so W07.3 is not marked complete.
  Current-main requalification
  [35678679681](https://github.com/andymac4182/mount-rs/actions/runs/35678679681)
  (job `106590602197`, exact source `94b76d79c1df2f98b4255ffdcb7d16f6bbe970d4`)
  passed the policy, negative-fixture, build, workload and Node/N-API gates but
  failed closed in the post-restart consumer-authority publication because the
  staged qualification clients had no continuously running authority publisher;
  the persisted authority therefore exceeded the validated 120-second
  forward-jump bound. This is a real production-shaped cadence gap, not a
  reason to relax the safety guard. The next correction adds an independent
  bounded authority heartbeat and checks its liveness across each staged client
  and service-restart gate; deployment-level credentials, monitored
  clock/cadence telemetry and failover evidence remain pending.
  Heartbeat-corrected current-tip requalification
  [35680085315](https://github.com/andymac4182/mount-rs/actions/runs/35680085315)
  (job `106594973866`, exact revision
  `71972b28ca7ae561325342ddf466c8353546547a`) completed green in 10m05s on
  Ubuntu 24.04/Linux `amd64`. It passed the policy and negative fixtures,
  rollout/qualification/workload/evidence regression cases, the validated
  lease policy and a bounded independent authority heartbeat at 30-second
  cadence with a 120-second maximum forward-jump bound; heartbeat liveness
  stayed true across staged clients and service restart. Durable
  FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, five-round soak,
  fresh-client and RustFS integration paths passed. The retained workload
  artifact records 400 successful writes, reads and deletes, 1,200 successful
  lifecycle operations, measured 202.31 IOPS, zero timeouts and zero cleanup
  failures. The retained artifact is
  `foundationdb-production-qualification-35680085315-1` (ID `10675087310`)
  with SHA-256
  `f9b47245760b4d03ddeeb9461b51bb1f29c8ce913920cbc629309ba024d2ecc5`;
  provenance records runner `GitHub Actions 1000024955`, Node 24.21.0,
  RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`,
  FoundationDB image
  `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`,
  and the `mount-rs-split-foundationdb-r2` split-store topology. The retained
  packet is still `NO-GO` with seven open gates and zero evidence records; the
  test heartbeat is not deployment-level authority credentials, monitored
  production clock/cadence telemetry or failover evidence, so W07.3 remains
  open.
  Telemetry-qualified current-tip requalification
  [35681642584](https://github.com/andymacclenaghan/mount-rs/actions/runs/35681642584)
  (job `106599666288`, exact revision
  `113b13751acc4885ca5916a7f70fe109e1dbadd4`) completed green in 10m51s on
  Ubuntu 24.04/Linux `amd64`. It passed the policy and negative fixtures,
  rollout/qualification/workload/evidence regression cases, the validated
  lease policy and an independent authority heartbeat at 30-second cadence
  with a 120-second maximum forward-jump bound. The shared-authority path
  emitted `FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3
  publication_successes=3 publication_failures=0 reader_attempts=5
  reader_successes=4 reader_failures=1 last_published_time_ms=2030001
  last_observed_time_ms=2030001`; durable FoundationDB/RustFS, Node/N-API,
  Linux CLI/FUSE, five-round soak, fresh-client and RustFS integration paths
  passed. The retained workload artifact records 400 successful writes, reads
  and deletes, 1,200 successful lifecycle operations, measured 441.76 IOPS,
  zero timeouts and zero cleanup failures. The artifact
  `foundationdb-production-qualification-35681642584-1` (ID `10675750181`)
  has SHA-256
  `5305418e1b253bdc621815311eaf58f80234ef193253fe4b9f23e3c0d938b23c`;
  the retained packet is still `NO-GO` with seven open gates and zero evidence
  records. The telemetry snapshots and test heartbeat are implementation
  qualification evidence, not deployment-level credentials, production
  collector/cadence monitoring or failover evidence, so W07.3 remains open.

  Telemetry-verifier current-tip requalification
  [35683503910](https://github.com/andymacclenaghan/mount-rs/actions/runs/35683503910)
  (job `106606452492`, exact revision
  `0e0327535fbcda0b1544400c00e938911a8ed6ad`) completed green in 12m44s on
  Ubuntu 24.04/Linux `amd64`. The stricter seven-case qualification-log
  verifier passed, including the 30-second/120-second authority heartbeat
  policy and reconciled shared-authority stats marker; durable
  FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart,
  five-round soak, fresh-client and RustFS integration paths passed. The
  base composition marker was p95/p99 29,220µs at 185.99 ops/s; five-round
  soak p95/p99 ranged from 13,491µs to 22,635µs with throughput from 176.18
  to 210.55 ops/s. The retained workload artifact records 1,200 successful
  lifecycle operations, 368.57 measured IOPS, zero timeouts and zero cleanup
  failures. Artifact `foundationdb-production-qualification-35683503910-1`
  (ID `10675579408`, SHA-256
  `ebe4ef52c09fa50e1395a0d672e1db715c9d60deb120f43c85499750ba6c6362`)
  remains qualification evidence; the packet is still **NO-GO** with seven
  open gates and zero evidence records. The stricter verifier, heartbeat and
  stats are implementation qualification evidence, not production collector,
  deployment credentials, failover, capacity or owner evidence, so W07.3
  remains open.

  Repaired exact-tip requalification
  [35686583792](https://github.com/andymacclenaghan/mount-rs/actions/runs/35686583792)
  (job `106614608451`, exact revision
  `7ba3998a6f6a88b7eedefd98cba9262980d24ce9`) completed green in 12m35s on
  Ubuntu 24.04/Linux `amd64` after the standalone FoundationDB lockfile was
  corrected. The seven-case qualification-log, four-case workload-artifact
  and twelve-case production-evidence regression suites passed; the bounded
  30-second/120-second authority heartbeat and reconciled stats marker passed;
  durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart,
  five-round soak, fresh-client and RustFS integration paths passed. Base
  composition recorded p50 3,133µs, p95/p99 32,028µs and 187.57 ops/s; soak
  p95/p99 ranged from 12,020µs to 12,490µs with throughput from 227.19 to
  235.58 ops/s. The retained workload artifact records 1,200 successful
  lifecycle operations, 166.29 measured IOPS, zero timeouts and zero cleanup
  failures. Artifact `foundationdb-production-qualification-35686583792-1`
  (ID `10676804103`, SHA-256
  `afccc878e46a096c6153f04fac017f3df19d37d147a2e83629fa14616f850e30`)
  remains qualification evidence; the packet is still **NO-GO** with seven
  open gates and zero evidence records. This repaired hosted checkpoint is not
  production collector, deployment-credential, failover, capacity, platform
  or owner evidence, so W07.3 remains open.

- [x] W07.4 Add conservative transaction/block limits, CAS, stale-writer and
  deterministic lease-fencing checks. Provider restart and hosted identity remain
  separate acceptance work.
- [ ] W07.5 Add Node, CLI, native-mount and macOS/Linux acceptance coverage. Rust
  SDK/CLI and the Node chunked factory now expose FoundationDB selection behind
  opt-in native features with explicit persisted single-authority/test and
  protected shared-provider modes. The latter requires an authority prefix and
  uses a read-only consumer API; deployment credentials must still enforce the
  provider-time publication boundary.
  The hosted FDB lane now builds the feature-enabled addon in the pinned
  libfdb_c client image and runs the live Node chunked factory against the real
  cluster, composing RustFS blocks when that lane is active. The same lane now
  has an explicit Linux FUSE prerequisite and runs the config-driven CLI native
  lifecycle/reopen test inside the client container with a separately
  published shared-provider authority when `/dev/fuse` is available; the script
  selects only that FoundationDB test so unrelated ignored FUSE cases cannot
  fail the lane. The live
  Node gate uses that same published authority prefix. The macOS native-NFS job
  now also compiles the FoundationDB-enabled CLI lifecycle test; live macOS
  service/cluster acceptance remains open, while the hosted Linux result is
  recorded under W07.6 below.
  The dedicated production workflow now has an independent macOS native-feature
  compile job plus an always-run `foundationdb-platform-evidence` aggregate.
  That aggregate downloads both platform artifacts, requires both upstream jobs
  to be terminally green, and runs
  `scripts/verify-w07-platform-evidence.mjs` against the Linux schema-2
  summary/log and the macOS compile marker. The macOS artifact now also carries
  a run-bound provenance marker, and the verifier requires its repository,
  workflow, ref, source revision, run and attempt to match the Linux schema-2
  provenance, while requiring a non-empty macOS runner identity, before
  emitting the aggregate pass. Platform runner identities remain distinct by
  design. This closes the
  evidence-packet
  integrity gap only: it does not claim a live macOS FoundationDB service or
  cluster, native mount, clean install, signing, or package acceptance. The
  terminal aggregate is green in
  [35700746198](https://github.com/andymacclenaghan/mount-rs/actions/runs/35700746198)
  at exact revision `8520e362710a4b3fe00fd567cf00fcc13e64c222`, with Linux job
  `106657901740`, macOS job `106657901971` and aggregate job `106660728286`;
  this closes packet integrity and macOS feature compilation qualification,
  not W07.5's live platform, mount, clean-install, signing or package gates.
  The repaired exact-tip aggregate is green in
  [35705886860](https://github.com/andymacclenaghan/mount-rs/actions/runs/35705886860)
  at revision `ab74870c58ab768c65679ea80feb18a8f54cbe00`, with Linux job
  `106674581511`, macOS job `106674582039` and aggregate job `106678244742`;
  shared repository/workflow/ref/SHA/run/attempt provenance matched, while
  the non-empty Linux and macOS runner identities remained distinct by
  design. The aggregate emitted `W07_PLATFORM_QUALIFICATION_PASS` with
  `provenance=bound`; this closes the strengthened packet-integrity
  control only, not W07.5's live platform, mount, clean-install, signing or
  package gates.
  The latest hosted platform checkpoint
  [35657842924](https://github.com/andymac4182/mount-rs/actions/runs/35657842924)
  (job
  [106525768365](https://github.com/andymac4182/mount-rs/actions/runs/35657842924/job/106525768365))
  tested revision `87a500b13bba305e7a7a5c80c328395d80eb772b` on
  `ubuntu-24.04` and passed the real Node/N-API, Linux CLI/FUSE mount and
  reopen, service-restart, durable FoundationDB/RustFS and RustFS integration
  paths. The retained schema-2 artifact reports `qualification-pass`, five
  soak rounds and the policy/provenance markers above. Live macOS
  service/cluster acceptance, the complete advertised platform/package
  matrix and production owner gates remain open.
  The follow-up hosted run
  [35663914822](https://github.com/andymac4182/mount-rs/actions/runs/35663914822)
  (job `106545260051`, revision `147c53a8`) completed green in 11m43s after
  the qualification lock repair. It passed the live Node/N-API, Linux
  CLI/FUSE mount and reopen, FoundationDB service restart/authority republish
  and RustFS restart/fault-recovery paths; the retained artifact is
  `foundationdb-production-qualification-35663914822-1` with SHA-256
  `098ab3a4bb1fd6ae84971cebbb67baf2e50d9cb271accdaa9351ec28e49c3042`.
  Live macOS service/cluster acceptance, the complete advertised
  platform/package matrix and production owner gates remain open.
  Fresh mainline requalification
  [35665547966](https://github.com/andymac4182/mount-rs/actions/runs/35665547966)
  (job `106550374750`, revision `1b98ed04`) completed green in 9m47s and
  passed the live Node/N-API, Linux CLI/FUSE mount and reopen, FoundationDB
  service restart/authority republish and RustFS restart/fault-recovery paths.
  Its retained artifact is
  `foundationdb-production-qualification-35665547966-1` with SHA-256
  `f37951fa1e20ce86bdc031d9529c1ace3506b03a9f2027b72b7186118eccbd02`.
  Live macOS service/cluster acceptance, the complete advertised
  platform/package matrix and production owner gates remain open.
  Current-tip requalification
  [35666991514](https://github.com/andymac4182/mount-rs/actions/runs/35666991514)
  (job `106554849953`, revision `5b9af323`) completed green in 11m23s and
  passed the live Node/N-API, Linux CLI/FUSE mount and reopen, FoundationDB
  service restart/authority republish and RustFS restart/fault-recovery paths.
  Its retained artifact is
  `foundationdb-production-qualification-35666991514-1` with SHA-256
  `7313f7ce6f7fb188d84d79a5c5d98df01a1319f071208f1058c5ab87a72acb12`.
  Live macOS service/cluster acceptance, the complete advertised
  platform/package matrix and production owner gates remain open.
  Latest packet-guard requalification
  [35668646531](https://github.com/andymac4182/mount-rs/actions/runs/35668646531)
  (job `106559918401`, revision `564d0949`) completed green in 10m27s and
  passed the live Node/N-API, Linux CLI/FUSE mount and reopen, FoundationDB
  service restart/authority republish and RustFS restart/fault-recovery paths.
  Its retained artifact is
  `foundationdb-production-qualification-35668646531-1` (ID `10670422714`)
  with SHA-256
  `f6f663ff26bb925cf601353a8e2c4e7d48bf15fbaeb7ba20163e32eb3c8a3897`.
  Live macOS service/cluster acceptance, the complete advertised
  platform/package matrix and production owner gates remain open.
  Exact published-tip requalification
  [35669850643](https://github.com/andymac4182/mount-rs/actions/runs/35669850643)
  (job `106563637562`, revision `618ee5fe`) completed green in 11m37s and
  passed the live Node/N-API, Linux CLI/FUSE mount and reopen, FoundationDB
  service restart/authority republish and RustFS restart/fault-recovery paths.
  Its retained artifact is
  `foundationdb-production-qualification-35669850643-1` (ID `10671065480`)
  with SHA-256
  `0def7863ba535c8b210865cf0cc0c13770f6fde268be4db9e4da6669e6abbd5c`.
  Live macOS service/cluster acceptance, the complete advertised
  platform/package matrix and production owner gates remain open.
  Workflow-artifact retention requalification
  [35671492720](https://github.com/andymac4182/mount-rs/actions/runs/35671492720)
  (job `106568742563`, revision `9460a62`) completed green in 12m21s and
  passed the live Node/N-API, Linux CLI/FUSE mount and reopen, FoundationDB
  service restart/authority republish and RustFS restart/fault-recovery paths.
  Its retained artifact is
  `foundationdb-production-qualification-35671492720-1` (ID `10672131196`)
  with SHA-256
  `815dfecadab366a9fca9a7501c4a36d771324d8f71b521e0e8082261cbe431c3` and
  was verified to contain the runtime log, summary and seven-gate
  `docs/W07-production-evidence.json` packet. Live macOS service/cluster
  acceptance, the complete advertised platform/package matrix and production
  owner gates remain open.
  The terminal corrected-profile run
  [35674506961](https://github.com/andymac4182/mount-rs/actions/runs/35674506961)
  (job `106578027627`, exact revision
  `a7eb3be47e3e5edfd4b3e8a516d1b8cbceaab199`) also passed live Node/N-API,
  Linux CLI/FUSE mount and reopen, durable FoundationDB/RustFS service
  restart/authority republish, fresh-client reopen and RustFS integration on
  Ubuntu 24.04/Linux `amd64`. This adds hosted evidence for the corrected
  workload artifact only; live macOS service/cluster acceptance, the complete
  advertised platform/package matrix and production owner gates remain open.
  Fresh published-tip requalification
  [35675987457](https://github.com/andymac4182/mount-rs/actions/runs/35675987457)
  (job `106582524993`, exact revision
  `1670ceba81b24c1ed39b8ab396671324c7f28193`) completed green in 11m57s and
  passed live Node/N-API, Linux CLI/FUSE mount and reopen, durable
  FoundationDB/RustFS service restart/authority republish, fresh-client
  reopen and RustFS integration on Ubuntu 24.04/Linux `amd64`. Its retained
  artifact is `foundationdb-production-qualification-35675987457-1` (ID
  `10673640911`) with SHA-256
  `0cb38b97b121ef8b6b69a1c4ef76d112bb24ba716b19eb6153ee52b13a3cc694`.
  Live macOS service/cluster acceptance, the complete advertised
  platform/package matrix and production owner gates remain open.
  Heartbeat-corrected current-tip requalification
  [35680085315](https://github.com/andymac4182/mount-rs/actions/runs/35680085315)
  (job `106594973866`, exact revision
  `71972b28ca7ae561325342ddf466c8353546547a`) completed green in 10m05s and
  passed live Node/N-API, Linux CLI/FUSE mount and reopen, durable
  FoundationDB/RustFS service restart/authority republish, fresh-client
  reopen and RustFS integration on Ubuntu 24.04/Linux `amd64`. It also kept
  the independent 30-second authority heartbeat live across staged clients
  and service restart, and retained the corrected 400-iteration workload
  artifact with 1,200 successful lifecycle operations, 202.31 measured IOPS,
  zero timeouts and zero cleanup failures. Its retained artifact is
  `foundationdb-production-qualification-35680085315-1` (ID `10675087310`)
  with SHA-256
  `f9b47245760b4d03ddeeb9461b51bb1f29c8ce913920cbc629309ba024d2ecc5`.
  Live macOS service/cluster acceptance, the complete advertised
  platform/package matrix, production capacity/observability/recovery and
  production owner gates remain open; the test heartbeat is not production
  monitoring or deployment-credential evidence.
  Telemetry-qualified current-tip requalification
  [35681642584](https://github.com/andymacclenaghan/mount-rs/actions/runs/35681642584)
  (job `106599666288`, exact revision
  `113b13751acc4885ca5916a7f70fe109e1dbadd4`) completed green in 10m51s and
  passed live Node/N-API, Linux CLI/FUSE mount and reopen, durable
  FoundationDB/RustFS service restart/authority republish, fresh-client
  reopen and RustFS integration on Ubuntu 24.04/Linux `amd64`. It also kept
  the independent 30-second authority heartbeat live and asserted
  `FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3
  publication_successes=3 publication_failures=0 reader_attempts=5
  reader_successes=4 reader_failures=1 last_published_time_ms=2030001
  last_observed_time_ms=2030001`; the retained corrected workload artifact
  records 1,200 successful lifecycle operations, 441.76 measured IOPS, zero
  timeouts and zero cleanup failures. Live macOS service/cluster acceptance,
  the complete advertised platform/package matrix, production
  capacity/observability/recovery and production owner gates remain open; the
  telemetry API and test heartbeat are not production monitoring or
  deployment-credential evidence.

  Telemetry-verifier current-tip requalification
  [35683503910](https://github.com/andymacclenaghan/mount-rs/actions/runs/35683503910)
  (job `106606452492`, exact revision
  `0e0327535fbcda0b1544400c00e938911a8ed6ad`) completed green in 12m44s and
  passed live Node/N-API, Linux CLI/FUSE mount and reopen, durable
  FoundationDB/RustFS service restart/authority republish, fresh-client
  reopen and RustFS integration on Ubuntu 24.04/Linux `amd64`. It also
  enforced the independent 30-second authority heartbeat and shared-authority
  stats marker, and retained the corrected 400-iteration workload artifact
  with 1,200 successful lifecycle operations, 368.57 measured IOPS, zero
  timeouts and zero cleanup failures. Live macOS service/cluster acceptance,
  the complete advertised platform/package matrix, production
  capacity/observability/recovery and production owner gates remain open; the
  telemetry API, test heartbeat and stricter verifier are not production
  monitoring, deployment-credential or failover evidence.

  The repaired exact-tip run
  [35686583792](https://github.com/andymacclenaghan/mount-rs/actions/runs/35686583792)
  (job `106614608451`, exact revision
  `7ba3998a6f6a88b7eedefd98cba9262980d24ce9`) completed green in 12m35s on
  Ubuntu 24.04/Linux `amd64`. It passed live Node/N-API, Linux CLI/FUSE mount
  and reopen, durable FoundationDB/RustFS service restart/authority republish,
  fresh-client reopen and RustFS integration; the independent 30-second /
  120-second authority heartbeat and shared-authority stats marker passed as
  well. The retained corrected 400-iteration workload artifact records 1,200
  successful lifecycle operations, 166.29 measured IOPS, zero timeouts and
  zero cleanup failures. Live macOS service/cluster acceptance, the complete
  advertised platform/package matrix, production capacity/observability/
  recovery and production owner gates remain open; the telemetry API, test
  heartbeat, stricter verifier and hosted checkpoint are not production
  monitoring, deployment-credential or failover evidence.

  The latest terminal cross-platform packet is green in
  [35709640688](https://github.com/andymacclenaghan/mount-rs/actions/runs/35709640688)
  at exact revision `8b4cbc3860bcb5c0fdfbb1a63cbe3b04f27a5b04`, with Linux job
  `106686845348`, macOS job `106686845149` and aggregate job `106690432291`.
  Linux and macOS run-bound provenance matched and the distinct platform
  runners were `GitHub Actions 1000027895` and `GitHub Actions 1000027899`;
  the aggregate emitted `W07_PLATFORM_QUALIFICATION_PASS provenance=bound`.
  This closes the latest packet-integrity and feature-compilation checkpoint
  only; live macOS FoundationDB service/cluster, native mount, clean install,
  signing and package acceptance remain W07.5 gates.

  The current-public-main follow-up is green in
  [35712676265](https://github.com/andymacclenaghan/mount-rs/actions/runs/35712676265)
  at exact revision `ff5cc58188d9783a80de96698a187e1f6416b83e`, with Linux job
  `106696766067`, macOS job `106696766203` and aggregate job `106702286990`.
  Linux passed the complete durable FoundationDB/RustFS, Node/N-API,
  Linux CLI/FUSE, service-restart, authority-heartbeat/stats, ten-round soak
  and bounded-workload lane; macOS passed the FoundationDB provider/test,
  native CLI lifecycle, N-API feature compile and bound-provenance lane; the
  aggregate emitted `W07_PLATFORM_QUALIFICATION_PASS provenance=bound`.
  Independent provenance, workload, summary-replay, platform,
  production-packet and rollout-ledger validators passed. This refresh
  strengthens current-head qualification evidence only; live macOS
  service/cluster, native mount, clean install, signing and package
  acceptance remain W07.5 gates.

- [x] W07.6 **FoundationDB metadata + RustFS S3 chunks:** main passed the real-service
  composition and provider contract in the full RustFS harness (exit 0), with
  multi-chunk round trips, fresh-client reopen, CAS and expired-writer fencing.
  The surrounding RustFS/PGlite VFS restart checks also passed, but are not
  FoundationDB service-restart evidence. The hosted CI lane and Docker harness
  now run the consumer feature checks, shared-provider authority publication,
  owned FoundationDB service restart, a separate post-restart authority
  republish, and fresh-client RustFS reopen; the terminal hosted result is
  recorded below. No emulated acceptance.

  The latest repaired exact-tip runtime qualification is hosted run
  [35709640688](https://github.com/andymacclenaghan/mount-rs/actions/runs/35709640688)
  at revision `8b4cbc3860bcb5c0fdfbb1a63cbe3b04f27a5b04`, with Linux job
  `106686845348`, macOS job `106686845149` and aggregate job `106690432291`
  all green. Linux passed the locked policy/evidence/workload preflight,
  durable FoundationDB/RustFS restart and republish, fresh-client reopen,
  RustFS integration, authority heartbeat/stats and ten-round soak; the
  bounded workload completed 1,200 operations at 338.55 IOPS with zero
  timeouts or cleanup failures. Independent provenance, workload,
  summary-replay, platform, packet and ledger validators passed. The
  latency spread is qualification telemetry rather than production capacity
  evidence; live production identity, recovery, observability, owner and
  release gates remain open.

  The current-public-main refresh is green in
  [35712676265](https://github.com/andymacclenaghan/mount-rs/actions/runs/35712676265)
  at exact revision `ff5cc58188d9783a80de96698a187e1f6416b83e`, with Linux job
  `106696766067`, macOS job `106696766203` and aggregate job `106702286990`.
  Linux passed durable FoundationDB/RustFS metadata and chunk composition,
  service restart and authority republish, fresh-client reopen, RustFS
  integration, heartbeat/stats, ten-round soak and the 400-iteration,
  64-concurrency, 4 KiB workload; all 1,200 lifecycle operations succeeded
  at 716.30 IOPS with zero timeouts or cleanup failures. Independent
  provenance, workload, summary-replay, platform, packet and ledger
  validators passed. This remains hosted qualification telemetry, not
  production identity, recovery, observability, capacity, owner or release
  evidence.

  The local arm64 durable qualification run on 2026-09-21 used the
  `foundationdb-soak-durable` composition name, three pinned FoundationDB
  7.4.7 server containers with `double`/SSD configuration, one bounded soak
  round, a replicated-node restart, authority republish and a fresh-client
  reopen. It emitted `FOUNDATIONDB_RUSTFS_CHUNKED_PASS`,
  `FOUNDATIONDB_SOAK_PASS rounds=1`,
  `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS` and
  `FOUNDATIONDB_TEST_PASS topology=durable`; the tested tree was `3b64a98`,
  published on main as `39a20b6`. This is local loopback/non-secure
  qualification evidence only: hosted acceptance, production identity/TLS,
  power-loss/backup recovery, capacity and operational gates remain open.
  Hosted run `35591729423` at revision `1ea183e` reached the durable
  FoundationDB configuration, transaction readiness, image match and cluster
  readiness markers, then failed before the composition client could start:
  `curl: (7) Failed to connect to host.docker.internal port 32768` and
  `FoundationDB client container could not reach the composed block endpoint`.
  The workflow was later superseded, so it is not hosted PASS evidence. This
  exposed the Linux loopback-publish portability gap fixed in `6d2a5d4`, which
  now attaches the owned RustFS container to the FoundationDB client network;
  a terminal hosted rerun of that fix remains required.
  The follow-up local arm64 durable run `foundationdb-network-cleanup` tested
  the fix before commit `65b521c` (published as `6d2a5d4`) and exited 0 with
  `FOUNDATIONDB_RUSTFS_NETWORK_READY`,
  `FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE status=403`,
  `FOUNDATIONDB_TEST_PASS topology=durable manifests=tests/foundationdb/Cargo.toml+integrations/mount-rs-foundationdb/Cargo.toml platform=linux/arm64 service_restart=pass soak_rounds=0`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`; owned network disconnect
  and cleanup also completed. This remains local qualification evidence only.
  The dedicated manually dispatched
  [W07 hosted qualification workflow](.github/workflows/foundationdb-production.yml)
  now runs this durable composition, five isolated bounded soak rounds, the live Node
  addon, the Linux native CLI/FUSE lifecycle and the service-restart/fresh-
  client checks with non-cancelling concurrency. It retains the terminal
  redacted log as a run artifact so a long W07 result is not invalidated by an
  unrelated mainline push; its result is still qualification evidence only.
  Hosted run
  [35598049389](https://github.com/andymac4182/mount-rs/actions/runs/35598049389)
  (job
  [106327338587](https://github.com/andymac4182/mount-rs/actions/runs/35598049389/job/106327338587))
  at revision `65c52b9` completed green on `ubuntu-24.04` in 12m15s. It
  emitted `FOUNDATIONDB_RUSTFS_NETWORK_READY alias=mount-rs-rustfs`,
  `FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE status=403`,
  `FOUNDATIONDB_RUSTFS_CHUNKED_PASS`, `FOUNDATIONDB_SOAK_PASS rounds=1`,
  `FOUNDATIONDB_NAPI_PASS image=node:24-bookworm`,
  `FOUNDATIONDB_SERVICE_RESTART_READY`,
  `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64 service_restart=pass soak_rounds=1`,
  `RUSTFS_COMBO_PASS name=foundationdb-production-qualification` and
  `RUSTFS_INTEGRATION_PASS`. This is terminal hosted Linux qualification for
  the tested revision; it does not close the production identity/TLS,
  backup/recovery, capacity, observability, macOS or release-owner gates.
A follow-up hosted run
[`35601357569`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35601357569)
(job
[`106337982523`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35601357569/job/106337982523))
at revision `622d0d1` also completed green on `ubuntu-24.04` in 11m38s. Both
production-config policy fixtures passed their expected positive/negative
outcomes, and the run emitted
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=11 p50_us=9893
p95_us=28865 p99_us=28865 total_ms=124 throughput_ops_per_sec=88.62`, followed
by the chunked, soak, N-API, restart, `FOUNDATIONDB_TEST_PASS`,
`RUSTFS_COMBO_PASS` and RustFS integration markers. This is terminal hosted
  Linux qualification and bounded workload measurement for the tested revision;
  it does not close production identity/ACL/TLS, backup/recovery, capacity,
  observability, macOS or release-owner gates.
  Hosted attempt
  [35606084750](https://github.com/andymacclenaghan/mount-rs/actions/runs/35606084750)
  at revision `2d4ca9f` reached the policy, prerequisite and N-API steps but
  stopped before provider execution because `tests/rustfs/Cargo.lock` was
  missing the `futures-util` dependency declared by `mount-rs-r2`. It is not
  runtime acceptance evidence; the lockfile correction was published in
  `4aadbb1` before the guarded-authority retry.
  The retry
  [35606741719](https://github.com/andymacclenaghan/mount-rs/actions/runs/35606741719)
  at revision `4aadbb1` completed green in 10m55s. It emitted
  `FOUNDATIONDB_RUSTFS_NETWORK_READY`, `FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE
  status=403`, `FOUNDATIONDB_LATENCY_PASS workload=composition operations=11
  p50_us=11702 p95_us=26193 p99_us=26193 total_ms=120
  throughput_ops_per_sec=91.24`, `FOUNDATIONDB_RUSTFS_CHUNKED_PASS`,
  `FOUNDATIONDB_SOAK_PASS rounds=1`, `FOUNDATIONDB_NAPI_PASS`,
  `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=1`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. This is terminal hosted Linux qualification and
  bounded workload/clock-guard evidence only; production identity/ACL/TLS,
  backup/recovery, capacity, observability, macOS and release-owner gates
  remain open.
  The current published workflow revision `976725e` expanded the dedicated
  qualification to five isolated real-provider rounds. Hosted run
  [35608336345](https://github.com/andymacclenaghan/mount-rs/actions/runs/35608336345)
  (job
  [106360895920](https://github.com/andymacclenaghan/mount-rs/actions/runs/35608336345/job/106360895920))
  completed green on `ubuntu-24.04` in 11m38s. It emitted the expected
  positive/negative config-policy markers, shared-network endpoint reachability
  (`403`),
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=11 p50_us=9990
  p95_us=85061 p99_us=85061 total_ms=167 throughput_ops_per_sec=65.76`,
  `FOUNDATIONDB_RUSTFS_CHUNKED_PASS`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_NAPI_PASS image=node:24-bookworm`, service restart readiness,
  `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. This is a stronger bounded hosted qualification
  signal, not production-duration, capacity, failover, or release acceptance.
  The corrected follow-up
  [35620006731](https://github.com/andymacclenaghan/mount-rs/actions/runs/35620006731)
  (job
  [106402140768](https://github.com/andymacclenaghan/mount-rs/actions/runs/35620006731/job/106402140768))
  tested revision `aa3dae3` on `ubuntu-24.04` and completed green in 11m05s.
  Its retained qualification artifact reported `qualification-pass` and
  emitted `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9593
  p95_us=48162 p99_us=48162 total_ms=171 throughput_ops_per_sec=87.35`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. This closes the hosted Linux/native-FUSE
  qualification checkpoint for that revision; production identity/ACL/TLS,
  backup/recovery, capacity, observability, macOS and release-owner gates
  remain open.
  A prior non-cancelling hosted run
  [35623491280](https://github.com/andymac4182/mount-rs/actions/runs/35623491280)
  (job
  [106415143857](https://github.com/andymac4182/mount-rs/actions/runs/35623491280/job/106415143857))
  tested current-main revision `336d9a3` on `ubuntu-24.04` and completed green
  in 12m26s. Its retained artifact
  `foundationdb-production-qualification-35623491280-1` reported
  `qualification-pass`, `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  five soak rounds, `FOUNDATIONDB_LATENCY_PASS workload=composition
  operations=15 p50_us=13140 p95_us=61081 p99_us=61081 total_ms=233
  throughput_ops_per_sec=64.28`, `FOUNDATIONDB_TEST_PASS topology=durable
  ... platform=linux/amd64 service_restart=pass soak_rounds=5`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. This is a prior bounded
  current-main qualification checkpoint; its retained artifact
  `foundationdb-production-qualification-35623491280-1` has SHA-256
  `b353ac2744dae9c469f7807533f8eec0d52e49120a101552c428585e0559741d`, and
  the workflow summary records the repository, source revision, ref, runner,
  workflow, run ID and attempt. It is not production-duration, capacity,
  identity/ACL/TLS, backup/recovery, macOS or release-owner evidence.
  The latest non-cancelling hosted run
  [35627206302](https://github.com/andymac4182/mount-rs/actions/runs/35627206302)
  (job
  [106424385715](https://github.com/andymac4182/mount-rs/actions/runs/35627206302/job/106424385715))
  tested current-main revision `87db9fd` on `ubuntu-24.04` and completed green
  in 10m47s. Its retained artifact
  `foundationdb-production-qualification-35627206302-1` reported
  `qualification-pass`, `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  five soak rounds, `FOUNDATIONDB_LATENCY_PASS workload=composition
  operations=15 p50_us=7981 p95_us=23550 p99_us=23550 total_ms=124
  throughput_ops_per_sec=120.85`, `FOUNDATIONDB_TEST_PASS topology=durable
  ... platform=linux/amd64 service_restart=pass soak_rounds=5`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. Its artifact has SHA-256
  `d2e6743dedf5d1099e383cfc41fe00cfb2e22a05068289084c2db75641cbf7e3`, and
  the schema-2 workflow summary records the repository, source revision, ref,
  runner, workflow, run ID and attempt. It is not production-duration,
  capacity, identity/ACL/TLS, backup/recovery, macOS or release-owner evidence.
  A subsequent hosted attempt
  [35632139449](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632139449)
  (job
  [106440649294](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632139449/job/106440649294))
  tested `3109a2e` but stopped in the default N-API build before provider
  execution because `FuseSession::interrupt_target` still called the expanded
  body validator without its protocol-context argument (`E0061`). It is a
  compile/reproducibility blocker, not runtime evidence; the correction was
  already published upstream as `f16eec2`.
  The corrected latest non-cancelling hosted run
  [35632935680](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632935680)
  (job
  [106443276982](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632935680/job/106443276982))
  tested current-main revision `0bb628b` on `ubuntu-24.04` and completed green
  in 11m41s. Its retained artifact
  `foundationdb-production-qualification-35632935680-1` reported
  `qualification-pass`, `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  five soak rounds, `FOUNDATIONDB_LATENCY_PASS workload=composition
  operations=15 p50_us=9577 p95_us=41547 p99_us=41547 total_ms=168
  throughput_ops_per_sec=89.02`, `FOUNDATIONDB_TEST_PASS topology=durable
  ... platform=linux/amd64 service_restart=pass soak_rounds=5`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. The schema-2 provenance
  summary records source revision
  `0bb628b0323901a1632b05dd8321c32fcabca7d8`, run `35632935680`, attempt `1`
  and runner `GitHub Actions 1000020575`; the artifact SHA-256 is
  `f22252db5e4efcbb3cb4d8f5d0bcf955bd96981c71ed314cdb3aa555add222f4`.
  This is terminal hosted Linux qualification for the tested revision only;
  production identity/ACL/TLS, backup/recovery, capacity, observability,
  macOS and release-owner gates remain open.
  The latest non-cancelling hosted run
  [35634895382](https://github.com/andymacclenaghan/mount-rs/actions/runs/35634895382)
  (job
  [106449795704](https://github.com/andymacclenaghan/mount-rs/actions/runs/35634895382/job/106449795704))
  tested current-main revision `0e0454da` on `ubuntu-24.04` and completed
  green in 10m10s. Its retained artifact
  `foundationdb-production-qualification-35634895382-1` reported
  `qualification-pass`, `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  five soak rounds, `FOUNDATIONDB_LATENCY_PASS workload=composition
  operations=15 p50_us=9462 p95_us=398345 p99_us=398345 total_ms=803
  throughput_ops_per_sec=18.68`, `FOUNDATIONDB_TEST_PASS topology=durable
  ... platform=linux/amd64 service_restart=pass soak_rounds=5`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. The schema-2 provenance
  summary records source revision
  `0e0454da7973c08a56c8634435909da037d21fe7`, run `35634895382`, attempt `1`
  and runner `GitHub Actions 1000020805`; the artifact SHA-256 is
  `c542e9538bc29708fa187ecf78281075060f981a3b06b4d30e686c79a6a33bf7`.
  The p95/p99 outlier is retained as qualification telemetry, not production
  capacity evidence; production identity/ACL/TLS, backup/recovery, capacity,
  observability, macOS and release-owner gates remain open.
  The previous post-FUSE-abort-fix hosted run
  [35636591071](https://github.com/andymac4182/mount-rs/actions/runs/35636591071)
  (job
  [106455406713](https://github.com/andymac4182/mount-rs/actions/runs/35636591071/job/106455406713))
  tested current-main revision `97b63aed` on `ubuntu-24.04` and completed
  green in 11m46s. Its retained artifact
  `foundationdb-production-qualification-35636591071-1` reported
  `qualification-pass`, `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  five soak rounds, `FOUNDATIONDB_LATENCY_PASS workload=composition
  operations=15 p50_us=9640 p95_us=27896 p99_us=27896 total_ms=157
  throughput_ops_per_sec=95.52`, `FOUNDATIONDB_TEST_PASS topology=durable
  ... platform=linux/amd64 service_restart=pass soak_rounds=5`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. The schema-2 provenance
  summary records source revision
  `97b63aed2236696a8d397054b3c04c824e89c229`, run `35636591071`, attempt `1`
  and runner `GitHub Actions 1000020918`; the artifact SHA-256 is
  `123c2c5ece757fda141342d2b8e415e34269fff4246f5c3a8c147902415c137d`.
  This is terminal hosted Linux qualification for the tested revision only;
  production identity/ACL/TLS, backup/recovery, capacity, observability,
  macOS and release-owner gates remain open.
  The previous exact-current-main hosted run
  [35638932960](https://github.com/andymac4182/mount-rs/actions/runs/35638932960)
  (job
  [106463213777](https://github.com/andymac4182/mount-rs/actions/runs/35638932960/job/106463213777))
  tested current-main revision `3817efc9` on `ubuntu-24.04` and completed
  green in 11m33s after the FUSE sync-barrier and blocked-read changes. Its
  retained artifact `foundationdb-production-qualification-35638932960-1`
  reported `qualification-pass`, `FOUNDATIONDB_CLI_PASS
  mode=foundationdb-rustfs-fuse`, five soak rounds,
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9751
  p95_us=28824 p99_us=28824 total_ms=151 throughput_ops_per_sec=99.31`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The schema-2 provenance summary records source
  revision `3817efc9999cc9d77e2c597873193d56206adb08`, run `35638932960`,
  attempt `1` and runner `GitHub Actions 1000021113`; the artifact SHA-256 is
  `c474b5ef9275ef88daf73e7fe90e36bd25eba8d149ac6849edfc672217ef4bd0`.
  This is terminal hosted Linux qualification for the tested revision only;
  production identity/ACL/TLS, backup/recovery, capacity, observability,
  macOS and release-owner gates remain open.
  The previous mainline hosted run
  [35641662353](https://github.com/andymacclenaghan/mount-rs/actions/runs/35641662353)
  (job
  [106472188828](https://github.com/andymacclenaghan/mount-rs/actions/runs/35641662353/job/106472188828))
  tested revision `31e122bc` on `ubuntu-24.04` and completed green in 11m37s.
  Its retained artifact `foundationdb-production-qualification-35641662353-1`
  reported `qualification-pass`, `FOUNDATIONDB_CLI_PASS
  mode=foundationdb-rustfs-fuse`, five soak rounds,
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9861
  p95_us=29367 p99_us=29367 total_ms=163 throughput_ops_per_sec=91.81`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The rollout-ledger guard also emitted
  `W07_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO w07_7=open nested_gates=7`.
  The schema-2 provenance summary records source revision
  `31e122bc2e5790bb3568c01aaea4b236d89dce96`, run `35641662353`, attempt `1`
  and runner `GitHub Actions 1000021349`; the artifact SHA-256 is
  `5f744788bd82fd52fd7e59f1201885dc79f80af2b0558eef917187abce222b50`.
  This is terminal hosted Linux qualification for the tested revision only;
  production identity/ACL/TLS, backup/recovery, capacity, observability,
  macOS and release-owner gates remain open.
  The previous mainline hosted run
  [35645459649](https://github.com/andymac4182/mount-rs/actions/runs/35645459649)
  (job
  [106484724044](https://github.com/andymac4182/mount-rs/actions/runs/35645459649/job/106484724044))
  tested revision `b3a0a92` on `ubuntu-24.04` and completed green in 11m28s.
  Its retained artifact `foundationdb-production-qualification-35645459649-1`
  reported `qualification-pass`, `FOUNDATIONDB_CLI_PASS
  mode=foundationdb-rustfs-fuse`, five soak rounds,
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9350
  p95_us=49303 p99_us=49303 total_ms=182 throughput_ops_per_sec=82.07`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The rollout-ledger guard emitted
  `W07_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO w07_7=open nested_gates=7`
  and `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`. The schema-2 provenance summary
  records source revision
  `b3a0a92d617e66ad58460f345c428e197f8c9e2d`, run `35645459649`, attempt `1`
  and runner `GitHub Actions 1000021544`; the artifact SHA-256 is
  `0bab89cbff4d36b68351d84978ad56991e2ac16621084dd987f8717d4b07e63e`.
  This is terminal hosted Linux qualification for the tested revision only;
  production identity/ACL/TLS, backup/recovery, capacity, observability,
  macOS and release-owner gates remain open.
  The previous mainline hosted run
  [35648296386](https://github.com/andymacclenaghan/mount-rs/actions/runs/35648296386)
  (job
  [106494088981](https://github.com/andymacclenaghan/mount-rs/actions/runs/35648296386/job/106494088981))
  tested revision `20fb445a` on `ubuntu-24.04` and completed green in 11m16s.
  Its retained artifact `foundationdb-production-qualification-35648296386-1`
  reported `qualification-pass`, `FOUNDATIONDB_CLI_PASS
  mode=foundationdb-rustfs-fuse`, five soak rounds,
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7654
  p95_us=36558 p99_us=36558 total_ms=146 throughput_ops_per_sec=102.61`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The rollout-ledger guard emitted
  `W07_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO w07_7=open nested_gates=7`
  and `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`. The schema-2 provenance summary
  records source revision
  `20fb445a02b16e3dcb62525fcbd84ff14d7c8dea`, run `35648296386`, attempt `1`
  and runner `GitHub Actions 1000021765`; the artifact SHA-256 is
  `8c93fc161217e7c032be17c6892791bb764a081ddad678ad601a0d6775a5962b`.
  This is terminal hosted Linux qualification for the tested revision only;
  production identity/ACL/TLS, backup/recovery, capacity, observability,
  macOS and release-owner gates remain open.
  The previous latest mainline hosted run
  [35650634174](https://github.com/andymac4182/mount-rs/actions/runs/35650634174)
  (job
  [106501907695](https://github.com/andymac4182/mount-rs/actions/runs/35650634174/job/106501907695))
  tested revision `8f3a19a` on `ubuntu-24.04` and completed green in 10m19s.
  Its retained artifact `foundationdb-production-qualification-35650634174-1`
  reported `qualification-pass`, `FOUNDATIONDB_CLI_PASS
  mode=foundationdb-rustfs-fuse`, five soak rounds, base
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=12440
  p95_us=117600 p99_us=117600 total_ms=385 throughput_ops_per_sec=38.88`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The five soak-round p95/p99 values ranged from
  31,686µs to 346,494µs and throughput ranged from 23.53 to 97.18 ops/s.
  The rollout-ledger guard emitted
  `W07_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO w07_7=open nested_gates=7`
  and `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`. The schema-2 provenance summary
  records source revision
  `8f3a19a891b8d432ff551c04789921575bb12f4f`, run `35650634174`, attempt `1`
  and runner `GitHub Actions 1000021966`; the artifact SHA-256 is
  `6a99d3778504fa2ab123c7256d168b4b52a424c2aea594f7dc03ce7d076a16f8`.
  This is terminal hosted Linux qualification for the tested revision only;
  the latency variability is not production capacity evidence, and production
  identity/ACL/TLS, backup/recovery, capacity, observability, macOS and
  release-owner gates remain open.
  The latest mainline hosted run
  [35652638242](https://github.com/andymac4182/mount-rs/actions/runs/35652638242)
  (job
  [106508480404](https://github.com/andymac4182/mount-rs/actions/runs/35652638242/job/106508480404))
  tested revision `c6f0039` on `ubuntu-24.04` and completed green in 11m58s.
  Its retained artifact `foundationdb-production-qualification-35652638242-1`
  reported `qualification-pass`, `FOUNDATIONDB_CLI_PASS
  mode=foundationdb-rustfs-fuse`, five soak rounds, base
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7712
  p95_us=228723 p99_us=228723 total_ms=353 throughput_ops_per_sec=42.41`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The five soak-round p95/p99 values ranged from
  27,598µs to 35,139µs and throughput ranged from 77.45 to 95.05 ops/s.
  The positive production-config fixture emitted
  `lease_ttl=explicit-bounded`; the inline-secret and unsafe-TTL fixtures
  failed closed. The rollout-ledger guard emitted
  `W07_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO w07_7=open nested_gates=7`
  and `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`. The schema-2 provenance summary
  records source revision
  `c6f0039471ebdd441a21648a1fb923abc38c9a6b`, run `35652638242`, attempt `1`
  and runner `GitHub Actions 1000022176`; the artifact SHA-256 is
  `366cb78d19bc5182a438a8459ebfe54efc510232e9dab667a815aba5eacfbee7`.
  This is terminal hosted Linux qualification for the tested revision only;
  the base latency outlier is qualification telemetry rather than production
  capacity evidence, and production identity/ACL/TLS, backup/recovery,
  capacity, observability, macOS and release-owner gates remain open.
  The latest policy-bearing hosted run
  [35655579378](https://github.com/andymacclenaghan/mount-rs/actions/runs/35655579378)
  (job
  [106518268071](https://github.com/andymacclenaghan/mount-rs/actions/runs/35655579378/job/106518268071))
  tested revision `03541161311983f497983ef0de1bcb09b958b2e4` on
  `ubuntu-24.04` and completed green in 11m06s. Its retained artifact
  `foundationdb-production-qualification-35655579378-1` reported
  `qualification-pass`, `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  five soak rounds, base
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7227
  p95_us=81981 p99_us=81981 total_ms=191 throughput_ops_per_sec=78.17`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The five soak-round p95/p99 values ranged from
  188,168µs to 737,468µs and throughput ranged from 9.40 to 39.58 ops/s.
  The positive production-config fixture emitted `lease_ttl=explicit-bounded`;
  the inline-secret and unsafe-TTL fixtures failed closed. The rollout-ledger
  guard emitted `W07_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO w07_7=open
  nested_gates=7` and `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`. The schema-2
  provenance summary records source revision
  `03541161311983f497983ef0de1bcb09b958b2e4`, run `35655579378`, attempt `1`
  and runner `GitHub Actions 1000022432`; the artifact SHA-256 is
  `dff8fb664a62d70e7736098d842cc9d976e942358c203c75cdb028e9ea42c177`.
  This is terminal hosted Linux qualification for the tested revision only;
  the base and soak latency variability is qualification telemetry rather than
  production capacity evidence, and production identity/ACL/TLS,
  backup/recovery, capacity, observability, macOS and release-owner gates
  remain open.
  The subsequent hosted run
  [35657842924](https://github.com/andymac4182/mount-rs/actions/runs/35657842924)
  (job
  [106525768365](https://github.com/andymac4182/mount-rs/actions/runs/35657842924/job/106525768365))
  tested revision `87a500b13bba305e7a7a5c80c328395d80eb772b` on
  `ubuntu-24.04` and completed green in 11m50s. Its retained artifact
  `foundationdb-production-qualification-35657842924-1` reported
  `qualification-pass`, `FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse`,
  five soak rounds, base
  `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9426
  p95_us=29053 p99_us=29053 total_ms=157 throughput_ops_per_sec=95.22`,
  `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`. The five soak-round p95/p99 values ranged from
  27,345µs to 29,175µs and throughput ranged from 85.77 to 94.49 ops/s.
  The lease-publication policy marker, production-config negative fixtures,
  rollout-ledger guard and six regression cases all passed while preserving
  NO-GO. The schema-2 provenance summary records source revision
  `87a500b13bba305e7a7a5c80c328395d80eb772b`, run `35657842924`, attempt `1`
  and runner `GitHub Actions 1000022644`; the artifact SHA-256 is
  `dc37b10f82ecd1c4c4513f4cf3ecc9a95e2ef37ccdadf0f4d69e56d30620846b`.
  This is terminal hosted Linux qualification for the tested revision only;
  the bounded telemetry is not production capacity evidence, and production
  identity/ACL/TLS, backup/recovery, capacity, observability, macOS and
  release-owner gates remain open.
  The next hosted attempt
  [35662679910](https://github.com/andymac4182/mount-rs/actions/runs/35662679910)
  passed policy/build preconditions but was **NO HOSTED PASS** because
  `tests/foundationdb/Cargo.lock` omitted the `sha2` dependency edge required
  by `mount-rs-r2`; it contributed no provider runtime evidence. After the
  repair in `ccd3f671`, hosted run
  [35663914822](https://github.com/andymac4182/mount-rs/actions/runs/35663914822)
  (job `106545260051`, revision `147c53a8`) completed green in 11m43s. It
  emitted `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_CLI_PASS`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`; base latency was
  `operations=15 p50_us=9564 p95_us=39516 p99_us=39516 total_ms=163
  throughput_ops_per_sec=91.71`, with five soak-round p95/p99 values from
  21,487µs to 24,210µs and throughput from 91.97 to 106.58 ops/s. The
  artifact digest is
  `098ab3a4bb1fd6ae84971cebbb67baf2e50d9cb271accdaa9351ec28e49c3042` and
  provenance records runner `GitHub Actions 1000023111`; this remains
  bounded hosted Linux qualification, not production capacity or rollout
  acceptance.
  Fresh mainline requalification
  [35665547966](https://github.com/andymac4182/mount-rs/actions/runs/35665547966)
  (job `106550374750`, revision `1b98ed04`) completed green in 9m47s. It
  emitted `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_CLI_PASS`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`; base latency was
  `operations=15 p50_us=26199 p95_us=321373 p99_us=321373 total_ms=1273
  throughput_ops_per_sec=11.78`, with five soak-round p95/p99 values from
  16,527µs to 34,188µs and throughput from 53.87 to 136.00 ops/s. The
  artifact digest is
  `f37951fa1e20ce86bdc031d9529c1ace3506b03a9f2027b72b7186118eccbd02` and
  provenance records runner `GitHub Actions 1000023256`; this remains
  bounded hosted Linux qualification, not production capacity or rollout
  acceptance.
  Current-tip requalification
  [35666991514](https://github.com/andymac4182/mount-rs/actions/runs/35666991514)
  (job `106554849953`, revision `5b9af323`) completed green in 11m23s. It
  emitted `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_CLI_PASS`,
  `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`; base latency was
  `operations=15 p50_us=8101 p95_us=35120 p99_us=35120 total_ms=141
  throughput_ops_per_sec=105.73`, with five soak-round p95/p99 values from
  20,100µs to 21,587µs and throughput from 107.93 to 119.84 ops/s. The
  artifact digest is
  `7313f7ce6f7fb188d84d79a5c5d98df01a1319f071208f1058c5ab87a72acb12` and
  provenance records runner `GitHub Actions 1000023347`; this remains
  bounded hosted Linux qualification, not production capacity or rollout
  acceptance.
  Latest packet-guard requalification
  [35668646531](https://github.com/andymac4182/mount-rs/actions/runs/35668646531)
  (job `106559918401`, revision `564d0949`) completed green in 10m27s. It
  emitted `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_CLI_PASS`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`; base latency was
  `operations=15 p50_us=9720 p95_us=51773 p99_us=51773 total_ms=180
  throughput_ops_per_sec=83.15`, with five soak-round p95/p99 values from
  17,332µs to 31,275µs and throughput from 102.88 to 129.93 ops/s. The
  artifact digest is
  `f6f663ff26bb925cf601353a8e2c4e7d48bf15fbaeb7ba20163e32eb3c8a3897` and
  provenance records runner `GitHub Actions 1000023513`; this remains
  bounded hosted Linux qualification, not production capacity or rollout
  acceptance.
  Exact published-tip requalification
  [35669850643](https://github.com/andymac4182/mount-rs/actions/runs/35669850643)
  (job `106563637562`, revision `618ee5fe`) completed green in 11m37s. It
  emitted `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_CLI_PASS`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`; base latency was
  `operations=15 p50_us=3704 p95_us=31810 p99_us=31810 total_ms=88
  throughput_ops_per_sec=169.44`, with five soak-round p95/p99 values from
  13,087µs to 14,017µs and throughput from 203.80 to 215.46 ops/s. The
  artifact digest is
  `0def7863ba535c8b210865cf0cc0c13770f6fde268be4db9e4da6669e6abbd5c` and
  provenance records runner `GitHub Actions 1000023659`; this remains
  bounded hosted Linux qualification, not production capacity or rollout
  acceptance.
  Workflow-artifact retention requalification
  [35671492720](https://github.com/andymac4182/mount-rs/actions/runs/35671492720)
  (job `106568742563`, revision `9460a62`) completed green in 12m21s. It
  emitted `FOUNDATIONDB_TEST_PASS topology=durable ... platform=linux/amd64
  service_restart=pass soak_rounds=5`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
  `FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_CLI_PASS`, `RUSTFS_COMBO_PASS` and
  `RUSTFS_INTEGRATION_PASS`; base latency was
  `operations=15 p50_us=3846 p95_us=38409 p99_us=38409 total_ms=96
  throughput_ops_per_sec=155.90`, with five soak-round p95/p99 values from
  13,005µs to 14,243µs and throughput from 199.39 to 214.47 ops/s. The
  artifact digest is
  `815dfecadab366a9fca9a7501c4a36d771324d8f71b521e0e8082261cbe431c3` and
  provenance records runner `GitHub Actions 1000023867`; the downloaded
  artifact contains the runtime log, summary and seven-gate production packet.
  This remains bounded hosted Linux qualification, not production capacity or
  rollout acceptance.
- [x] W07.6a The bounded mixed-provider packet also verifies exact owned-prefix
  cleanup: every tracked block is absent after cleanup while sibling and parent
  sentinel objects remain untouched. The earlier target-gated packet did not
  itself close the W07.6 service-restart or hosted-composition boundaries; the
  terminal hosted evidence above now does. The
  published `629c2f6` packet adds an owned FoundationDB restart/readiness gate,
  fresh-client RustFS reopen/CAS/fencing checks and fail-closed external-FDB
  handling; its original local runtime lane was blocked by host `libfdb_c` and
  Docker, while the dedicated hosted lane is now green.
- [ ] W07.7 **Production rollout readiness and go/no-go:** the demo and local
  Docker evidence are not production acceptance. Before enabling any production
  consumer, close every gate below with a linked revision, test/run result,
  environment identity and accountable owner:
  The credential-free
  `scripts/verify-w07-production-config.mjs` gate and its positive/negative
  fixtures now enforce the accepted production configuration shape in hosted
  qualification: durable FoundationDB metadata, `shared-provider` authority,
  an explicit positive lease TTL bounded to 24 hours, HTTPS RustFS blocks and
  external credential references. The negative fixtures independently reject
  inline block credentials and an unsafe lease TTL. This is static policy
  evidence only; it cannot prove the actual cluster, ACLs, TLS handshake,
  replication, recovery, capacity, telemetry or release approval.
  The hosted workflow also runs `scripts/verify-w07-rollout-ledger.mjs`, which
  fails closed if a **NO-GO** ledger loses its open W07.7 checkbox, nested
  production gates or external-drill boundary. This is an internal tracking
  invariant, not production acceptance.
  `scripts/test-w07-rollout-ledger.mjs` runs eight regression cases for the
  current NO-GO, premature-GO, missing-gate, missing-drill, incomplete
  P0–P14 ledger and synthetic complete-GO states; these cases validate the
  tracking control only. The verifier also requires exactly one row for every
  P0–P14 gate, rejects a terminal gate status while the decision is **NO-GO**,
  and requires every gate to be terminally accepted before a **GO** decision.
  The hosted workflow runs `scripts/test-w07-qualification-log.mjs` with
  eight credential-free verifier cases. The qualification log verifier
  requires the exact accepted configuration shape, both expected negative
  fixtures, a parsed lease-publication policy whose cadence is shorter than
  the TTL, whose forward-jump bound is no larger than the TTL and whose TTL is
  at most 24 hours, and at least one valid
  `FOUNDATIONDB_LATENCY_PASS` sample for the base composition plus every
  declared soak round. It retains all parsed latency samples in the schema-2
  summary, so a standalone `FOUNDATIONDB_SOAK_PASS` marker cannot hide a
  missing or malformed round measurement. These checks harden evidence
  integrity only; they do not close any production identity, failover,
  recovery, capacity, observability, native-platform or release-owner gate.
  The dedicated hosted lane now also requests a fixed bounded workload profile
  through the live FoundationDB/RustFS Node consumer (400 iterations, 64-way
  concurrency and 4 KiB payloads), independently validates and retains
  `foundationdb-ozone-iops.json`, and requires the corresponding
  `FOUNDATIONDB_W07_WORKLOAD_PASS` marker. The profile requires complete
  lifecycles and records measured throughput; its non-zero floor is structural,
  not a capacity target. This improves repeatable qualification evidence only;
  hosted attempt `35673343510` at revision `9ea7e493` correctly produced no
  pass because the first implementation imposed an unsupported 1,000-IOPS
  floor and measured 29.13 IOPS. Terminal run
  [35675987457](https://github.com/andymac4182/mount-rs/actions/runs/35675987457)
  at exact revision `1670ceba81b24c1ed39b8ab396671324c7f28193` now passes the
  corrected profile: 400 complete lifecycles at 64-way concurrency and 4 KiB,
  measured 64.73 lifecycle IOPS, all 1,200 operations successful, zero
  timeouts/cleanup failures, and a retained machine-readable artifact. This
  is bounded hosted Linux qualification only; production-like duration/load,
  retry/error-budget, resource-growth, capacity, cost and scaling acceptance
  remain open.
  The hosted workflow also validates the machine-readable
  `docs/W07-production-evidence.json` packet with
  `scripts/verify-w07-production-evidence.mjs` and runs sixteen credential-free
  packet cases through `scripts/test-w07-production-evidence.mjs`. The packet
  has one row for each W07.7 production gate and requires explicit remaining
  actions while **NO-GO**; any future **GO** packet must carry a concrete
  source revision, unique record ID, accountable owner, target environment,
  terminal run, provider versions, configuration and authority references,
  people, ordered ISO-8601 lifecycle timestamps, measured result,
  cleanup/rollback outcome and evidence reference for every closed gate. The
  workflow retains this packet beside the qualification log and summary in the
  run artifact, so the seven-gate NO-GO state travels with each bounded
  qualification result. This is admission/tracking integrity only and cannot
  authenticate production evidence or release approval.
  - [ ] **Identity and least privilege:** document and deploy one
    write-capable authority identity per authority prefix, read-only consumer
    identities, secret injection/rotation and no shared credentials. Prove
    with the actual production credential/tenant/ACL policy that a consumer
    cannot publish or overwrite the authority record.
  - [ ] **Time and authority failure policy:** enforce the authority-host
    clock-skew bound and alerting; publish more frequently than the shortest
    lease TTL; republish after authority restart before admitting readers; fail
    closed during authority loss; and test the reviewed failover and recovery
    procedure.
  - [ ] **FoundationDB durability and recovery:** prove the production
    cluster's replication/storage policy, backup/restore, service
    restart/failover, keyspace/version compatibility and a recovery drill.
    The single-node pinned Docker harness is not this evidence.
  - [ ] **Production-like workload and soak:** run multi-chunk reads/writes,
    partial writes, truncate/extend, concurrent publication, stale-writer
    fencing, maybe-committed reconciliation, lease renewal/expiry, reconnect
    and fresh-client reopen at production-like duration and load. Record
    latency, retry, capacity and error-budget results. The real composition
    harness now emits `FOUNDATIONDB_LATENCY_PASS` with p50/p95/p99 operation
    latency and throughput; prior terminal hosted run `35675987457` recorded base
    `operations=15 p50_us=3394 p95_us=43146 p99_us=43146 total_ms=94
    throughput_ops_per_sec=158.97` at exact revision
    `1670ceba81b24c1ed39b8ab396671324c7f28193`; its five isolated soak-round
    p95/p99 values ranged from 12,701µs to 14,187µs and throughput ranged
    from 218.16 to 225.92 ops/s. The same terminal run passed the fixed
    400-iteration, 64-concurrency, 4 KiB profile with measured 64.73 lifecycle
    IOPS, 400 successful writes/reads/deletes, zero timeouts and zero cleanup
    failures; artifact `foundationdb-production-qualification-35675987457-1`
    (ID `10673640911`, SHA-256
    `0cb38b97b121ef8b6b69a1c4ef76d112bb24ba716b19eb6153ee52b13a3cc694`) was
    retained and independently validated. The profile's non-zero floor is
    structural, not a capacity target. The prior `35674506961` recorded base
    `operations=15 p50_us=2617 p95_us=88362 p99_us=88362 total_ms=134
    throughput_ops_per_sec=111.17`; soak p95/p99 ranged from 13,388µs to
    15,041µs and throughput from 206.94 to 236.23 ops/s. Production-shaped
    duration, retry/error budget, resource growth, safe capacity, cost and
    scaling triggers are still required. The newer heartbeat-corrected
    current-tip run `35680085315` at exact revision
    `71972b28ca7ae561325342ddf466c8353546547a` passed the same bounded
    workload with 202.31 measured lifecycle IOPS, 1,200 successful operations,
    zero timeouts/cleanup failures, a 30-second authority heartbeat and
    service-restart liveness checks. This is a stronger implementation
    qualification checkpoint, not production capacity, monitoring or failover
    evidence.
    The telemetry-qualified current-tip run `35681642584` at exact revision
    `113b13751acc4885ca5916a7f70fe109e1dbadd4` passed the same bounded
    workload with 441.76 measured lifecycle IOPS, 1,200 successful operations,
    zero timeouts/cleanup failures, the 30-second authority heartbeat and the
    new shared-authority stats accounting marker. Base composition p95/p99 was
    321,472µs at 30.29 ops/s; five-round soak p95/p99 ranged from 10,229µs to
    10,887µs with throughput from 257.61 to 295.85 ops/s. This is a stronger
    implementation qualification checkpoint, not production capacity,
    monitoring or failover evidence; production-shaped duration, retry/error
    budget, resource growth, safe capacity, cost and scaling triggers remain
    required.

    The telemetry-verifier current-tip run
    [35683503910](https://github.com/andymacclenaghan/mount-rs/actions/runs/35683503910)
    at exact revision `0e0327535fbcda0b1544400c00e938911a8ed6ad` (job
    `106606452492`) completed green on Ubuntu 24.04/Linux `amd64` in 12m44s.
    Its stricter seven-case qualification-log verifier required the bounded
    30-second/120-second authority heartbeat and reconciled stats marker.
    The base composition run recorded p50 3,281µs, p95/p99 29,220µs and
    185.99 ops/s; five-round soak p95/p99 ranged from 13,491µs to 22,635µs
    with throughput from 176.18 to 210.55 ops/s. The retained workload
    artifact passed 400 writes, reads and deletes, 1,200 successful lifecycle
    operations, 368.57 measured IOPS, zero timeouts and zero cleanup failures.
    The packet remains **NO-GO** with all seven production gates open; this is
    stronger hosted implementation qualification, not production capacity,
    collector, failover, recovery or owner evidence.

    Hosted attempt
    [35685846631](https://github.com/andymacclenaghan/mount-rs/actions/runs/35685846631)
    (job `106612350173`, exact revision
    `e9e2d30c6be06a5aa1f0e81e39fb5a0450c77a78`) did not produce a qualification
    pass: all policy, configuration, prerequisite, N-API and Linux FUSE steps
    passed, but the durable step stopped before provider execution because the
    standalone `tests/foundationdb/Cargo.lock` lacked the new `tracing` feature
    dependency and `--locked` refused to update it. This is a reproducibility
    failure, not FoundationDB/RustFS runtime evidence; the missing lockfile edge
    is corrected in the next mainline chunk and requires a fresh exact-tip run.

    Repaired hosted requalification
    [35686583792](https://github.com/andymacclenaghan/mount-rs/actions/runs/35686583792)
    (job `106614608451`, exact revision
    `7ba3998a6f6a88b7eedefd98cba9262980d24ce9`) completed green in 12m35s on
    Ubuntu 24.04/Linux `amd64`. The standalone lockfile correction allowed the
    durable step to run: FoundationDB/RustFS metadata and chunks, service
    restart, authority republish, fresh-client reopen, Node/N-API, Linux
    CLI/FUSE, five-round soak and RustFS integration all passed. The run also
    passed the stricter seven-case qualification-log verifier, including the
    30-second/120-second authority heartbeat and reconciled stats marker, and
    the four-case workload artifact verifier. Base composition recorded p50
    3,133µs, p95/p99 32,028µs and 187.57 ops/s; soak p95/p99 ranged from
    12,020µs to 12,490µs with throughput from 227.19 to 235.58 ops/s. The
    retained workload artifact records 1,200 successful lifecycle operations,
    166.29 measured IOPS, zero timeouts and zero cleanup failures. Artifact
    `foundationdb-production-qualification-35686583792-1` (ID `10676804103`,
    SHA-256
    `afccc878e46a096c6153f04fac017f3df19d37d147a2e83629fa14616f850e30`)
    remains bounded implementation qualification; the packet remains
    **NO-GO** with seven open gates and zero evidence records.

    The evidence-integrity follow-up at exact source
    `419fbf217b5c40e0e62371e9b41badc07582e3d5` now runs eight qualification-log
    regression cases and requires one valid latency sample for the base
    composition plus every declared soak round; all samples are retained in
    the schema-2 summary. The retained `35686583792` log independently passes
    this stricter verifier because it contains the base sample plus five soak
    samples. The follow-up provenance-binding chunk at exact source
    `07e455cd48b2962e2d06290bad35379b575d521e` adds a raw-log
    `W07_QUALIFICATION_PROVENANCE` marker and three regression cases for valid,
    missing and mismatched run context; the local suite now passes 11 cases.
    These controls harden evidence integrity only and do not change the
    production **NO-GO** boundary.

    Hosted attempt
    [35690122405](https://github.com/andymacclenaghan/mount-rs/actions/runs/35690122405)
    (job `106625124766`, exact revision
    `07e455cd48b2962e2d06290bad35379b575d521e`) reached the durable and
    workload passes but failed final validation with
    `missing=provenance-marker-missing` because the config-policy `tee`
    overwrote the raw-log marker. It produced no qualification pass; the
    append-only repair is published in `6b8639aa` and required a fresh run.

    The corrected current-tip execution is hosted run
    [35691106828](https://github.com/andymacclenaghan/mount-rs/actions/runs/35691106828)
    (job `106628088824`, exact revision
    `6b8639aa618f301e8424b5619bc1dda559a75948`). It completed green in 12m10s
    on Ubuntu 24.04/Linux `amd64`; the current 11-case verifier passed, the
    raw-log provenance marker matched the exact run context, and the retained
    schema-2 summary contains the base plus five soak latency samples. The
    base marker was p50 2,680µs, p95/p99 13,759µs and 254.79 ops/s; soak
    p95/p99 ranged from 12,701µs to 13,784µs with throughput from 241.60 to
    248.00 ops/s. The bounded 400-iteration, 64-concurrency, 4 KiB workload
    recorded 1,200 successful lifecycle operations, 259.35 measured IOPS,
    zero timeouts and zero cleanup failures. Artifact
    `foundationdb-production-qualification-35691106828-1` (ID `10678891553`,
    SHA-256
    `f26ae7cb81dd298cc78153f27e573bdb1096a6ae038cc8f42a40d7cbeeab3c17`)
    was independently downloaded and revalidated with the exact provenance
    environment. This is a current-tip hosted implementation qualification
    pass, not production evidence; the seven-gate packet remains **NO-GO**
    with zero production evidence records.

    The extended-soak workflow chunk is published at exact source
    `5a6d6507c6deac160f54a246a2d715c05fc35268` and hosted run
    [35692674674](https://github.com/andymacclenaghan/mount-rs/actions/runs/35692674674)
    (job `106632794791`) completed green in 11m26s on Ubuntu 24.04/Linux
    `amd64`. It increased the isolated real-provider soak from five to ten
    rounds while retaining the same multi-chunk, partial-write, truncate/
    extend, CAS/fencing, lease-expiry, fresh-client reopen, service-restart
    and scoped cleanup checks per round. The raw provenance marker matched
    the exact repository, workflow, ref, source revision, run, attempt and
    runner `GitHub Actions 1000026766`; the eleven-sample schema-2 summary
    recorded base p95/p99 439,603µs at 21.41 ops/s and ten-round soak p95/p99
    70,204–236,130µs at 18.19–96.22 ops/s. The bounded workload recorded
    1,200 successful lifecycle operations at 110.18 IOPS with zero timeouts
    and cleanup failures. Artifact
    `foundationdb-production-qualification-35692674674-1` (ID `10679154003`,
    SHA-256
    `44beef451832c307a53571bb86f92293b70c302d0add5104152059ed4c41d2ec`)
    was independently downloaded and revalidated. This is extended hosted
    implementation qualification only; the seven-gate packet remains
    **NO-GO** with zero production evidence records.

    The terminal cross-platform qualification is hosted run
    [35698630720](https://github.com/andymacclenaghan/mount-rs/actions/runs/35698630720)
    at exact revision `a19d37b61fdbb769102df15714cc88a3ff5eea91`, with Linux
    job `106651101783`, macOS job `106651102065` and aggregate job
    `106654577928` all green. The Linux packet retained the exact provenance,
    heartbeat/stats, durable FoundationDB/RustFS, Node/N-API, CLI/FUSE,
    restart, fresh-client, RustFS and ten-round soak results; base composition
    was p50 2,469µs, p95/p99 25,063µs at 208.39 ops/s, soak p95/p99 was
    13,399–13,906µs at 225.29–258.64 ops/s, and the bounded workload recorded
    1,200 successful lifecycle operations at 467.15 IOPS with zero timeouts or
    cleanup failures. The macOS packet emitted
    `W07_MACOS_FOUNDATIONDB_COMPILE_PASS`; the aggregate downloaded both
    platform artifacts and emitted `W07_PLATFORM_QUALIFICATION_PASS`.
    Artifacts were independently revalidated with exact provenance and
    platform validators. This closes the terminal qualification packet and
    feature-compile control only; live macOS service/cluster/mount,
    clean-install, signing/package, production capacity, identity/ACL,
    backup/restore, failover, observability and owner evidence remain open.

  - [ ] **Observability and operations:** expose and alert on cluster health,
    authority publication age/errors, reader failures, lease-fence/ESTALE,
    transaction retries/maybe-committed EIO and cleanup/space pressure.
    The FoundationDB authority and shared-reader handles now expose bounded
    process-local `stats()` snapshots for publication/read attempts,
    successes/failures, last provider-time observations and local diagnostic
    timestamps, and each publication/read attempt emits a bounded structured
    `tracing` event with fixed event names and no cluster paths, prefixes,
    credentials or provider error text; the hosted shared-authority path
    asserts the success/failure accounting and emits
    `FOUNDATIONDB_AUTHORITY_STATS_PASS`; the
    qualification-log verifier now fails closed unless the bounded heartbeat
    and stats markers are present and their counters/policy values reconcile.
    Map these snapshots into the approved collector and pager, publish dashboards,
    escalation thresholds and incident/recovery ownership, then execute the
    alert drills. The API is an implementation input only: counters reset with
    a new handle and no production collector, alert route or owner evidence is
    claimed yet.
  - [ ] **Rollout and rollback:** stage a canary with a holdback, define
    go/no-go and abort criteria, verify backward/forward compatibility of the
    keyspace and configuration, rehearse rollback/authority recovery and
    record owner sign-off.
  - [ ] **Hosted and platform evidence:** terminal hosted FoundationDB/RustFS,
    Node, CLI/native Linux plus macOS feature-compile evidence is green for
    exact revision `8520e362710a4b3fe00fd567cf00fcc13e64c222` in run
    `35700746198`, with Linux job `106657901740`, macOS job `106657901971` and
    aggregate job `106660728286` on Ubuntu 24.04/Linux `amd64` plus
    `macos-latest`. Linux retained the 30-second/120-second heartbeat marker,
    reconciled `FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3
    publication_successes=3 publication_failures=0 reader_attempts=5
    reader_successes=4 reader_failures=1 last_published_time_ms=2030001
    last_observed_time_ms=2030001`, exact run-bound provenance and the durable,
    Node/N-API, CLI/FUSE, restart, fresh-client, RustFS and ten-round soak
    paths. Base composition was p95/p99 24,689µs at 242.82 ops/s; ten-round
    soak was p95/p99 9,727–266,600µs at 47.11–346.34 ops/s; and the bounded
    workload recorded 1,200 successful lifecycle operations at 323.68 IOPS
    with zero timeouts or cleanup failures. The macOS marker was
    `W07_MACOS_FOUNDATIONDB_COMPILE_PASS`; the aggregate downloaded both
    platform artifacts and emitted `W07_PLATFORM_QUALIFICATION_PASS`.
    Retained artifacts are Linux
    `foundationdb-production-qualification-35700746198-1` (ID `10682882571`,
    SHA-256
    `88f5e05ceb926917e251cfb5d8a949ec2ec6448233e94b9496cbd7d07aafe807`),
    macOS ID `10682322954` (SHA-256
    `76ed42b3cde607d702b93b0c5e1a1a75c0bf71da3ec7db14b18fd1d3b145bd36`) and
    aggregate ID `10681913394` (SHA-256
    `92712f0e09745a9a97345be3a55e9f86b8084b1b403415f62c8ec291ec26c82d`).
    Independent exact-provenance, workload, platform, production-packet and
    rollout-ledger validators passed. This is a fail-closed qualification
    control, not production platform, signing, package or live macOS-cluster
    evidence; live macOS service/cluster/mount, clean-install/package,
    production capacity, identity/ACL, backup/restore, failover,
    observability and owner evidence remain open. Failed, skipped, cancelled
    or unavailable production evidence remains open.

    The exact-tip terminal cross-platform qualification is hosted run
    [35700746198](https://github.com/andymacclenaghan/mount-rs/actions/runs/35700746198)
    at revision `8520e362710a4b3fe00fd567cf00fcc13e64c222`, with Linux job
    `106657901740`, macOS job `106657901971` and aggregate job `106660728286`
    all green. Linux matched the exact repository, workflow, ref, SHA, run,
    attempt and runner `GitHub Actions 1000027426`; base composition was p50
    1,950µs, p95/p99 24,689µs at 242.82 ops/s, ten-round soak p95/p99 was
    9,727–266,600µs at 47.11–346.34 ops/s, and the bounded workload recorded
    1,200 successful lifecycle operations at 323.68 IOPS with zero timeouts or
    cleanup failures. The macOS marker and aggregate platform marker passed,
    and independent provenance, workload, platform, packet and ledger
    validators passed. This closes exact-tip hosted qualification only; live
    macOS service/cluster/mount, clean-install, signing/package, production
    capacity, identity/ACL, backup/restore, failover, observability and owner
    evidence remain open. The seven-gate packet remains **NO-GO** with zero
    production evidence records.

    The repaired exact-tip terminal cross-platform qualification is hosted run
    [35705886860](https://github.com/andymacclenaghan/mount-rs/actions/runs/35705886860)
    at revision `ab74870c58ab768c65679ea80feb18a8f54cbe00`, with Linux job
    `106674581511`, macOS job `106674582039` and aggregate job `106678244742`
    all green. Linux matched the exact repository, workflow, ref, SHA, run,
    attempt and runner `GitHub Actions 1000027673`; macOS matched the shared
    fields with runner `GitHub Actions 1000027679`. Base composition was p50
    2,848µs, p95/p99 13,430µs at 260.46 ops/s; ten-round soak p95/p99 was
    11,732–13,359µs at 241.11–268.03 ops/s; and the bounded workload recorded
    1,200 successful lifecycle operations at 228.83 IOPS with zero timeouts or
    cleanup failures. The macOS compile and bound-provenance markers passed,
    the aggregate emitted `W07_PLATFORM_QUALIFICATION_PASS provenance=bound`,
    and independent provenance, workload, platform, packet and ledger
    validators passed. This closes exact-tip hosted qualification only; live
    macOS service/cluster/mount, clean-install, signing/package, production
    capacity, identity/ACL, backup/restore, failover, observability and owner
    evidence remain open. The seven-gate packet remains **NO-GO** with zero
    production evidence records.

    The latest W07.7 qualification checkpoint is hosted run
    [35712676265](https://github.com/andymacclenaghan/mount-rs/actions/runs/35712676265)
    at exact revision `ff5cc58188d9783a80de96698a187e1f6416b83e`, with Linux
    job `106696766067`, macOS job `106696766203` and aggregate job
    `106702286990` all terminally green. The independent provenance,
    workload, summary-replay, platform, production-packet and rollout-ledger
    validators passed, but the retained packet still has seven open
    production gates and zero evidence records. This is current-public-main
    cross-platform qualification, not a production PASS; W07.3, W07.5 and
    W07.7 remain open pending live credentials, platform/deployment,
    recovery, observability, capacity and named-owner evidence.

  The previous current-main source gate on 2026-09-22 tested revision `3cd4377` and
  passed `./scripts/cargo-shared fmt --all -- --check`, strict workspace
  Clippy with `-D warnings`, and the locked
  `./scripts/cargo-shared test --workspace --all-targets --locked` suite,
  including the current FUSE sync-barrier/session and NFS coverage. The
  suite's provider, native-mount and external-service rows remained explicitly
  ignored where their required harnesses were not present. This is current
  source qualification only; it does not close the hosted platform matrix or
  any production deployment gate. Earlier source checkpoints at `9d3a6e5`,
  `29365e9` and `717a0ab` remain historical evidence in the rollout ledger.

  The previous shared-tip source gate on 2026-09-22 tested revision
  `2641962a6a65179abf4b8d785345fbe6af4be9b8` and passed formatting, strict
  locked workspace Clippy and the locked all-target workspace test suite.
  Current FUSE sync-barrier/session, NFS, transport, SDK, CLI and provider unit
  coverage passed; provider, native-mount and external-service rows remained
  explicitly ignored where their required harnesses were unavailable. This is
  source qualification only, not hosted or production acceptance.

  The latest published-tip source gate on 2026-09-22 tested revision
  `4ea3268306d71fe96c537cd0f3c4d4393f173a60` and passed formatting, strict
  locked workspace Clippy and the locked all-target workspace test suite.
  Current 9P/FUSE, NFS, transport, SDK, CLI and provider unit coverage passed;
  provider, native-mount and external-service rows remained explicitly ignored
  where their required harnesses were unavailable. This is source
  qualification only, not hosted or production acceptance.

  The latest shared-tip source gate on 2026-09-22 tested revision
  `e8f37152afdc79cf05a10eeb11d603fc55d395e0` and passed
  `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace
  Clippy with `-D warnings`, and the locked all-target workspace test suite.
  The current WebDAV close-timeout, core, transport, SDK and CLI coverage
  passed; FoundationDB provider, native-mount and external-service rows
  remained explicitly ignored where their required harnesses were unavailable.
  This is source qualification only, not hosted or production acceptance.

  The latest combined-mainline source gate on 2026-09-22 tested revision
  `57ade44a4ac59899f3ff561075701046d94564ca` and passed
  `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace
  Clippy with `-D warnings`, and
  `./scripts/cargo-shared test --workspace --all-targets --locked`. The
  feature-enabled FoundationDB provider and tests also passed link-free
  `check` and strict Clippy checks; native host unit linking remains blocked by
  the unavailable `libfdb_c`, while hosted Linux runtime evidence is recorded
  separately. This is source/provider compilation qualification only, not
  hosted or production acceptance.

  The current source gate on 2026-09-22 tested revision `87a500b1` and passed
  `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace
  Clippy with `-D warnings`, and
  `./scripts/cargo-shared test --workspace --all-targets --locked`. The
  feature-enabled FoundationDB provider and tests also passed link-free
  `check` and strict Clippy checks. The full suite retains explicit
  environment-gated skips for external credentials and native mount
  privileges; native host FoundationDB unit linking remains blocked by the
  unavailable `libfdb_c`. This is source/provider compilation qualification
  only, not hosted or production acceptance.

  The latest published-tip source gate on 2026-09-22 tested exact revision
  `a1fe6c88181350d1429f34911d2bc03f1ed34b44` and passed
  `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace
  Clippy with `-D warnings`, and
  `./scripts/cargo-shared test --workspace --all-targets --locked`. The
  link-free feature-enabled `mount-rs-foundationdb` `check` and strict Clippy
  checks also passed. The all-target suite retains explicit environment-gated
  skips for external credentials, native mount privileges and service-backed
  rows; this is source/provider qualification only, not production identity,
  platform, recovery, capacity, observability or release evidence.

  W07.7 remains open until every nested gate has concrete production-like
  evidence. No demo, local qualification, queued CI run or installation-only
  result may be promoted to a production PASS.
  The production gate ledger, deployment contract, operational runbook and
  rollout sequence are tracked in
  [`docs/foundationdb-production-rollout.md`](docs/foundationdb-production-rollout.md)
  and [`docs/W07-operations-runbook.md`](docs/W07-operations-runbook.md).

## W08 — TiDB

- [x] W08.1 Review and land the separate TiDB metadata/block implementation and
  dependencies (`ca57758`); four unit tests, format checks and test binaries
  passed.
- [x] W08.2 Complete the durable real TiDB/PD/TiKV Docker harness and restart
  results. Hosted CI run `35585066458` at source `9c098e5`, `tidb` job
  `106286459436`, passed the pinned v8.5.7 durable 3PD/3TiKV topology,
  frontend replacement, TiKV restart, PD restart, readiness checks and the
  post-restart persisted provider rerun. The acceptance marker was
  `evidence=durable-multinode-restart` on `linux/amd64` with 4 CPUs and
  16,766,414,848 bytes of Docker memory. The local host remains capacity-gated;
  hosted evidence is the authoritative service result.
- [x] W08.3 Verify provider time/fencing, ambiguous commits, concurrency and
  deployment durability assumptions. Liveness queries are not fsync evidence.
  The same hosted durable run passed provider-clock fencing/concurrency,
  ambiguous-commit no-replay, schema/identity, frontend/store/PD restart and
  persisted fresh-client checks. The ambiguous-commit injection is ordered
  after the durable restart gate so its intentionally unknown client outcome
  cannot contaminate restart readiness; it remains a distinct backend outcome.
- [x] W08.4 Add Node, CLI, native-mount and macOS/Linux acceptance coverage.
  Run `35585066458` passed the live Linux TiDB/RustFS consumer composition,
  ARM Node consumers, Linux FUSE, and both Ubuntu and macOS native-NFS jobs.
- [x] W08.4a The bounded Node/CLI consumer slice is wired through the public
  Rust SDK, N-API and Rust/Node CLI configuration: TiDB metadata can compose
  with RustFS/S3-compatible `r2` chunks, with explicit `durable` assertions.
  The provider matrix covers configuration, partial write, truncate, shutdown,
  reopen and owned RustFS-prefix cleanup. Local evidence: `cargo fmt --all -- --check`,
  focused Clippy, Rust SDK `2 passed`, CLI `40 library + 9 CLI
  tests passed`, N-API chunked/CLI/shutdown tests passed, Node matrix
  `pass=4 skip=3 fail=0`, and CLI matrix `pass=10 skip=3 fail=0`. The live
  TiDB+RustFS rows are explicit skips because `MOUNT_RS_TIDB_URL` and the
  loopback RustFS credential set are absent here. The consumer cleanup row
  removes only owned RustFS objects and verifies provider shutdown; exact TiDB
  metadata-row deletion remains the W08.5 service-harness boundary.
- [x] W08.4b Native-mount and live TiDB/RustFS consumer acceptance are
  complete for the defined hosted matrix. The `tidb-rustfs` job
  `106286459715` passed live TiDB/RustFS Node and CLI matrices, independent
  Rust/Node mounted I/O through Linux FUSE, clean unmount, fresh-provider
  readback and retained client bytes. The standalone `native-fuse` job
  `106286459998`, ARM Node job `106286459639`, Ubuntu native-NFS job
  `106286459483`, and macOS native-NFS job `106286459246` also passed. The
  macOS native-NFS row is a native-platform lifecycle gate, not a claim that
  TiDB/RustFS itself ran on macOS; the live provider row is the Linux FUSE
  composition job.
- [x] W08.5 **TiDB metadata + RustFS S3 chunks:** the real single-node v8.5.7
  TiDB service and pinned loopback RustFS endpoint passed the mixed-provider
  seed, partial-write, truncate, reopen, CAS/fencing and exact cleanup path;
  the hosted durable composition additionally passed the RustFS restart/reopen
  phases. Persisted fixtures require explicit volume/prefix/manifest scope and
  reject transient or symlink paths.
- [x] W08.6 **Production deployment-contract policy:**
  `scripts/verify-w08-production-config.mjs` now validates the production
  `splitstore` shape, durable TiDB and block declarations, HTTPS object
  storage, external secret references, no placeholders/inline secrets and
  TLS-required TiDB input without opening a provider connection. The positive
  fixture passed, the insecure fixture failed closed, and the public Rust CLI
  schema accepted the positive fixture. The hosted CI positive/negative gate
  passed in run `35592902494`, `tidb-tls-compile` job `106311076905` at source
  `4326c54`; this is implementation evidence only. P01/P02/P07 remain open
  for real topology, IAM, certificates and provider/security sign-off.
- [x] W08.7 **Production operator runbook and timed-drill matrix:** commit
  `ea01338` added `docs/W08-operations-runbook.md` with deployment admission,
  incident response, backup/restore, rollback, observability handoff and D01–D09
  drill templates. The artifact is complete; execution, owners and on-call
  acknowledgement remain P03–P08 gates.
- [x] W08.8 **Bounded TiDB/RustFS load and soak harness:**
  `tests/provider_matrix/tidb-rustfs-soak.mjs` and the hosted 64-operation
  seed/reopen slice are implemented and retained in run `35595664981`,
  `tidb-rustfs` job `106319766691`. This is bounded qualification, not a
  production capacity/SLO result; P06 remains open.
- [x] W08.9 **HTTP health/readiness contract:** `/healthz` and `/readyz` are
  implemented and documented with local and terminal hosted all-features
  contract evidence in `http-observability` job `106340034907`; collector,
  provider-aware checks, SLOs, paging and alert execution remain P05 gates.
- [x] W08.10 **Health response hardening:** `2f7919a` adds
  `Cache-Control: no-store` and `X-Content-Type-Options: nosniff` with local
  regression coverage and hosted terminal evidence in run `35601990956`; this
  remains HTTP contract evidence only.
- [x] W08.11 **Release identity/provenance policy:**
  `scripts/verify-w08-release-manifest.mjs` validates W08 source/repository,
  artifact SHA-256/size, `SHA256SUMS`, GitHub Actions workflow/run provenance and
  explicit signature/SBOM/canary states. Local pending/strict-accepted/invalid
  fixtures passed their intended paths, and hosted run `35606873984`, source
  `fecec0e`, `w08-release-policy` job `106356402785` was terminal success. This
  is a synthetic credential-free policy gate; real artifact signing, SBOM,
  canary, rollback and approval remain W08-P09 external gates.
- [x] W08.12 **Release artifact manifest generation and CLI preview wiring:**
  `scripts/write-w08-release-manifest.mjs` computes the actual artifact
  SHA-256/size and records source/tag/target/workflow/run provenance. The
  tag-triggered macOS CLI release workflow now generates/verifies
  `release-manifest.json` before upload, publishes it beside `SHA256SUMS`, and
  re-downloads/re-verifies it. Local locked CLI packaging and hosted run
  `35609172786`, source `66544b5`, `w08-release-policy` job `106363893748`
  passed. No tag publication, signing/SBOM, canary, rollback or approval is
  claimed.
- [x] W08.13 **Dedicated non-cancelling hosted release-policy gate:** commit
  `a9f51e4` moved the W08 policy job into
  `.github/workflows/w08-release-policy.yml`, removed the duplicate cancellable
  job from monolithic CI and set `cancel-in-progress: false`. Hosted run
  `35611883547`, source `f432441`, `w08-release-policy` job `106372777281`
  reached terminal success after building a real Ubuntu CLI artifact,
  packaging and checksumming it, generating/verifying the artifact-derived
  manifest, checking direct and extracted `mount-rs --version`, and running
  the policy fixtures. This is stable hosted artifact-path evidence; tag
  publication, signing/SBOM, target-platform acceptance, canary, rollback and
  approval remain W08-P09 gates.
- [x] W08.14 **Unsigned SBOM generation and release-artifact binding:**
  `scripts/write-w08-release-sbom.mjs` derives the `mount-rs-cli` transitive
  dependency closure from locked Cargo metadata and emits CycloneDX 1.5;
  `scripts/verify-w08-release-sbom.mjs` checks the graph, source identity and
  artifact SHA-256/size, including a local wrong-source negative test. The CLI
  preview and dedicated policy workflows generate and verify the SBOM, with
  the preview path publishing and re-downloading it beside the artifact,
  manifest and `SHA256SUMS`. Hosted run `35614345209`, source `9c9d0e4`,
  `w08-release-policy` job `106381893114` passed the real Ubuntu artifact path
  with `components=288` and `W08_RELEASE_SBOM_PASS`. The SBOM is unsigned and
  the manifest remains `sbom=pending`; signing/attestation, target-platform
  parity, canary, rollback and approval remain W08-P07/P09 gates.
- [x] W08.15 **All-release-asset checksum coverage:** both
  `.github/workflows/cli-release.yml` and
  `.github/workflows/w08-release-policy.yml` now generate `SHA256SUMS` only
  after the tarball, `release-manifest.json` and `release-sbom.json` exist,
  then verify all three entries. Hosted run `35615714935`, source `5116ded`,
  `w08-release-policy` job `106386240669` passed the real Ubuntu path with
  `mount-rs-0.1.0-x86_64-unknown-linux-gnu.tar.gz: OK`,
  `release-manifest.json: OK` and `release-sbom.json: OK`. This closes the
  repository asset-integrity slice only; tag publication, downloaded-release
  inspection, signing/attestation, target-platform parity, canary, rollback
  and approval remain W08-P09 gates.
- [x] W08.16 **Linux x86_64 and macOS arm64 target-package matrix:**
  `.github/workflows/w08-release-targets.yml` builds/tests the CLI on
  `ubuntu-latest` (`x86_64-unknown-linux-gnu`) and `macos-14`
  (`aarch64-apple-darwin`), generates/verifies each manifest and
  288-component SBOM, creates three-entry checksums, runs direct/extracted
  version checks, uploads each four-file asset set and verifies each set after
  download. Hosted run `35617415427`, source `b0ca8a9`, passed build jobs
  `106391572292` and `106391572540` plus downloaded-asset jobs
  `106393402528` and `106393402680`. This closes target-package and hosted
  artifact-boundary implementation evidence only; tag publication,
  signing/attestation, release-registry acceptance, canary, rollback and
  approval remain W08-P09 gates.
- [x] W08.17 **Hosted cryptographic provenance and SBOM attestation wiring:**
  `.github/workflows/cli-release.yml` now grants OIDC/attestation permissions
  only to the tag-release job, invokes pinned
  `actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6` (`v4.2.2`) for the
  exact CLI tarball and CycloneDX SBOM, and verifies both with
  `gh attestation verify` against the repository, signer workflow, source
  commit, tag ref and hosted-runner identity. After those checks it rewrites
  the manifest with `signature=verified sbom=verified` and rebuilds the
  three-entry `SHA256SUMS`. `.github/workflows/w08-release-targets.yml` also
  exposes a manual `attest=true` path that performs the same per-target
  provenance/SBOM qualification. Local YAML, embedded-Bash and `gh` flag
  checks pass. Manual run `35620932700` reached terminal target-build and
  downloaded-asset PASS jobs, but both attestation jobs failed during setup
  because GitHub rejected the shortened action ref; no action step, OIDC token
  or Sigstore bundle was created. The full v4.2.2 SHA correction is now in the
  workflow; later terminal reruns are recorded in W08.21 and W08.22. This
  implementation row does not itself claim an attestation ID/URL,
  release-registry result, canary, rollback or approval. *(Release
  implementation slice; GitHub OIDC/attestation availability, release owner
  and production approvers are external gates.)*
- [x] W08.18 **Attestation dispatch concurrency isolation:**
  `.github/workflows/w08-release-targets.yml` now keys its concurrency group by
  event type and ref, separating the explicit manual `workflow_dispatch`
  attestation qualification from push-triggered target runs. The first
  dispatch `35620392878` at source `0a4de6f` was accepted but cancelled before
  job creation (`jobs=[]`) while the old shared pending group was occupied by
  concurrent main pushes; it created no OIDC token or attestation and is not a
  PASS or provider failure. Push the fix and rerun the manual qualification;
  target attestations, tag publication, canary, rollback and approval remain
  W08-P09 gates. *(Release workflow implementation; hosted concurrency and
  GitHub OIDC/attestation service are external gates.)*
- [x] W08.19 **Full SHA pin correction after hosted setup failure:** both
  `actions/attest` references now use the full immutable
  `1e69f48acb82d1966a394da916b4c1698aa569d6` commit for `v4.2.2`. Hosted run
  `35620932700` passed both target builds and downloaded-asset checks, but its
  attestation jobs `106406881524` and `106406881625` failed before any action
  step because GitHub rejected the shortened ref; no OIDC token or Sigstore
  bundle was created. The correction is implementation-complete but requires a
  fresh `attest=true` hosted run; target attestation, tag publication, canary,
  rollback and approval remain W08-P09 gates. The corrected run `35624385556`
  later passed both target attestation jobs. *(Release implementation fix;
  GitHub action resolution and hosted attestation are external gates.)*
- [x] W08.20 **Hosted attestation verifier identity-flag correction:** the
  target qualification `35622899242` at source `2f43721` passed both target
  builds, both downloaded-asset checks and both provenance/SBOM attestation
  generation steps. Its final `gh attestation verify` steps failed because
  GitHub CLI rejects simultaneous `--signer-repo` and `--signer-workflow`
  options. Both release workflows now use the precise `--signer-workflow`
  identity without the redundant repository option. The correction is ready
  for a fresh `attest=true` run; terminal verifier acceptance, tag publication,
  canary, rollback and approval remain W08-P09 gates. The corrected run
  `35624385556` passed both target verifier jobs. *(Release implementation fix;
  hosted verifier behavior, OIDC/attestation availability and release approval
  are external gates.)*
- [x] W08.21 **Terminal hosted target attestation qualification:** manual run
  `35624385556` at source `2ab3cf1` passed Linux x86_64 and macOS arm64 build,
  package and downloaded-asset verification, then generated and verified both
  provenance and CycloneDX SBOM attestations for each target. Linux tarball
  SHA-256 is `5f7f3c6345013144d8889b107cde41c9e5b69d688e21a975ba6fbb33ce2507d6`
  (8,277,051 bytes); macOS arm64 is
  `add8e365c0ab1c0390267531144b77b6c7cf7f338d03ce0a549a5783c834fc8e`
  (6,910,097 bytes). Repository attestation records `48984689`, `48984696`,
  `48984678` and `48984687` and Rekor entries `2906371944`, `2906371970`,
  `2906371892` and `2906371927` are retained in the ledger. This closes the
  hosted target-attestation qualification only; approved tag publication,
  release-registry acceptance, canary, rollback and owner approval remain
  W08-P09 gates. *(Hosted/provider qualification; release approval and
  production deployment are external gates.)*
- [x] W08.22 **Current-main hosted target-attestation retest:** manual
  `workflow_dispatch` run `35627761501` completed successfully at source
  `50a33ace3880632bec514ed9f163b84906376f5e`. Linux build/download/attestation
  jobs `106426208811`, `106427912681`, `106428371158` and macOS jobs
  `106426208445`, `106427912698`, `106428371145` all passed final target
  verification. Linux tarball SHA-256 is
  `432049a39cd648bda95a9aeaaf81d5bb2933267c467f69b617e6e3f718d5303b`
  (8,306,821 bytes); macOS arm64 is
  `647f4d0fe7cb7f8446ed6eba3a2c1ad4b4f7413ad59afa10d68617b86ec10421`
  (6,926,739 bytes). Repository attestation records are `48993605`,
  `48993613`, `48993668` and `48993679`; Rekor entries are `2906426581`,
  `2906426592`, `2906426797` and `2906426824`. Both
  `W08_RELEASE_TARGET_ATTESTATION_PASS` markers passed. This is a
  current-main-at-dispatch hosted retest, not an approved tag release,
  registry acceptance, canary, rollback or owner approval. *(Hosted/provider
  qualification; release approval and production deployment are external
  gates.)*
- [x] W08.23 **Current shared-main workspace verification:** on source
  `5aae52bc776e17f1ee56b164ee161f01692b3e83`, the locked workspace command
  `./scripts/cargo-shared test --workspace --all-targets --locked` and strict
  workspace Clippy command
  `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings`
  both exited successfully. All non-ignored tests passed; provider/native rows
  requiring TiDB, RustFS, PGlite, R2, FUSE, NFS or other host capabilities
  remained explicit skips/ignores. This is current source-health evidence only;
  it does not close live provider, native-kernel or W08-P01–P09 production
  gates. *(Implementation verification; provider, native and production
  environments remain external.)*
- [x] W08.24 **Current published-main hosted target-attestation qualification:**
  manual `workflow_dispatch` run `35631063978` completed successfully at
  source `cf75835e5a4b00c1e9b31f4060093710df610ece`. Linux build/download/
  attestation jobs `106437098891`, `106439285204`, `106439636662` and macOS
  jobs `106437098611`, `106439285356`, `106439636979` all passed final target
  verification. Linux tarball SHA-256 is
  `54220877022f67640aa7b470a8eaa09b24df1d867e00463533642b6fa462182a`
  (8,315,540 bytes); macOS arm64 is
  `6df488c71a0eb65674af9adf7660b870424b4e47227d832028180947dbb83b39`
  (6,918,800 bytes). Repository attestation records are `49001528`,
  `49001543`, `49001531` and `49001545`; Rekor entries are `2906464391`,
  `2906464436`, `2906464411` and `2906464449`. Both
  `W08_RELEASE_TARGET_ATTESTATION_PASS` markers passed. This is hosted
  published-main qualification, not an approved tag release, registry
  acceptance, canary, rollback or owner approval. *(Hosted/provider
  qualification; release approval and production deployment are external
  gates.)*
- [x] W08.25 **Current shared-main workspace verification after the FUSE session
  fix:** on source `f16eec2a6a1f79a5543ba345c67b86eaae837312`, the full locked
  workspace test command and strict workspace Clippy command both exited 0
  using fresh bounded target `/private/tmp/mount-rs-w08-current-cargo-target`.
  All non-ignored tests passed and Clippy reported no diagnostics with
  `-D warnings`; TiDB, RustFS, PGlite, R2, FUSE, NFS and other capability-gated
  rows remained explicit skips/ignores. Two attempts against the configured
  shared target failed before tests because dependency metadata/artifacts were
  missing, and are not counted as source failures or passing evidence. This
  is source-health evidence only; it does not close live provider,
  native-kernel or W08-P01–P09 production gates. *(Implementation
  verification; provider, native and production environments remain
  external.)*

- [x] W08.26 **Current-main hosted Linux/macOS target and attestation
  qualification:** manual `workflow_dispatch` with `attest=true` run
  `35633841116` completed successfully from source
  `56aae0c3f28686e225bb5feb0c528e50acc0a1e8`. Linux build/download/
  attestation jobs `106446286719`, `106447678688`, `106448672423` and macOS
  jobs `106446286427`, `106447678718`, `106448672409` all passed final target
  verification. Linux tarball SHA-256 is
  `5ef9ddb46b5a7967fb26b6379a217f648291d2598be75e9c7d8847f7aa42371c`
  (8,328,164 bytes); macOS arm64 is
  `f857b369be15f8f09833722acb4b1739dd05a794aa0636aa4135f5f703905bec`
  (6,918,766 bytes). Repository attestation records are `49008658`,
  `49008661`, `49008767` and `49008775`; Rekor entries are `2906497681`,
  `2906497694`, `2906498178` and `2906498191`. Both
  `W08_RELEASE_TARGET_ATTESTATION_PASS` markers passed. This is current-main
  hosted qualification, not an approved tag release, registry acceptance,
  canary, rollback or owner approval. *(Hosted/provider qualification;
  release approval and production deployment are external gates.)*

- [x] W08.27 **Latest integrated-main workspace verification:** on source
  `5c7cc7053523a92c8f4da13f09993910dfdb541a`, the full locked workspace test
  command and strict workspace Clippy command both exited 0 using bounded
  target `/private/tmp/mount-rs-w08-current-cargo-target`. All non-ignored
  tests passed and Clippy reported no diagnostics with `-D warnings` after the
  latest integrated FUSE/S3/N-API changes; TiDB, RustFS, PGlite, R2, FUSE, NFS
  and other capability-gated rows remained explicit skips/ignores. This is
  source-health evidence only; it does not close live provider, native-kernel
  or W08-P01–P09 production gates. *(Implementation verification; provider,
  native and production environments remain external.)*

- [x] W08.28 **Protected production-candidate release admission:** added
  `.github/workflows/w08-production-release.yml`, scoped to immutable
  `v*-cli-production-candidate*` tags. It builds/tests/packages Linux x86_64 and
  macOS arm64, verifies downloaded target assets, generates and verifies the
  artifact-bound manifest/SBOM/checksums, generates and verifies both hosted
  provenance/SBOM attestations, and re-verifies the final GitHub release assets.
  Publication is a prerelease and is held behind the protected `w08-production`
  environment; it refuses non-main ancestry and refuses to overwrite an
  existing release. The existing preview workflow now matches only
  `v*-cli-preview*` tags, so production candidates cannot silently use the
  preview path. Local YAML parsing, embedded Bash `bash -n` checks and
  `git diff --check` passed. No candidate tag was created in this chunk, so no
  hosted release, canary, rollback or production GO evidence is claimed.
  Implementation commit `dff55858` was reconciled with concurrent mainline
  changes and published in merge tip `6abc0535` (`origin/main`).
  *(Implementation/static qualification; environment approval, release
  registry and rollout evidence remain external.)*

- [x] W08.29 **Current published-main hosted target and attestation
  qualification:** manual `workflow_dispatch` with `attest=true` run
  `35638433010` at source
  `9d3a6e502eccec9ba54c00e80c98e6e1da175177` passed both target builds,
  downloaded-asset checks, provenance attestations, CycloneDX SBOM attestations
  and final `gh attestation verify` jobs. Linux job `106461545303` built
  `x86_64-unknown-linux-gnu` SHA-256
  `29130f9d1a753bdcf7dbd2412146585bfc4bdce55c5ddbb4946a79a653696c20`
  (8,347,688 bytes); macOS job `106461544977` built
  `aarch64-apple-darwin` SHA-256
  `1530afd0325416db239776c843343e6c99e37406a0cc82be27490d26060ba27c`
  (6,943,737 bytes). Download jobs `106463019865` and `106463019693`, and
  attestation jobs `106463113298` and `106463113247`, all passed; both
  `W08_RELEASE_TARGET_ATTESTATION_PASS` markers passed. The downloaded assets'
  checksums, manifests, 288-component SBOMs and tar contents were independently
  reverified locally. This is hosted target qualification only; no candidate
  tag, protected-environment approval, release registry publication, canary,
  rollback or production GO evidence is claimed. *(Hosted/provider
  qualification; production release gates remain external.)* The evidence
  capture was committed as `ad45bf06` and published in merge tip
  `d68f14de3c13c0510237e9a88c615c1a8e74c9a3`.

- [x] W08.30 **Current public-tip source verification:** on source
  `76c2b1a863c23afe71c0591d0a480433e1b9078d`,
  `CARGO_TARGET_DIR=/private/tmp/mount-rs-w08-final-cargo-target
  ./scripts/cargo-shared test --workspace --all-targets --locked --offline`
  completed successfully with all executed tests passing, and the matching
  strict workspace Clippy command with `-D warnings` completed successfully
  with no diagnostics. The bounded target stayed outside the worktree. Tests
  that require TiDB/RustFS/PGlite/R2 credentials or FUSE/NFS/native services
  remained explicit ignored prerequisites; this is source-level verification,
  not live-provider or production-rollout evidence. *(Implementation
  verification; provider, native and production gates remain external.)*
- [x] W08.31 **Latest hosted target, artifact and attestation qualification:**
  manual `workflow_dispatch` with `attest=true` run `35641555767` completed
  successfully from current-main-at-dispatch source
  `2bbd0266a094b06bf97d51baafe0d3a8800bfec5`. Linux build job
  `106471833712`, macOS arm64 build job `106471833967`, downloaded-asset jobs
  `106473358964` and `106473359006`, and attestation jobs `106475107803` and
  `106475108148` all passed. Linux SHA-256 is
  `6a3de0a607ffafcedf6bd3385c3849a30208df0345120061061821109a09ac79`
  (8,373,376 bytes); macOS arm64 SHA-256 is
  `75a031c4e439ede07f0fa1a09db050b15c45f6d802a703d90b67cde52b4a941d`
  (6,958,388 bytes). Both hosted target/download/attestation markers passed;
  downloaded checksums, manifests, 288-component SBOMs, archive contents and
  the extracted macOS `mount-rs 0.1.0` runtime were independently rechecked,
  and exact-identity SLSA/CycloneDX attestation verification passed with the
  `main` source ref and `--deny-self-hosted-runners`. The production-config
  positive policy fixture passed and the insecure fixture failed closed with
  `secret_access_key-must-not-be-inline`; this is policy evidence only. No
  production-candidate tag, protected-environment approval, registry
  publication, canary, rollback or GO evidence is claimed. *(Hosted/provider
  qualification and implementation policy; production gates remain external.)*
  The evidence documentation commit `d293fe5b` was reconciled into public
  merge tip `682d2441` for other workstreams to consume.
- [x] W08.32 **Production rollout ledger consistency guard:** added
  `scripts/verify-w08-rollout-ledger.mjs` and its six-case
  `scripts/test-w08-rollout-ledger.mjs` control. The verifier requires all
  W08.1–W08.32 implementation items to be checked, keeps the nine W08-P01–P09
  production gates open while the decision is NO-GO, and cross-checks the
  tracker, production-rollout document, detailed ledger and operations runbook.
  It fails closed if a gate is marked complete prematurely, a drill boundary
  disappears, the decision changes to GO without all gate checkboxes, or the
  documents disagree. The dedicated W08 release-policy workflow now runs both
  the current NO-GO verifier and simulated invalid/GO transitions. This is a
  repository tracking control only; it does not close any provider, hosted,
  native or production gate. Hosted run `35647096385` from source
  `2641962a6a65179abf4b8d785345fbe6af4be9b8`, job `106490142559`, reached
  terminal `success` and passed both the rollout-ledger verifier/test and the
  existing release-identity/provenance policy step. The earlier push-triggered
  run `35646848615` was cancelled before creating jobs (`jobs=[]`) and is not
  evidence. *(Implementation/static qualification; production evidence and
  approval remain external.)*
- [x] W08.33 **Production-candidate GO admission guard:** extended
  `scripts/verify-w08-rollout-ledger.mjs` with `--require-go`, added the
  NO-GO admission-negative and simulated-complete-GO cases to
  `scripts/test-w08-rollout-ledger.mjs`, and placed the fail-closed check in
  the `admission` job of `.github/workflows/w08-production-release.yml` before
  either target can build. A production-candidate tag therefore cannot reach
  artifact generation or publication while the authoritative tracker remains
  NO-GO or any P01–P09 checkbox is open. This is a release safety control only;
  it does not create topology, provider, secret, canary, rollback or owner
  approval evidence. The post-push policy run `35648755697` and credential-free
  manual dispatch `35648898876` both cancelled before creating jobs, so hosted
  W08.33 admission qualification remains unexecuted. *(Implementation/static
  qualification; production evidence and approval remain external.)*
- [x] W08.34 **Machine-readable production evidence admission:** added
  `docs/W08-production-evidence.json` as an explicit NO-GO packet with all nine
  P01–P09 rows, remaining actions and empty evidence arrays; added
  `scripts/verify-w08-production-evidence.mjs` and its ten-case
  `scripts/test-w08-production-evidence.mjs` control. The validator cross-checks
  the authoritative rollout decision, requires all nine gates to be closed and
  populated with full revision/provider-version/topology/environment/run,
  terminal-status/owner/cleanup/rollback/reference fields before GO, and is
  wired into both the normal policy workflow and the production-candidate
  admission job. This validates evidence completeness only; it cannot
  authenticate provider results or create production approval. *(Implementation
  /static qualification; production evidence and approval remain external.)*
  The current-tip push run `35650626028` at source
  `8f3a19a891b8d432ff551c04789921575bb12f4f` cancelled before creating jobs
  (`jobs=[]`); the earlier successful run `35650533691` at source `95437f6`
  predates W08.34 and is not counted as packet-validator evidence. Current-tip
  run `35651363875` at source `14f8c344a5a7f5b2e8cb08475db3d87ecbfc23d7`,
  job `106504528377`, later completed successfully with the W08 policy and
  release-identity/provenance steps green.

- [x] W08.35 **Docker server-capacity preflight hardening:** the real TiDB
  harness now obtains architecture, CPU and memory from one formatted Docker
  server-info probe before any network, volume or container is created. A
  daemon that becomes unavailable between a plain health check and a field
  query now exits cleanly with a prerequisite-boundary message instead of
  leaking a Docker CLI panic or entering partial topology setup. `sh -n` and
  the current local unavailable-daemon run both passed the expected fail-closed
  path with exit status 2 and no `TIDB_ACCEPTANCE` marker. This improves
  diagnosis and cleanup safety; it does not make local Docker capacity,
  provider credentials or production topology evidence available. *(Harness
  implementation/static qualification; provider and production gates remain
  external.)*

- [x] W08.36 **Production evidence placeholder rejection:** the machine-
  readable W08 GO validator now rejects placeholder-shaped provider versions,
  topology, environment, run, owner, cleanup, rollback and evidence-reference
  values such as `TBD`, `pending`, `unknown`, `TODO`, template markers and
  angle-bracket substitutions. The regression suite now covers a synthetic GO
  packet with `topology=TBD` and fails it closed while the real packet remains
  NO-GO with zero evidence records. This prevents tracking text from being
  mistaken for terminal production evidence; it does not authenticate a real
  provider run or grant release approval. *(Implementation/static
  qualification; production evidence and approval remain external.)*

- [x] W08.37 **Replicated-durable production topology policy:** added
  `scripts/verify-w08-production-topology.mjs` and its eight-case
  `scripts/test-w08-production-topology.mjs` control over
  `tests/tidb/production-topology-policy.json`. The policy requires an
  explicitly replicated-durable topology with at least three PD members,
  three TiKV members, two SQL frontends, majority quorum, pinned coherent
  TiDB component versions, private TLS-enabled networking, the durable
  resource floor and tenant-isolation support metadata. A
  `single-node-smoke` classification is rejected rather than promoted to
  production-like evidence; the existing harness marker
  `single-node-smoke-not-replicated-acceptance` remains a separate smoke
  boundary. The credential-free policy and test are wired into both the
  dedicated W08 release-policy workflow and protected production admission.
  Implementation commit `263be2f4` was reconciled with concurrent mainline
  changes and published in exact public merge `fab2a0cb`; hosted W08 policy
  run `35709828967`, exact head `fab2a0cb`, job `106688181078`, completed
  successfully in about 2m45s. Its topology-control, rollout/evidence and
  release-policy artifact checks are hosted implementation/static evidence
  only, not provider or production acceptance.
  This is a P01 implementation/control boundary only; it does not prove a
  deployed topology, quorum, provider durability, capacity or production
  approval. *(Implementation/static qualification; provider and production
  evidence remain external.)*

- [x] W08.38 **External secret-manager rotation and audit policy:** added
  `scripts/verify-w08-production-secrets.mjs` and its eight-case
  `scripts/test-w08-production-secrets.mjs` control over
  `tests/tidb/production-secrets-policy.json`. The credential-free contract
  requires an external secret manager, workload/managed identity, exactly the
  three production secret references, bounded rotation with overlap and
  revocation-on-failure, redacted access auditing, retained audit records and
  a two-person break-glass procedure reference. Inline secret values, static
  identity, missing/duplicate references, unsafe rotation and unredacted audit
  fixtures fail closed. The checks are wired into both W08 release workflows.
  This is a P02 implementation/control boundary only; it does not prove a
  configured secret manager, IAM grants, credential rotation, audit events or
  production approval. *(Implementation/static qualification; provider and
  production evidence remain external.)*
- [x] W08.39 **External backup, restore and DR policy:** added
  `scripts/verify-w08-production-backup.mjs` and its eight-case
  `scripts/test-w08-production-backup.mjs` control over
  `tests/tidb/production-backup-policy.json`. The credential-free contract
  requires transactionally consistent TiDB metadata snapshots, revision
  capture, encrypted immutable/versioned block retention, isolated restore
  with separate identity and no production-writer access, fresh-client
  readback, corruption/partial-object/region-loss cases, bounded 60-minute
  RPO and 240-minute RTO, a second region and data-owner/release-owner
  sign-off references. Missing consistency, unsafe restore access, weak
  retention, unbounded recovery objectives and incomplete DR sign-off fail
  closed. The checks are wired into both W08 release workflows. This is a P03
  implementation/control boundary only; it does not prove a backup, restore,
  second region, provider recovery drill or production approval.
  *(Implementation/static qualification; provider and production evidence
  remain external.)*
- [x] W08.40 **External upgrade, compatibility and rollback policy:** added
  `scripts/verify-w08-production-upgrade.mjs` and its eight-case
  `scripts/test-w08-production-upgrade.mjs` control over
  `tests/tidb/production-upgrade-policy.json`. The credential-free contract
  requires pinned current/previous component versions, all five supported
  consumer surfaces, rehearsed wire/data-read compatibility, expand-contract
  schema and versioned config migration, forward/backward compatibility,
  interrupted-upgrade recovery, rolling quorum-preserving upgrade gates and
  retained artifact/config/data rollback with writer fencing and fresh-client
  readback. It requires a 30-day retained rollback artifact and service-owner/
  release-owner sign-off references. Destructive migrations, missing client
  surfaces, quorum loss, weak retention and incomplete rollback controls fail
  closed. The checks are wired into both W08 release workflows. This is a P04
  implementation/control boundary only; it does not prove a live upgrade,
  rollback, provider compatibility or production approval.
  *(Implementation/static qualification; provider and production evidence
  remain external.)*

  Tested source base `c9df268902335934dbe2c369de881803ca376bcd` was freshly
  reverified after the 9P N-API server-lifecycle gate isolation, the WebDAV
  bounded propfind/copy failure fix, the
  FoundationDB storage/test qualification changes, 9P
  undefined-UID preservation, W07 lease-authority telemetry, N-API
  postbuild/session-metadata changes, the W26 fenced-metadata publication fast
  path, the 9P platform-type alias and 9P direct-probe
  absence-field normalization and WebDAV
  shared-resource ordering qualification, plus concurrent WebDAV
  mounted-restart cleanup correction, chunked lease-release fix, 9P synchronous
  mount inspection, FoundationDB qualification-harness updates, S3
  conditional-put/session-concurrency gateway coverage, N-API
  provider-network cleanup, RustFS/Ozone lockfile refreshes, 9P bounded-reader,
  9P frame-assembler and WebDAV native-concurrency, R2 upload coalescing, N-API
  declaration/P9 normalization, 9P codec, S3-session-concurrency,
  session-parity, chunked durability, FUSE, S3-test and WebDAV updates with
  the full locked workspace test suite (exit 0) and strict workspace Clippy
  with `-D warnings` (exit 0).
  The four W08 rollout/evidence policy commands also passed with 36 functional
  items, 9 open production gates, 7 rollout tests and 11 evidence tests; the
  packet remains NO-GO with zero evidence records. Changed N-API JavaScript and
  package JSON also passed static checks. Provider/native rows requiring TiDB,
  RustFS, PGlite, R2, FUSE or NFS remained explicit opt-in skips. This is
  source-health and tracking-control evidence only and does not close
  W08-P01–P09. This exact merged source was requalified after the 9P
  workflow/server, WebDAV, FoundationDB, 9P, W07 and N-API source changes; no
  source result is inferred from documentation-only evidence.

  After origin/main advanced with the WebDAV N-API test update `de78011` and
  concurrent documentation, exact merged source
  `068c42d6f540e31e88368e59546f19dd595f1070` was freshly requalified. The full
  locked workspace test and strict workspace Clippy exited 0; changed N-API
  JavaScript/package checks, all four W08 rollout/evidence validators/tests,
  native-9P workflow YAML parsing and `git diff --check` also passed. This is
  source-health and tracking-control evidence only; provider/native rows remain
  explicit opt-in skips and production P01–P09 remain open.

  After a subsequent origin/main advance with W01/W07/S3 N-API and session
  source changes, exact merged source
  `a47dbb75b9830fc83e17cc7dfa78b06ad3b9077b` was freshly requalified. The full
  locked workspace test and strict workspace Clippy exited 0; all runnable tests
  passed, provider/native rows remained explicit opt-in skips, and the affected
  N-API, W07 and W08 static/policy checks also passed. This is source-health
  evidence only and does not close P01–P09 or change the NO-GO decision.

  After origin/main advanced with the S3 delete-error source fix `d5c3f672` and
  its gateway coverage, exact merged source
  `6f8548a9179083ee088d4c1f66ded8d4c3b5b87e` was freshly requalified. The full
  locked workspace test and strict workspace Clippy exited 0; all runnable tests
  passed, provider/native rows remained explicit opt-in skips, and affected
  N-API/package, W08 policy/evidence and native-9P workflow-shape checks passed.
  This is source-health evidence only and does not close P01–P09 or change the
  NO-GO decision.

  After origin/main advanced with the WebDAV listener restart/backpressure and
  bounded recursive-mutation fixes, duplicate-header handling and related N-API
  changes, exact merged source
  `7752dd2e26ab5f423dfd984c1f9e5887465fd9c9` was freshly requalified. The full
  locked workspace test and strict workspace Clippy exited 0; all runnable tests
  passed, the WebDAV test group reported 26 passing tests, provider/native rows
  remained explicit opt-in skips, and affected N-API/package, W08 policy/evidence
  and native-9P workflow-shape checks passed. This is source-health evidence only
  and does not close P01–P09 or change the NO-GO decision.

  Hosted W08 policy run `35681936375` at source
  `e7be3769dd7c6722ce096c481f30d042d7895dbe`, job `106600554617`, completed
  successfully in 2m48s. Its rollout-ledger and release-identity/provenance
  checks are hosted implementation/static evidence only; they do not create
  provider, candidate-release, canary, rollback or owner-approval evidence.

  The later hosted W08 policy run `35682838216` at source
  `1eef1d9df6c9b05350c1857a72237bb7e5603fac`, job `106603416051`, completed
  successfully in 2m47s with the rollout-ledger and release-identity/provenance
  checks green. The hosted native-9P workflow run `35682638941` at source
  `007e6545d1b25d708abfa10f2120f81fba59a74a` also completed successfully; its
  `native-9p` job `106602683880` and `N-API native 9P lifecycle` job
  `106602684115` were both green. These are hosted implementation/native
  functional qualification only. They do not supply production topology,
  provider, candidate-release, registry, canary, rollback or owner-approval
  evidence, so P01–P09 remain open and the decision remains NO-GO.

  The public source-equivalent checkpoint
  `62e86dc092dfe9816ef54ca681872ff9f51d8895` passed hosted W08 release-policy
  run `35683936652`, job `106607017325`, which completed successfully in
  approximately 2m47s. Its rollout-ledger and release-identity/provenance steps
    were green. This is hosted implementation/static evidence only; it does not
    provide production topology, provider, candidate-release, registry, canary,
    rollback or owner-approval evidence, so P01–P09 remain open and the decision
    remains NO-GO.

  The immediately subsequent public merge tip
  `622dd0dc82125ba1979ea7ebf2b6a1b11145c6bb` had W08 policy run `35684358649`
  cancelled before job creation (`jobs=[]`) when concurrent public tip
  `9563d2db8d73b8583212eed00f5b909cbcadf27e` arrived. The surviving current
  public-source-equivalent run `35684400799` at source `9563d2db`, job
  `106608087688`, completed successfully in 2m05s with both W08 policy steps
  green. The cancellation is a hosted scheduling boundary, not evidence of a
  source or production failure; the successful run remains implementation/static
  evidence only and P01–P09 remain open.

  The current public source-equivalent checkpoint
  `1626d5381625d81696fa28624342f07087593760` passed hosted W08 release-policy
  run `35684975387`, job `106609795509`, which completed successfully in 2m46s
  with both hosted policy steps green. This remains hosted implementation/static
  evidence only; it does not provide production topology, provider,
  candidate-release, registry, canary, rollback or owner-approval evidence, so
  P01–P09 remain open and the decision remains NO-GO.

  After origin/main advanced with the FoundationDB/TiDB storage changes, 9P/W07
  telemetry updates and N-API test expansion, exact merged source
  `858682f63ca5b2da3f3610a8d8d641d77c2a904e` was freshly requalified. The full
  locked workspace test and strict workspace Clippy exited 0; all runnable tests
  passed, provider/native rows remained explicit opt-in skips, and affected
  N-API/package, W08 policy/evidence and native-9P workflow-shape checks passed.
  This is source-health evidence only and does not close P01–P09 or change the
  NO-GO decision.

  The published source-equivalent checkpoint
  `a47c0cd91699deea9888d3d87aeb04b64fdb6576` passed hosted W08 release-policy
  run `35685756342`, job `106612127535`, which completed successfully in 2m47s
  with both hosted policy steps green. This remains hosted implementation/static
  evidence only; it does not provide production topology, provider,
  candidate-release, registry, canary, rollback or owner-approval evidence, so
  P01–P09 remain open and the decision remains NO-GO.

  The public source-equivalent checkpoint
  `1e64bc250b5e863620f069aad173945dc474c5b4` passed hosted W08 release-policy
  run `35686337804`, job `106613902895`, which completed successfully in 2m48s
  with both hosted policy steps green. This remains hosted implementation/static
  evidence only; it does not provide production topology, provider,
  candidate-release, registry, canary, rollback or owner-approval evidence, so
  P01–P09 remain open and the decision remains NO-GO.

  After origin/main advanced with the 9P EOF/backpressure server fix `1179d9e3`,
  N-API test expansion and site component updates, exact merged source
  `d725248534eb897de4d49e916c47425e6266c05d` was freshly requalified. The full
  locked workspace test and strict workspace Clippy exited 0; all runnable tests
  passed, provider/native rows remained explicit opt-in skips, and affected
  N-API/package, W08 policy/evidence and native-9P workflow-shape checks passed.
  This is source-health evidence only and does not close P01–P09 or change the
  NO-GO decision.

  A fresh read-only production-boundary audit at **2026-09-22 14:23 AEST**
  returned no `w08-production-release.yml` runs, HTTP 404 for the
  `w08-production` environment, only prerelease `v0.1.0-cli-preview`, and no
  `v*-cli-production-candidate*` tag. This is an external GitHub/API and
  release-configuration blocker; no production mutation or approval was
  attempted, so P09 remains open.

  A fresh 11:36 AEST repository-policy check passed the positive production
  config fixture with an out-of-band non-secret TLS-policy URL, failed closed
  on the insecure/inline-secret fixture, passed the pending and strict accepted
  release-manifest fixtures, and failed closed on the invalid manifest. These
  are admission-control checks only; no provider, signing, SBOM, canary,
  rollback or approval evidence was created, so P01/P02/P07/P09 remain open.
  A 12:30 AEST prerequisite probe could not contact the Docker daemon and
  found no live TiDB/R2/PGlite/RustFS endpoint or credential variable. The
  representative TLS-policy config passed, the inline-secret config failed
  closed, and the strict accepted release-manifest fixture passed with
  `--require-release-acceptance`; these remain repository controls only and
  do not close P01/P02/P07/P09.
  A fresh 12:40 AEST read-only audit again returned HTTP 404 for both W08
  workflow queries and the `w08-production` environment, could not resolve the
  release surface, and found no production-candidate tag. No release,
  canary, rollback or approval evidence was created, so P09 remains externally
  blocked.

### W08 production rollout track — NO-GO (15% provisional)

The demo and W08 functional acceptance are not production approval. Track the
following gates separately from implementation, hosted provider, and native
platform evidence; the detailed ledger and evidence boundaries are in
`docs/W08-progress-ledger.md`; the deployment contract and rollout sequence
are in [`docs/W08-production-rollout.md`](docs/W08-production-rollout.md),
with operator procedures and timed drills in
[`docs/W08-operations-runbook.md`](docs/W08-operations-runbook.md). No
production gate is checked until its exit evidence is terminal, owned and
reproducible in a production-like environment.

- [ ] **W08-P01 (25%) — deployment contract/topology:** choose and document the
  managed or self-hosted TiDB/PD/TiKV and object-storage architecture, HA,
  regions, TLS/network policy, resource limits, versions, tenancy and IaC;
  prove a staging deployment and smoke/restart gate. The terminal
  `tidb-tls-compile` job `106311076905` in run `35592902494` confirms that the
  public consumers can include the TLS client graph and that the production
  config policy passes positive/negative checks, but it does not prove a
  deployment or handshake. The local `verify-w08-production-config.mjs`
  positive fixture and public CLI schema check also pass, while its insecure
  fixture fails closed; this validates contract shape only. *(Implementation
  + hosted/provider; target platform not supplied.)*
  A fresh local `MOUNT_RS_TIDB_TOPOLOGY=single ./scripts/test-tidb.sh` attempt
  entered the real v8.5.7 PD startup lane but exited 125 when the Docker engine
  returned `Bad response from Docker engine`; the preceding durable attempt
  failed closed before launch at `memory_bytes=8232747008` below the required
  `10737418240`. Neither attempt emitted `TIDB_ACCEPTANCE`, so neither closes
  P01 or promotes local infrastructure into production evidence.
- [ ] **W08-P02 (20%) — secrets/IAM/rotation:** bind production credentials
  through the approved secret manager; prove least privilege, rotation,
  revocation, audit and redaction without data loss. The production-config
  policy requires external `MOUNT_RS_TIDB_TLS_URL`, `R2_ACCESS_KEY_ID` and
  `R2_SECRET_ACCESS_KEY` references and rejects inline secret strings; it does
  not prove that the secret manager or IAM policy is configured. *(Implementation
  + provider; secret manager and IAM owner are external.)*
- [ ] **W08-P03 (10%) — backup/restore/DR:** define RPO/RTO and retention;
  configure backups/versioning and complete an isolated restore, corruption and
  region-loss/recovery drill with metadata/block consistency evidence. *(Hosted
  provider; backup environment and second region are external.)*
- [ ] **W08-P04 (10%) — upgrade/compatibility/rollback:** rehearse the
  supported TiDB/RustFS/client upgrade matrix, schema/config migration,
  interrupted upgrade recovery and package/provider rollback. *(Hosted
  provider + implementation; target versions and maintenance window are open.)*
- [ ] **W08-P05 (15%) — observability/SLO/alerting:** define SLOs and error
  budgets; configure metrics/logs/traces, health checks, dashboards, paging,
  retention and redaction; exercise an alert end to end. The HTTP transport
  now implements unauthenticated `GET`/`HEAD /healthz` (listener/process
  liveness) and `/readyz` (non-empty configured drive registry, with `503`
  for an empty registry); the default `mount-rs-http` suite passed 8 unit and
  10 integration tests, and its all-features observability/OTLP suite passed 8
  unit and 13 integration tests; strict Clippy passed in both configurations.
  Responses also carry `Cache-Control: no-store` and
  `X-Content-Type-Options: nosniff`, with local regression assertions passing
  on reconciled source `2159976`.
  Hosted CI run `35599817215`, source `b26819e`, job `106333141914` also
  reached terminal success for the pre-hardening all-features gate. Terminal
  hosted CI run `35601990956`, source `f09fbe9`, job `106340034907` also
  passed the hardening-bearing all-features gate, with `2f7919a` in its
  ancestry. This is process/configuration and local/hosted exporter-path
  evidence only:
  provider-aware readiness, collector, SLO, paging, redaction and end-to-end
  alert evidence remain open. *(Implementation + hosted/provider; collector
  and on-call route are not configured.)*
- [ ] **W08-P06 (20%) — capacity/load/soak:** run representative baseline,
  peak, saturation, failover and multi-hour soak workloads; record latency,
  throughput, errors, headroom and scaling limits. The bounded public-N-API
  TiDB/RustFS soak harness in
  `tests/provider_matrix/tidb-rustfs-soak.mjs` is enabled in the hosted
  `tidb-rustfs` composition at 64 operations, concurrency 8 and 65,536-byte
  payloads, and emits p50/p95/p99/throughput markers. Run
  `35595664981`, source `c5532e3`, `tidb-rustfs` job `106319766691` passed both
  seed and reopen phases with `errors=0`; seed p50/p95/p99/throughput were
  `668.59/918.49/918.62 ms` and `11.47 ops/s`, and reopen values were
  `648.69/815.50/816.83 ms` and `12.15 ops/s`. This is bounded hosted
  qualification only; workload representativeness, resource telemetry,
  failover, multi-hour duration and production-sized capacity remain open.
  *(Hosted/provider + implementation; production gate remains unchecked.)*
- [ ] **W08-P07 (20%) — security/hardening:** enforce TLS/certificate
  rotation, network segmentation, authz/tenant isolation; complete dependency,
  image and SBOM scanning, threat-model review and security sign-off. *(Provider
  + implementation; hosted TLS compile and guard job `106311076905` in run
  `35592902494` passed, while the actual endpoint, certificates, security
  approval and network controls are external.
  `scripts/test-tidb-tls.sh` now provides the guarded credentialed provider
  gate and rejects missing TLS/CA/hostname verification before connecting;
  `scripts/verify-w08-production-config.mjs` also requires HTTPS block storage
  and TLS-required TiDB input without contacting either provider.)*
- [ ] **W08-P08 (25%) — failure drills/runbooks/on-call:** exercise client and
  provider loss, stale leases, partitions, partial writes, rolling restart and
  restore; the operator procedures and D01–D09 timed-drill matrix now live in
  [`docs/W08-operations-runbook.md`](docs/W08-operations-runbook.md). Complete
  the drills, evidence capture and on-call tabletop/acknowledgement before
  closing the gate. *(Hosted/provider + operations; named operators and
  incident tooling are open.)*
- [ ] **W08-P09 (10%) — release/canary/go-no-go:** produce immutable signed
  artifacts and SBOM, verify target-platform packages, run a staged canary with
  live SLO telemetry, rehearse rollback and record explicit approval. The
  credential-free W08.11 policy verifier and hosted job `106356402785` reject
  placeholder source identity and require explicit verified signature, SBOM and
  passed-canary states in strict mode; W08.12 wires the same verifier to the
  actual CLI preview artifact path and hosted generator policy job
  `106363893748`; W08.13 runs a real Ubuntu artifact path in dedicated
  non-cancelling hosted job `106372777281` from run `35611883547`, source
  `f432441`; W08.14 generates/verifies a real 288-component CycloneDX SBOM in
  job `106381893114` from run `35614345209`, source `9c9d0e4`. These slices do
  include W08.15's three-asset checksum pass, W08.16's Linux/macOS
  target/download matrix and W08.17–W08.24's pinned attestation wiring,
  dispatch isolation, full-pin correction, verifier identity fix and terminal
  target qualification. W08.29's current published-main run `35638433010`,
  source `9d3a6e5`, passed both target builds/downloads and both final
  provenance/SBOM attestation verifiers. W08.31's current-main-at-dispatch
  run `35641555767`, source `2bbd0266`, passed both target builds/downloads
  and both final provenance/SBOM attestation verifiers; the downloaded assets,
  checksums, manifests, 288-component SBOMs and macOS runtime were independently
  rechecked. W08.28 adds the protected
  `v*-cli-production-candidate*` workflow, which builds both targets, verifies
  final release assets and attestations, and requires the `w08-production`
  environment before publishing a prerelease. It has not been run from an
  approved tag and does not close the canary, rollback or approval gates.
  A live read-only audit on 2026-09-22 07:35 AEST found HTTP 404 for the
  `w08-production` environment and its environment-secret surface, no
  `v*-cli-production-candidate*` tag, no run for
  `w08-production-release.yml`, and only the `v0.1.0-cli-preview` prerelease;
  the protected workflow file is present on remote mainline (blob
  `9cacf0f19c8fc475df508684cf0ffcc591c96625`). This is an external hosted
  environment/tag/execution blocker, not missing repository implementation.
  A repeat read-only audit at 09:25 AEST returned the same repository,
  environment and secret-surface HTTP 404s; release/workflow API queries were
  still unavailable, no production-candidate tag was found, and the workflow
  file remained present on public `origin/main`. A fresh 10:24 AEST audit again
  returned repository/environment/secrets/release/workflow HTTP 404s, found no
  candidate tag, and confirmed the protected workflow file is present in the
  fetched mainline. The last successful release observation remains the 08:38
  AEST preview-only result. This is still an external hosted boundary, not a
  W08 implementation pass.
  A further read-only audit at 10:38 AEST returned HTTP 404 for both W08
  workflow-list queries, the `w08-production` environment and release surface;
  no candidate tag was found, while both protected workflow files remained
  present in the fetched mainline. The latest known release remains the
  preview-only observation above. This does not close P09.
  A fresh read-only audit at 11:08 AEST again returned HTTP 404 for the W08
  production workflow query and protected environment, could not resolve the
  repository release surface, and found no candidate tag. This remains an
  external API/configuration blocker and does not close P09.
  A fresh read-only audit at 11:57 AEST returned the same workflow/environment
  HTTP 404s, could not resolve the repository release surface, and found no
  candidate tag. The protected workflow file remains present in fetched
  mainline; this is still an external API/configuration blocker and does not
  close P09.
  A fresh 12:25 AEST audit returned HTTP 404 for both protected workflow
  queries and the `w08-production` environment, could not resolve the release
  surface, and found no production-candidate tag. A later branch-ref
  `git ls-remote` hit transient DNS failure, so it is not used as public-ref
  evidence; checkpoint `ea49dc65` had already passed exact public equality and
  ancestry verification at 12:24 AEST. No release, canary, rollback or
  approval evidence was created, so P09 remains externally blocked.
  *(Release implementation + hosted;
  registry, signing/attestation, deployment controller and approvers are
  external.)*

## W09 — napi-rs, Node API and packaging

- [x] Add public `createLoopback`/`resolveCapabilities` and associated types.
  Main passed pinned-oracle capability/binding/path/partial-I/O/error comparisons,
  TypeScript checks and package-content validation. This wrapper preserves caller
  driver/handle identity and does not own the caller's shutdown lifecycle.
- [x] Accept structural JavaScript drivers in mount/server factories and mixed
  S3 bucket maps, with owned-adapter cleanup and TypeScript declarations.
  Main independently passed the pinned-oracle structural factory suite and
  eight WebDAV DELETE status/survivor comparisons, plus 11 Rust WebDAV tests.
  Native structural mounts remain unverified; harness exports and remaining
  public API gaps are still open. These results do not qualify remote backends.
- [x] Migrate npm packages to `@mount-rs/core` and `@mount-rs/virtual-fs`,
  including loaders, dependencies, imports and distribution checks. No registry
  publication or namespace-ownership verification is implied.
- [x] Harden the scoped `@mount-rs/core` distribution metadata and generated
  platform-package loaders (`4fa908e`); aggregate, pack, loader and consumer
  checks pass. Publication and native artifact qualification remain open.
- [x] Land shared cancellation-safe shutdown, provider-reference release ordering,
  panic/error retry and Windows numeric flags. Main passed 12 native binding
  tests, strict Clippy, rebuilt-addon/oracle-enabled Node suite, consumer tests
  and TypeScript checks. R2/PGlite/native-mount opt-ins were skipped in this
  package run; hosted Windows verification remains open. Independent handles,
  servers and in-flight operations must finish before backing-file deletion.
- [x] Land JavaScript driver bridge, codec/server hooks and 9P ESM exports.
- [x] Fix shutdown test's early rejection handling (`8d1f4ad`); 25 strict repeats
  passed locally. This corrects the test race, not a proven runtime defect.
- [x] Add server-test phase/cleanup diagnostics (`2452463`); 20 strict main
  repeats passed. Earlier intermittent timeout cause is not established.
- [x] Full local Node suite passed after fixes, excluding opt-in service/native
  lanes; opt-in skips are not acceptance evidence.
- [x] `57632c6` + `e213856` add structural-driver adapter and opt-in native
  lifecycle acceptance; read/callback/unmount cleanup passed on macOS NFS.
  Structural native write support remains explicitly bounded by missing decoded
  `open_flags`.
- [x] `3042d09` exposes Rust-backed FUSE inode state through the N-API package;
  the Rust inode table, Node parity test, generated declarations and distribution
  checks passed locally.
- [ ] W09.1 Close remaining public exports, factories and declaration parity gaps.
- [ ] W09.2 Investigate any repeated server timeout using the new diagnostics.
- [ ] W09.3 Expose new providers, versioning, VFS and HTTP through tested APIs.
- [ ] W09.4 Verify platform artifacts, package install and native loading on both
  operating systems; keep package publication behind D06.

## W10 — Filesystem and protocol transports

- [x] Land FUSE, NFS, 9P, WebDAV and S3 transport implementations and tests.
- [x] `1af1986` + `8ac77f3` + `9215ba3` retain held NFSv3/v4 backend handles
  across unlink/rename; 266 pinned oracle cases passed through the supported
  Vitest runner, with the TypeScript control's stateless handle limitation kept
  explicit.
- [x] Land HTTP early-rejection regression (`aa44413`).
- [x] Land incremental S3 chunked codec helpers (`e218d18`); all 27 S3 tests and
  strict Clippy passed locally.
- [x] The W01-S3 peer-fault packet preserves the current loopback-only S3
  boundary while adding bounded drain timeout, live accepted-TCP connection
  accounting, peer-aware `Connection`/`Server` transport hooks, and a
  reset-on-close gateway fault. The locked S3 target passed 4 unit, 6
  chunked, 17 gateway, and 5 public-API tests with host loopback permission.
- [x] The W01-S3 N-API streaming packet exposes `S3Server.session`, buffered
  and incremental `S3Session` request/response methods, async-iterable and
  `ReadableStream` request bodies, response cancellation, generator-failure
  mapping, and async metrics snapshots. The release N-API binding loaded
  directly from the locked Rust build, and the host-enabled N-API server
  integration passed streamed PUT/GET, bucket isolation, cancellation, and
  metrics-delta checks. The same packet now verifies safe effective session
  options, session-owned bucket wrappers, debug-gated assertions, live
  `connections`, typed `onTransportError`, and one direct Node peer-reset
  callback. The generated `pnpm build`, package typecheck, strict TypeScript
  fixture check, 4/6/18/5 S3 Rust target, 16/16 N-API library tests, and
  warning-denied Clippy passed. Complete oracle-specific member/codec parity,
  live AWS/R2, and broader restart/durability/concurrency/native gates remain
  open; W01-S3 remains **NO-GO**.
- [x] The subsequent W01-S3 local ordering packet added bounded direct-session
  unique-object PUT/GET concurrency and same-key conditional PUT CAS coverage:
  the deterministic Rust paused-write race produced exactly one `200` and one
  `412`, while the rebuilt N-API probe passed 64 concurrent PUT/GET records,
  two concurrent buffered `If-Match` PUTs with one winner, a streamed
  conditional update, exact final bytes, and clean counters/assertions. The
  packet was published fast-forward-only at `9e98f14a`; the current provider
  admission rows remain explicitly blocked (`missing_bucket` for AWS and
  `count=281 limit=20` for R2), so W01-S3 remains **NO-GO**.
- [x] The latest W01-S3 qualification refresh at `71972b28ca7ae561325342ddf466c8353546547a`
  passed the release N-API build, all four pinned upstream oracle files
  (990 passed/79 skipped), the Rust S3 4/6/29/5 packet and strict Clippy, exact
  Node scope/session/concurrency/restart checks, structural factory parity,
  package typecheck/distribution/server smoke, and the 40-case S3+WebDAV HTTP
  differential. These are current local and pinned-oracle gates only; live
  AWS/R2, physical power-loss durability, broader workload bounds, and
  native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The subsequent W01-S3 option/callback packet added Rust `S3SessionHooks`
  and N-API `now`, `requestId`, `onError`, and `onAssertion` controls with
  close-safe callback ownership. The release addon/declaration build,
  callback observability test, Rust 5/6/29/5 packet, warning-denied Clippy,
  package typecheck/distribution, session differential, 64-way/CAS
  concurrency, and process-restart recovery passed. Current AWS remains
  blocked by `missing_bucket` and current R2 by `count=285 limit=20`; physical
  power-loss durability, broader workload bounds, and native/hosted acceptance
  remain open, so W01-S3 stays **NO-GO**.
- [x] The next W01-S3 callback packet forwards per-key driver failures from
  `DeleteObjects` to `S3SessionHooks.on_error` without changing its 200
  partial-result response. The focused regression and full Rust 5/6/30/5
  packet, warning-denied Clippy, release N-API build, callback observability,
  session differential, 64-way/CAS concurrency, process-restart recovery,
  typecheck, and distribution checks passed. AWS run `35683716247` remains
  blocked by `missing_bucket` and R2 run `35683716251` by `count=294 limit=20`;
  physical power-loss durability, broader workload bounds, and native/hosted
  acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The published S3 packet's automatic provider runs were refreshed:
  AWS run `35684677320` stopped at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`,
  while R2 run `35684677273` stopped at `count=295 limit=20` before live
  admission. No service PASS is claimable; protected AWS configuration, the
  R2 budget reset, physical power-loss durability, broader workload bounds,
  and native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The next W01-S3 lifecycle packet makes `S3Session.close()` mirror the
  oracle's non-rejecting cleanup contract: per-driver sweep failures reach
  `onError(error, undefined)` and later cleanup remains eligible. The focused
  regression and full Rust 5/6/31/5 packet, warning-denied Clippy, release
  N-API build, callback observability, session differential, 64-way/CAS
  concurrency, process restart, typecheck, and distribution checks passed.
  AWS `35684677320` remains blocked by `missing_bucket` and R2 `35684677273`
  by `count=295 limit=20`; physical power-loss durability, broader workload
  bounds, and native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The automatic provider runs for published packet `5e910e80` were refreshed:
  AWS `35686512809` stopped at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, while
  R2 `35686512842` stopped at `count=302 limit=20` before live admission. No
  service PASS is claimable; protected AWS configuration, the R2 budget reset,
  physical power-loss durability, broader workload bounds, and native/hosted
  acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The next W01-S3 server-lifecycle packet makes bounded close match the
  oracle's drain-deadline behavior: tracked connections are cancelled and the
  detached Axum task is aborted instead of returning a timeout while retaining
  a live connection. The focused stalled-response regression and full Rust
  5/6/32/5 packet, strict Clippy, release addon, host server integration,
  callback/session/concurrency/restart/typecheck/distribution checks passed;
  live providers, power-loss durability, broader workload bounds, and
  native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The next W01-S3 connection-fault packet closes the short streamed-
  response framing gap: the HTTP boundary counts streamed bytes against
  `Content-Length`, reports one bounded `out of frame` transport error, and
  terminates the damaged keep-alive connection with `UnexpectedEof`. The
  oracle-derived regression and full Rust 5/6/33/5 packet, strict Clippy,
  release addon, isolated S3 N-API server phase, callback/session/concurrency/
  restart/typecheck/distribution checks passed; live providers, power-loss
  durability, broader workload bounds, and native/hosted acceptance remain
  open, so W01-S3 stays **NO-GO**.
- [x] The next W01-S3 wire-protocol packet adds the oracle-derived HTTP/1.1
  pipelining regression: two complete GET requests sent in one TCP write return
  two ordered `200` responses on one connection. The focused test and full Rust
  5/6/34/5 packet, strict Clippy, formatting, and diff checks passed; this is
  bounded local wire evidence only, so live providers, power-loss durability,
  broader workload bounds, and native/hosted acceptance remain open and
  W01-S3 stays **NO-GO**.
- [x] The next W01-S3 framing packet adds the oracle-derived `Expect:
  100-continue` regression: the gateway emits the interim response, accepts the
  delayed content-length body, returns `200`, and persists exact bytes. The
  focused test and full Rust 5/6/35/5 packet, strict Clippy, formatting, and
  diff checks passed; this is bounded local HTTP/1.1 evidence only, so live
  providers, power-loss durability, broader workload bounds, and native/hosted
  acceptance remain open and W01-S3 stays **NO-GO**.
- [x] The next W01-S3 framing packet adds oracle-derived HEAD and
  `Transfer-Encoding: chunked` regressions: HEAD preserves `Content-Length`
  with no body, while a transfer-encoded PUT returns `411
  MissingContentLength`. The focused HTTP subset and full Rust 5/6/37/5 packet,
  strict Clippy, formatting, and diff checks passed; this is bounded local
  HTTP/1.1 evidence only, so live providers, power-loss durability, broader
  workload bounds, and native/hosted acceptance remain open and W01-S3 stays
  **NO-GO**.
- [x] The automatic provider runs for published packet `a861032f` were
  refreshed: AWS run `35691926924` stopped at
  `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, while R2 run `35691926938`
  stopped at `R2 CI monthly run cap already exceeded: count=323` before live
  admission. No service PASS is claimable; protected AWS configuration, the R2
  budget reset, physical power-loss durability, broader workload bounds, and
  native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The current mainline rejected-request-body fix is now directly covered:
  `http_server_drains_rejected_body_before_reusing_connection` sends a
  body-bearing rejected PUT followed immediately by a GET and proves the
  second `200` response remains ordered and readable. The complete current
  Rust 5/6/38/5 packet, strict Clippy, formatting, and diff checks passed; this
  is bounded local keep-alive recovery only, so live providers, power-loss
  durability, broader workload bounds, and native/hosted acceptance remain
  open and W01-S3 stays **NO-GO**.
- [x] The next W01-S3 streaming packet adds the oracle-derived abandoned-
  download regression `http_server_closes_abandoned_download_handle`: after
  the first response bytes, the client disconnects while the driver read is
  parked, the opened handle closes exactly once, and a fresh ranged GET returns
  `206`. The complete current Rust 5/6/39/5 packet, strict Clippy, formatting,
  and diff checks passed; this is bounded local cancellation cleanup only, so
  live providers, power-loss durability, broader workload bounds, and
  native/hosted acceptance remain open and W01-S3 stays **NO-GO**.
- [x] The automatic provider runs for published packet `09eca17b` were
  refreshed: AWS run `35693316456` stopped at
  `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, while R2 run `35693316400`
  stopped at `R2 CI monthly run cap already exceeded: count=326` before live
  admission. No service PASS is claimable; protected AWS configuration, the R2
  budget reset, physical power-loss durability, broader workload bounds, and
  native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The next W01-S3 lifecycle packet adds the oracle-derived positive
  `S3Server.close()` drain regression `http_server_close_allows_inflight_response_to_finish`:
  a parked 2 MiB response resumes and returns every byte while bounded close
  completes; the timeout-abort case remains separately covered. The complete
  current Rust 5/6/40/5 packet, strict Clippy, formatting, and diff checks
  passed; this is bounded local graceful-close evidence only, so live providers,
  power-loss durability, broader workload bounds, and native/hosted acceptance
  remain open and W01-S3 stays **NO-GO**.
- [x] Re-qualified the current release N-API S3 runtime on published mainline
  `76eb2914`: the fresh release addon build, callback observability, 64-way/CAS
  concurrency, session differential, exact 155/258/153 barrel scope, process
  restart, generated typecheck, distribution, and isolated S3 server phase all
  passed. This refreshes local Rust/N-API evidence only; live providers,
  power-loss durability, broader workload bounds, and native/hosted acceptance
  remain open and W01-S3 stays **NO-GO**.
- [x] Added the oracle-derived pre-body disconnect regression
  `http_server_closes_download_handle_before_first_body_chunk`: a delayed
  driver open is parked after opening the object, the client disconnects before
  any response body chunk, and the released handle closes exactly once. The
  complete current Rust 5/6/41/5 packet, strict Clippy, formatting/diff checks,
  and isolated S3 N-API server integration passed; the initial sandbox socket
  bind `PermissionDenied` was classified and the host-enabled rerun passed.
  This is bounded local pre-body cancellation evidence only; live providers,
  power-loss durability, broader workload bounds, and native/hosted acceptance
  remain open and W01-S3 stays **NO-GO**.
- [x] Qualified streamed driver-read fault recovery with the oracle-aligned
  `http_server_aborts_driver_read_error_without_reusing_connection`: an
  injected `EIO` on the first streamed GET read terminates the damaged response,
  emits exactly one `S3 response body stream failed` transport report, and a
  fresh GET returns exact bytes. The complete current Rust 5/6/42/5 packet,
  strict Clippy, formatting/diff checks, and isolated S3 N-API server
  integration passed. This is bounded local streamed fault evidence only; live
  providers, power-loss durability, broader workload bounds, and native/hosted
  acceptance remain open and W01-S3 stays **NO-GO**.
- [x] Qualified real HTTP aborted-upload cleanup with
  `real_http_aborted_upload_removes_staging_and_object`: after a partial staged
  PUT is visible, the client sends a TCP reset; the private `.mountx-put-*`
  entry is reaped within the bounded wait and the destination is not published.
  The complete current Rust 5/6/43/5 packet, strict Clippy, formatting/diff
  checks, and isolated S3 N-API server integration passed. This is bounded
  local request-cancellation staging evidence only; live providers, power-loss
  durability, broader workload bounds, and native/hosted acceptance remain open
  and W01-S3 stays **NO-GO**.
- [x] Qualified real HTTP aborted multipart-part replacement cleanup with
  `real_http_aborted_multipart_part_preserves_existing_part`: after an existing
  multipart part is committed, a replacement part is partially staged and the
  client sends a TCP reset; the private `.part-*` staging entry is reaped within
  the bounded wait and the original part remains byte-for-byte intact. The
  complete current Rust 5/6/44/5 packet, strict Clippy, formatting/diff checks,
  and isolated S3 N-API server integration passed. This is bounded local
  multipart request-cancellation replacement evidence only; live providers,
  power-loss durability, broader workload bounds, and native/hosted acceptance
  remain open and W01-S3 stays **NO-GO**.
- [x] Qualified provider-backed S3 network concurrency and the staging-scan
  race with `s3-provider-network-concurrency.mjs`: the NodeFs/SQLite matrix
  initially reproduced transient `NoSuchKey` responses when concurrent
  streaming PUTs renamed private staging files between `readdir` and `stat`;
  S3 quota/reaper scans now ignore only vanished `ENOENT`/`ENOTDIR` entries while
  preserving other errors. The final matrix passed 32 concurrent buffered
  PUT/GET pairs plus content-length streamed PUT/GET for both providers with
  zero session errors. The current Rust 5/6/44/5 packet, strict Clippy,
  formatting/diff checks, release N-API build, pinned S3 session differential,
  exact barrel scope, 64-way/CAS concurrency, process restart, callback
  observability, typecheck/distribution, and isolated S3 N-API server
  integration passed. This closes bounded local NodeFs/SQLite provider
  concurrency and transient staging-scan race evidence only; live providers,
  power-loss durability, broader workload bounds, and native/hosted acceptance
  remain open and W01-S3 stays **NO-GO**.
- [x] Qualified in-flight streamed PUT crash/restart recovery with
  `s3-inflight-crash.mjs`: a child process is forced down after writing a
  private streaming prefix, and replacement NodeFs and SQLite sessions keep
  the destination unpublished, accept a fresh streamed PUT with exact bytes,
  and reap the orphan `.mountx-put-*` entry after the effective session TTL
  through the existing `now` hook. This is local provider/process-crash and
  staging-TTL evidence only; physical power-loss/torn-write ordering, live
  providers, hosted/native lifecycle, and broader workload bounds remain open,
  so W01-S3 stays **NO-GO**.
- [x] Added the bounded provider-failure regression
  `streaming_publish_rename_failure_removes_staging_and_preserves_object`:
  one injected `rename(EIO)` during streamed replacement must return 500,
  remove private `.mountx-put-*` staging, and preserve the prior object bytes.
  The focused shared-target gateway target passed 45/45, with formatting,
  diff checks, and strict warning-denied Clippy also passing. This is bounded
  local provider-failure evidence only; live AWS/R2 failure handling, physical
  power-loss/torn-write durability, hosted/native lifecycle, and broader
  workload bounds remain open, so W01-S3 stays **NO-GO**.
- [x] The automatic provider runs for published packet `fcf1d547` were
  refreshed: AWS run `35693941024` stopped at
  `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, while R2 run `35693941037`
  stopped at `R2 CI monthly run cap already exceeded: count=329` before live
  admission. No service PASS is claimable; protected AWS configuration, the R2
  budget reset, physical power-loss durability, broader workload bounds, and
  native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The automatic provider runs for published packet `d4f43b28` were
  refreshed: AWS run `35692509801` stopped at
  `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, while R2 run `35692509869`
  stopped at `R2 CI monthly run cap already exceeded: count=325` before live
  admission. No service PASS is claimable; protected AWS configuration, the R2
  budget reset, physical power-loss durability, broader workload bounds, and
  native/hosted acceptance remain open, so W01-S3 stays **NO-GO**.
- [x] The automatic provider runs for published packet `4f9120a2` were
  refreshed: AWS run `35690334807` stopped at
  `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, while R2 run `35690334795`
  stopped at `R2 CI monthly run cap already exceeded: count=317 limit=20`
  before live admission. No service PASS is claimable; protected AWS
  configuration, the R2 budget reset, physical power-loss durability, broader
  workload bounds, and native/hosted acceptance remain open, so W01-S3 stays
  **NO-GO**.
- [x] The automatic provider runs for published packet `f607d445` were
  refreshed: AWS run `35688895917` stopped at
  `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, while R2 run `35688895958`
  stopped at `R2 CI monthly run cap already exceeded: count=313 limit=20`
  before live admission. No service PASS is claimable; protected AWS
  configuration, the R2 budget reset, physical power-loss durability, broader
  workload bounds, and native/hosted acceptance remain open, so W01-S3 stays
  **NO-GO**.
- [ ] W10.1 Finish per-transport backend/platform acceptance matrix, including
  native lifecycle, disconnect/error behavior and streaming/backpressure.
- [ ] W10.2 Verify transport auto-selection and explicit unsupported behavior.
- [ ] W10.3 Ensure all protocol paths operate on the same configured drive state.

## W11 — Config-driven CLI

- [x] Land strict configuration and CLI (`f20f1e3`).
- [x] Land actual macOS NFS host-backed mount/IO/unmount lifecycle and CI gate
  (`c4058f2`); main native test and strict Clippy passed.
- [x] W11.1 Review SQLite-backed native lifecycle and UID/GID config; main's
  33 unit + seven integration tests and formatting passed. Landing with this update.
- [x] W11.2 Fix review finding: explicit ownership mismatch on read-only config
  must not mutate persisted root ownership.
- [x] W11.3 Fix review finding: an existing `0:0` root is not proof of a new
  filesystem. Preserve existing ownership; avoid check-then-create races.
- [x] W11.4 Main ran actual macOS config→NFS mount→SQLite workload→unmount→
  fresh-process reopen: PASS, SQLite 3.51.0, DELETE journal, synchronous FULL.
  Process locking/recovery passed; mount-service crash was not tested here.
- [ ] W11.5 Extend configuration and lifecycle coverage to all new providers,
  version views, FSKit and multi-drive HTTP without silently falling back.
- [x] Main passed the checked-in HTTP config as a real local demo after
  `4af2a30`: memory and split SQLite drives served, bearer isolation returned
  401, and split SQLite data survived a service restart. This is local
  evidence only and does not qualify live R2 or native-mount acceptance.
- [x] `d526c18` extends the HTTP subprocess contract with streamed writes,
  ranges, truncate, concurrent writes, aborted-write recovery and listener
  cleanup; the focused CLI/HTTP tests and strict Clippy passed locally.
- [x] `0d7f1f4` adds a bounded native demo using the Rust CLI, independent Rust
  and Node clients, real macOS NFS, SIGINT unmount and durable backing-byte
  verification.
- [x] `0dca1d1` adds the Node SDK CLI example and default mount-free checks;
  the opt-in macOS NFS self-test passed against the rebuilt local N-API addon.
- [x] `29337f7` adds direct Node SDK read/write self-test coverage and routes the
  Rust CLI through `mount-rs-sdk`; both CLI entry points are now usable examples
  of the public SDKs and are included in the provider/consumer matrix.
- [x] `946a7cb` fixes the Windows Node CLI integration test to use the platform
  temp directory; the local check passes and the hosted Windows rerun is queued.
- [x] `21803fd` adds the Node CLI's versioned provider config, SDK-only reopen
  self-test and PGlite integration-matrix row. The PGlite config-to-SDK path
  passed through clean shutdown/reopen; memory is correctly rejected for
  `--reopen` because it is process-local.
- [x] `fd5eb04` makes the Node structural-driver lifecycle exercise real
  mounted writes and readback on the authorized macOS NFS path.
- [ ] W11.6 Run the same SDK-backed CLI flow against the configured metadata/
  block providers, including restart and cleanup, before treating the demo as a
  provider-integrated acceptance path. The local PGlite path is covered; live
  R2, native configured-provider mounts, and hosted platform runs remain open.

## W12 — Safely host SQLite database files

- [x] Record prior Linux FUSE DELETE/WAL and macOS NFS single-host DELETE
  evidence; those configurations alone do not establish universal safety.
- [ ] W12.1 Define and test supported journal/locking modes per transport.
  The local VFS checkpoint covers the required rollback matrix for
  DELETE/TRUNCATE/PERSIST × NORMAL/FULL/EXTRA and explicit HostLocal/ProcessLocal
  WAL cases; MEMORY/OFF, all transports and hosted platform coverage remain open.
- [x] W12.2 Prevent silent WAL fallback from being reported as WAL success.
  `5d9e513` asserts effective WAL for supported HostLocal scope and rejects
  unsupported WAL requests instead of silently falling back.
- [ ] W12.3 Exercise multiple connections/processes, readers/writers, locks,
  sync barriers, rename/unlink, disk-full errors and crash/restart integrity.
  The checkpoint covers process/child-process locking, reader/writer snapshots,
  sync-fault recovery, checkpoint/reopen and integrity; rename/unlink,
  disk-full and hosted crash coverage remain open.
- [ ] W12.4 Run integrity checks and acknowledged-commit recovery across every
  supported metadata/block combination and operating system.
- [x] W12.5 Explicitly reject unsupported safety modes and document constraints
  for WAL capability scope in `docs/sqlite-vfs-wal-plan.md` (`5d9e513`).
- [ ] W12.6 Run the new Linux CLI SQLite-backed FUSE SIGKILL/reopen test in
  hosted CI. Implementation and bounded cleanup are added; main independently
  passed both shared macOS NFS lifecycle regressions, formatting and Clippy.
  Linux execution is not yet verified on this macOS host.
- [x] `d6b80f4` adds local SQLite VFS fail-closed behavior after injected block or
  metadata-publication failures, plus the focused unit/engine/bridge gates. This
  is a local reliability boundary, not hosted cross-platform acceptance.

## W13 — FSKit

- [x] Land unsigned SDK compile target (`5993984`); main arm64 compile passed,
  worker reported x64 compile. Neither is a mounted-filesystem result.
- [x] W13.1 Implement FSVolume operations/read-write and tested Swift/Rust IPC
  (`95aca9c`); Rust bridge tests, Swift frame tests, XPC lifecycle tests, and
  unsigned arm64 Xcode targets passed locally.
- [ ] W13.2 Verify errors, handles, identity, concurrency and lifecycle at the seam.
  The FSKit path-resource bridge now maps macOS 26+ `FSPathURLResource` values
  to rooted `HostFs` workers and its direct host-path lifecycle/reopen test
  passes; installed-volume concurrency and cross-mount visibility remain open.
- [ ] W13.3 Complete packaging, entitlements and signing plan, then request D03.
  A minimal `MountRsHost` containing app now embeds the FSKit appex in
  `Contents/Extensions` and builds unsigned; Apple team/profile authorization
  and activation remain open.
- [x] `fdd7c81` adds the strict macOS activation gate and publishes it through `ea9ac68`: Rust/Swift/Xcode/embedded-bundle checks pass, while the real activation probe correctly reports `FSKIT_ACTIVATION=FAIL` because the host has zero valid Apple signing identities, only ad-hoc signing, and no installed/enabled `mount-rs` FSClient entry. This proves the blocker and does not close W13.4.
- [ ] W13.4 Activate and test real FSKit mounts, CLI integration, persistence and
  supported SQLite workloads. NFS/FUSE fallback does not satisfy this stream.

## W14 — Versioned filesystems

- [x] Land additive core versioning contract, separate coordinator crate and
  memory/SQLite history providers. Includes explicit publication identities,
  lost-ack fork reconciliation, pinned read-only views, restore, cross-store
  copying, schema/head validation and cancellation-safe pin accounting.
  Main tested the exact staged checkout: 6 memory, 10 SQLite and 20 versioning
  tests passed. This is not remote/Node/CLI/platform acceptance or physical COW;
  dropped views may retain pins until TTL unless explicitly closed.

- [x] Record versioning design (`dc6b0bb`).
- [x] Local uncommitted prototype passed four tests covering history, restore,
  fork, pins/retention, SQLite reopen and operation replay after head moves.
- [ ] W14.1 Review and land version IDs, publication, pinned views, history,
  restore/fork, retention and explicit provider capabilities.
- [ ] W14.2 Fix pin lifetime: an open historical handle must retain protection or
  be invalidated when its view closes; do not allow reads after protection lapses.
- [ ] W14.3 Validate expiry before/after reads and during slow in-flight reads;
  protect immutable public records and test close/read/delete races.
  Worker checkpoint reports pin-lifetime fixes, seven versioning tests and
  strict Clippy passing; main review and independent verification remain open.
- [ ] W14.4 Verify replay payload matching, CAS conflicts, cross-store forks,
  crash/reopen, block reachability and safe retention/GC boundaries.
- [ ] W14.5 Add real remote-provider, Node and CLI coverage; clearly label
  unsupported capabilities and database-quiescence requirements.
- [ ] W14.6 Keep full-copy snapshots distinct from future physical COW (W23).

## W15 — Mount-free SQLite VFS

- [x] Extend the same remote VFS fixture through a graceful PGlite process
  stop/start using its persisted data directory and a fresh listening port.
  Main's full local harness passed exact ledger and integrity checks after
  both RustFS and PGlite restarts, with fresh VFS test processes at each phase.
  Clean shutdown status is required. Abrupt process loss, power loss, WAL and
  hosted results remain separate unverified gates.
- [x] Actual RustFS service stop/start now has its own VFS fixture: separate
  prepare/reopen test processes share the persisted metadata/block prefix,
  verify two exact committed binary rows, exclusion of a rolled-back row,
  and `integrity_check = ok`. Full local RustFS harness passed both phases.
  PGlite itself was not restarted; this is not a power-loss or WAL result.
- [x] Live PGlite metadata + RustFS blocks VFS test passed locally: real SQLite
  transaction, exact binary bytes and integrity after reconnecting both clients.
  Added it to the standard RustFS harness instead of leaving it opt-in only.
  The surrounding RustFS suite also passed its service-restart check, but that
  fixture is separate: VFS-specific service restart and PGlite restart remain
  unverified. No remote power-loss durability claim is made.
- [x] Root-integrate the separate rollback-journal VFS and metadata/block
  storage bridge. Main passed 22 local tests and strict all-feature Clippy;
  the remote PGlite/RustFS test remains ignored in this run. Host-backed engine
  tests cover DELETE/TRUNCATE/PERSIST x NORMAL/FULL/EXTRA with exact binary
  ledgers and reopen checks. Memory/SQLite storage bridges have separate engine,
  fencing, failure and subprocess checks. The follow-up now adds all nine
  rollback journal/synchronous combinations to each memory and durable SQLite
  provider pair (18 cells), with exact committed/rolled-back bytes and reopen.
  WAL is explicitly rejected and remains unimplemented; Windows qualification,
  remote recovery, Node exposure, and broader crash/fault coverage stay open.
- [x] W15.1 Complete the separate draft crate and actual mount-rs storage bridge
  (`5d9e513`); host-file and StorageBackend implementations are both tested.
- [x] W15.2 Fix registration lifetime escapes through connection extraction or
  mutable access; test duplicate names and independently opened connections.
  Explicit quiescent close releases provider resources, rejects active callbacks
  and file handles, and retains backend-free callback tombstones. Main added
  extracted-connection and closed-wrapper/name-reuse regressions and passed
  28 local tests. Reentrant backend destruction runs outside the registry lock.
  Two remote tests remain opt-in; these local results are not WAL acceptance.
- [x] W15.3 Replace noop-waker/busy polling with a valid executor contract and
  test a real wake path (`5d9e513`); no hidden busy/noop-waker success path is
  used by the process-local bridge.
- [x] W15.4 Implement cross-connection SQLite locking or explicit safe
  serialization with fencing for the supported HostLocal/ProcessLocal scopes;
  child-process contention and stale-reader promotion tests pass in `5d9e513`.
- [ ] W15.5 Test stale-reader promotion, separate processes, lost updates,
  short reads, sync failures, journal recovery and database integrity. The
  checkpoint covers stale-reader promotion, separate processes, sync failures,
  journal recovery and integrity; lost-update/short-read and broader hosted
  crash coverage remain open.
- [ ] W15.6 Expose and integrate through Rust and Node across storage engines
  without requiring a native mount; document supported journal modes.

## W16 — Mount-independent consumer adapters

- [x] W16.1 Implement separate just-bash and Mastra adapters over mount-rs drives.
- [x] W16.2 Main independently passed actual just-bash 3.4.2 and Mastra 1.67.0
  tests, strict TypeScript checking and package dry-run (11 intended files).
  Memory and SQLite reopen/shared Node API visibility are covered. Wired into
  test-all and four-platform Node CI; hosted results remain pending.
- [ ] W16.3 Verify binary/path/error behavior, readonly/versioned views, shared
  namespace visibility, persistence and lifecycle in consumer integration tests.
- [ ] W16.4 Reuse the drive abstraction with W17 without requiring OS mounts.
- [x] W16.5 Fix review findings before landing: reject recursive copy into a
  descendant (including symlink aliases), preserve source on same-object copy,
  and handle partial/zero-progress writes. Add targeted regressions.
- [x] W16.6 Document that waiting for admitted operations does not establish
  bounded cleanup if a backend request never resolves.
- [ ] W16.7 Add explicit backend cancellation/timeout semantics and remote-store,
  versioned-view and actual native-mount shared-visibility acceptance. The new
  `tests/native_shared_visibility.rs` records the macOS capability boundary
  without claiming that acceptance: the current FUSE API is Linux-only and the
  FSKit target has no signed/activated mounted-volume host. The real three-way
  test remains open until all three independent mount authorities and their
  shared-provider locking contract exist.

## W17 — Multi-drive HTTP server

- [x] Root-integrate the separate HTTP crate and shared lockfile. Real loopback
  clients exercise isolated memory/SQLite drives, bounded requests, streaming
  disconnect cleanup, and shutdown retries. Main passed 7 unit and 6 integration
  tests locally. Listener completion is cached under one mutex, with a
  deterministic cancellation-after-join regression. Hosted, Node/CLI, remote
  backend, and distributed-cache acceptance remain open.
- [x] W17.1 Implement a separately packaged server and drive registry/config;
  expose several named drives, including unmounted drives.
  CLI `serve-http --config` reuses provider factories with per-drive env-token
  references. Main tested the exact staged snapshot: 38 unit, 7 CLI, 2 real
  HTTP subprocess and 1 native-artifact tests passed; 2 native-mount tests
  remained opt-in/ignored. Tests cover memory/SQLite/split-store isolation,
  forced-process SQLite reopen, volatile-store loss, and Unix SIGINT shutdown.
  Portable HTTP coverage now runs on Windows CI; no Windows runtime pass yet.
- [x] Add mandatory real CLI HTTP gate to the RustFS harness: PGlite metadata
  and SQLite metadata each use actual RustFS chunks. Main's full harness run
  exited 0 with binary multi-chunk writes, full/range reads, per-drive auth,
  graceful shutdown and fresh-process durable reopen with distinct default
  owners. Existing VFS/RustFS/PGlite restart checks also passed. The live
  Cloudflare R2 CLI gate now passes; Windows remote execution and abrupt CLI
  crash recovery remain open.
- [ ] W17.2 Define discovery, routing, filesystem operations, streaming/ranges,
  stable errors and lifecycle; share the actual native/API drive namespace.
- [ ] W17.3 Add per-drive authorization/isolation, limits and deployment/TLS guidance.
- [ ] W17.4 Test multiple drives and mixed stores through real HTTP clients,
  including concurrency, restart, failures and Node/CLI configuration. The
  local memory/split-store subprocess now covers concurrency, range reads,
  truncate, aborted writes and process restart (`d526c18`); remote, Node and
  crash-recovery coverage remain open.
- [ ] W17.5 Define a cache integration boundary, but do not implement/select the
  distributed cache until the W22 discussion and primary acceptance.
- [x] `4af2a30` records a runnable cross-platform local demo using independent
  SQLite metadata/blocks, live PUT/GET, authorization isolation and process-
  reopen persistence. Cloud/TLS/deployment acceptance remains open.

## W18 — Benchmarks, allocations and dependencies

- [x] Land canonical dispatch benchmark (`2aecccf`) and forwarding allocation
  cleanup (`a84faf7`); cleanup is not evidence of a measured performance win.
- [ ] W18.1 Align workloads with ComputeSDK storage benchmarks, including
  representative 1/4/10/16 MiB data sizes and small-file/metadata workloads.
- [ ] W18.2 Measure direct Rust, Node and native paths across required stores;
  report cold/warm behavior, latency, throughput, memory and service settings.
- [ ] W18.3 Produce controlled before/after dispatch/allocation results.
- [ ] W18.4 Add license-reviewed ArtifactFS comparisons and explain semantic
  differences rather than presenting incomparable throughput as equivalent.
- [ ] W18.5 Audit dependency count/features per crate; justify runtime/codec/SDK
  additions and keep integrations out of the core dependency surface.
- [ ] W18.6 Review the untracked duplicate dispatch draft against the canonical
  benchmark before any cleanup; preserve unrelated working-tree files.
- [x] Live Cloudflare R2 smoke benchmark passed on 2026-09-20 through the public
  N-API split-store path with PGlite metadata, R2 blocks, fixed 64 KiB chunks,
  verified full-byte readback, delete and remote-prefix cleanup. This is one
  macOS smoke measurement, not completion of the full matrix.

## W19 — Compression evaluation

- [x] Record [compression design review](docs/compression-design.md) (`642e81a`):
  initial recommendation is chunk first, then independently compress blocks
  through an optional wrapper. This is a proposal, not implemented compression.
- [ ] W19.1 Benchmark raw, zstd levels 1/3/6, LZ4 and justified alternatives over
  representative data and chunk sizes; compare pre/post-chunking layouts.
- [ ] W19.2 Measure ratio, CPU, memory, random access, rewrites, deduplication,
  network cost and effects on future chunkers/version diffs.
- [ ] W19.3 Specify codec/version metadata, dictionaries, corruption handling,
  decompression bounds and compatibility before choosing implementation scope.

## W20 — CI, packaging and final acceptance

- [ ] Windows pinned-oracle N-API parity and the portable HTTP tests are now in
  the `windows-node` job (`d526c18`); hosted execution and Windows Rust/host
  runtime qualification remain required.
- [ ] W20.1 Obtain revision-matched green required hosted macOS/Linux jobs.
  Latest jobs were queued/in progress at this update; earlier Node/PGlite
  failures are not closed by local fixes alone.
- [ ] W20.2 Verify new RustFS and CLI native gates actually execute and pass.
  Main reran all three macOS CLI native tests after collision-resistant paths
  and panic-safe cleanup: passed, including actual SQLite DELETE/FULL reopen.
  Linux crash harness now serializes native cases and tries regular unmount
  before lazy fallback; its previously failed hosted gate remains unverified.
- [ ] W20.3 Add real FoundationDB/TiDB, VFS, versioning, adapters and HTTP gates.
- [ ] W20.4 Run locked build/tests, formatting and strict Clippy on final changes;
  retain logs and clearly distinguish ignored, skipped and credential-gated lanes.
- [ ] W20.5 Validate install/package artifacts, license consistency and dependency
  inventory; document supported and explicitly unsupported configurations.
- [ ] W20.6 Audit every task and requirement against code plus integration
  evidence before marking the goal complete. Do not claim “100%” from test count.

## W21 — Reference review and learnings

Canonical source links and license considerations live in [REFERENCES.md](REFERENCES.md).
For each review, record applicable lessons, rejected ideas and resulting task IDs;
listing a source does not mean it has been reviewed or its code can be reused.

- [ ] W21.1 Close agentfs and Archil architecture/storage review.
- [ ] W21.2 Close Tensorlake repository/TLFS and changed-byte Firecracker snapshot
  review; feed version/diff implications into W14/W23.
- [ ] W21.3 Review relevant CrabBuild repositories for blob-backed versioning.
- [ ] W21.4 Review SlateDB for S3 storage, publication and recovery tradeoffs.
- [ ] W21.5 Review SQLite's VFS abstraction for logical/storage separation.
- [ ] W21.6 Review Cloudflare ArtifactFS for feature and benchmark comparisons.
- [ ] W21.7 Review Erlang gen_statem at the end of primary work for storage and
  communication lifecycle lessons; create follow-up tasks for applicable findings.
- [ ] W21.8 Verify license compatibility before incorporating any reference code.
- [ ] W21.9 Review Rivet Actors' FoundationDB/SQLite VFS implementation at a
  pinned revision. Record exact source paths, transaction/locking/durability
  assumptions, ambiguous-commit behavior and recovery tests; map adopt/adapt/
  reject decisions to W07/W12/W15/W28 and preserve applicable attribution.
  Source review recorded at [Rivet Actors review](docs/reviews/rivet-actors.md),
  pinned to `78336a1a0ee33bb45e6b15e89963cbe91713353b`: Apache-2.0, no code
  copied. Relevant implementation is UDB/Postgres/RocksDB rather than a direct
  FoundationDB client. Stable operation IDs, fencing and explicit sync/commit
  boundaries are applicable; single-actor no-op SQLite locks are not. Mapping
  all lessons into implemented acceptance tests remains open.

## W22 — Distributed cache: discuss after primary work

- [ ] W22.1 Hold D04 discussion before implementation: topology, service choice,
  consistency, invalidation, failure behavior, tenancy and cost expectations.
- [ ] W22.2 Implement the agreed cache across multiple HTTP server instances and
  drives; a single-process cache is not distributed-cache acceptance.
- [ ] W22.3 Test stale reads, concurrent writes, eviction, partitions, recovery,
  version-pinned reads and cross-drive isolation; benchmark against uncached IO.

## W23 — Future physical copy-on-write

- [ ] W23.1 Define immutable sharing, changed-byte/block granularity, reference
  lifetime, writable forks and reclamation using W14 and reviewed sources.
- [ ] W23.2 Agree implementation sequencing after primary requirements; then
  implement and test crash-safe sharing, isolation and changed-byte cost scaling.

## W24 — Domain and marketing site

- [x] W24.1 AWS MCP verified the Route 53 hosted zone for `mount-rs.com` and
  configured the apex and `www` A records to Vercel at `76.76.21.21`.
- [x] W24.2 Built and deployed the combined marketing/docs site with TanStack
  Start to the user's Vercel Hobby plan, explicitly authorized 2026-09-20. No
  paid plan upgrade or paid resources. Claims and support status are visible.
- [x] Site child task delivered Vercel prebuilt-output/header/route hardening in
  `c9088aa` and `8cf0c5d`; these commits are site-only and do not prove a live
  Vercel deployment.
- [x] W24.3 Public DNS, HTTPS and the actual deployment are verified for
  `mount-rs.com` and `www.mount-rs.com`. Route 53 change
  `C0259256PYIMTC38BKLA` reached `INSYNC`; both hosts returned HTTP 200 from
  Vercel with the expected homepage and security headers. Operational handoff
  is complete; this backlog entry did not authorize spending.

## W25 — Actual AWS S3 integration

### Current W25 gate map — 2026-09-22

This map is the current scope summary for W25. A qualification-account or
credential-free PASS proves only the bounded provider contract and safety
check it names; it does not promote test resources, local metadata, or CI
configuration into a production deployment.

| Gate | Current status | Boundary and next evidence |
| --- | --- | --- |
| AWS S3 provider and public SDK/CLI qualification | **PASS for W25 qualification** | The authorized `myroot` packet and public consumer paths pass the live block, composed-filesystem, restart/reopen, cleanup, and optional local PGlite pairing rows recorded below. This is provider compatibility evidence; make a separate current-source library release decision after the package/support-matrix and release-artifact gates. |
| Qualification-account resources and role | **PASS for test scope only** | The read-only audit and scoped-role prefix denial pass for the dedicated test bucket. The bucket, role, prefix, and `myroot` session are not production ownership or least-privilege approval. |
| Production bucket/IAM/IaC | **OPEN** | The CloudFormation and policy contracts are reviewable and locally validated, but production parameters, change set, role trust, resource creation, and live production audit still require an adopter/deployment owner. |
| Hosted OIDC and protected environment | **BLOCKED** | Read-only audits still report missing environment protection/reviewer and inputs, GitHub OIDC provider, and immutable-subject role trust. The workflow must remain unauthenticated until those external controls are configured and approved. |
| Production metadata, recovery, and DR | **OPEN** | Local AWS+PGlite pairing, fencing, restore, and fresh-server reopen are composition evidence. Production metadata ownership, multi-writer scope, schema migration, failure recovery, independent backup/restore, and DR drills remain unverified. |
| Deployment operations | **OPEN** | Stats, runbooks, provenance binding, and contract fixtures are implementation safeguards. Exporters, retry measurement, credential-expiry/cost alerts, approved SLOs, load/soak/fault/restore, canary, rollback, and post-deploy smoke remain deployment evidence. |
| Hosted release evidence | **BLOCKED** | The provenance contract is locally fail-closed, but hosted AWS authentication/acceptance is still blocked by external OIDC and protected-environment state; no load/soak/fault/restore, canary, rollback, or post-deploy smoke result is claimed. |
| Production sign-off | **NO-GO** | W25.5-W25.8 are not all complete. Do not call the adopter/reference deployment production-ready from the qualification packet or local gates. |

### Next verifiable W25 gates

Run these in order and record each result against the exact source and owner:

1. **Library/runtime release decision:** run the current-source locked package
   gates, supported-platform matrix, security/release-artifact review, and
   public SDK/CLI provenance check. This is the remaining runtime release
   decision; it does not require this repository to own a production AWS
   account, pager, canary, or customer SLO.
2. **Deployment contract:** the adopter/deployment owner supplies and reviews
   the production account, bucket, region, prefix, versioning/encryption,
   runtime and maintenance roles, IaC change set, and protected OIDC
   environment. Re-run the read-only resource, policy, environment, and OIDC
   audits before any live acceptance.
3. **Hosted base acceptance:** run the workflow on `refs/heads/main` with the
   approved short-lived role and expected account binding. Require the public
   SDK/CLI, block, composed, restart/reopen, ownership-gated cleanup, and
   artifact/provenance results; a safe preflight refusal is not acceptance.
4. **Metadata and recovery acceptance:** select the production metadata
   provider, then pass multi-writer/fencing, restart, schema migration,
   backup/restore, failure recovery, and DR tests against the production
   topology. Hosted PGlite remains optional deployment-confidence evidence
   unless this repository operates that reference deployment.
5. **Operations and rollout:** attach exporter/retry/expiry/cost signals to
   approved SLOs, run load/soak/fault/restore drills, deploy a canary, exercise
   rollback, and capture post-deploy smoke for the exact released artifact.
6. **Sign-off:** record the released commit/image, configuration digest,
   identity, evidence links, rollback owner, and explicit GO/NO-GO decision.

- [x] Scope boundary clarified for this library/runtime: W25 qualification
  evidence determines whether the AWS S3 provider and its public SDK/CLI paths
  are suitable for release, while production bucket/IAM/IaC, hosted OIDC,
  deployment metadata/DR, operational SLOs, canary, rollback, and post-deploy
  smoke are adopter or reference-deployment gates. The latter remain tracked
  below and must not be promoted to library-release blockers unless this
  repository explicitly operates that deployment.
- [x] W25.1 AWS MCP became available after the app restart. STS identity and
  account-owned bucket inventory verified; testing region is `ap-southeast-2`.
- [x] Create and read back private `myroot` bucket
  `mount-rs-integration-922978963556-ap-southeast-2`: all four public-access
  blocks enabled, bucket-owner-enforced ownership, AES256 server-side
  encryption, test-resource tags, seven-day expiry under `mount-rs-tests/`,
  and one-day incomplete multipart cleanup. The
  `mount-rs-aws-s3-integration-test` role is scoped to the harness prefix and
  trusted only by the authenticated `myroot` SSO role; no access keys were
  created. MCP provisioning is not a Rust integration test result.
- [x] AWS MCP OAuth was revalidated on 2026-09-21 for account `106427005394`: bucket location, all four public-access blocks, bucket-owner-enforced ownership, AES256 encryption, lifecycle and tags were read successfully; the root `mount-rs-tests/aws-s3/` inventory was empty before and after testing. A unique-prefix service-side probe passed create-only immutable publication, duplicate rejection, byte ranges, stale conditional read/CAS rejection, current ETag CAS, a 65,537-byte boundary read, four concurrent writers, scoped deletion, and post-delete empty-prefix verification. This is AWS API/SDK evidence only: the MCP caller was account root and the bucket has no bucket policy, so least-privilege authorization remains unverified. That historical MCP bucket is not the selected `myroot` target and remains untouched.
- [x] W25.2 Provision private test bucket, narrowly scoped access, and test-data
  cleanup/retention policy. The 2026-09-21 `myroot` provisioning created the
  dedicated `ap-southeast-2` bucket, Block Public Access, BucketOwnerEnforced,
  AES256, seven-day `mount-rs-tests/` retention, one-day multipart-abort
  cleanup, tags, and a one-hour role at
  `arn:aws:iam::922978963556:role/mount-rs/mount-rs-aws-s3-integration-test`.
  The role grants only prefix-scoped list/object access plus caller identity,
  and credentials remain outside chat and source control.
- [x] W25.3 Execute actual AWS S3 block and composed-filesystem integration
  tests with restart/reopen, ranges, conditional immutable writes and cleanup.
  The clean 2026-09-21 live run used `AWS_PROFILE=myroot` and the scoped role
  in `ap-southeast-2` and passed both ignored service tests in separate Cargo
  processes: `actual_aws_s3_block_and_composed_filesystem` (1 passed) and
  `actual_aws_s3_reopen_after_process_restart` (1 passed). The run covered
  immutable blocks, ranges, conditional/CAS behavior, multi-chunk writes and
  overwrite/truncate/extend/sparse-tail behavior, SQLite metadata composition,
  fresh-process reopen, and exact owner-verified prefix cleanup with zero
  remaining objects. The same run also passed the real Rust CLI
  `sdk-self-test --reopen` against a separate owned prefix, proving the
  configuration-driven consumer path. AWS S3 evidence does not replace
  Cloudflare R2 or RustFS acceptance.
- [x] The 2026-09-21 rerun after hardening the qualification boundary passed
  the read-only resource audit with expected caller account `922978963556`
  and bucket-region verification, then passed the CLI, composed SDK, and
  fresh-process reopen gates under owned prefix
  `mount-rs-tests/aws-s3/20260921T110709Z-26488-cb5ea2ff3d0d69de816b63db34ef1806`.
  Direct AWS tests now construct the public `AwsS3Config` path rather than a
  parallel raw client, and version-aware cleanup completed with
  `AWS_S3_TEST_PASS`.
- [x] The current pushed-head rerun at `da4d36c` refreshed the same evidence
  under the selected `myroot` account: the read-only audit passed for
  `mount-rs-integration-922978963556-ap-southeast-2` in `ap-southeast-2`
  (`BucketOwnerEnforced`, `AES256`, versioning `None`, seven-day lifecycle,
  one-day incomplete-multipart abort), the scoped role denied a sibling
  prefix, the public CLI SDK self-test reopened successfully, the composed
  AWS block test passed, and the cross-process reopen test passed. The run
  cleaned its owned prefix and emitted `AWS_S3_TEST_PASS` at
  `mount-rs-tests/aws-s3/20260921T120543Z-65309-0663df4c1d84504983babd5ff88f4e02`.
- [x] The current pushed head `2633bec` refreshed the live qualification in the
  selected `myroot` account. The read-only resource audit passed with caller
  account `922978963556`, the scoped role denied the sibling prefix, the public
  Rust CLI self-test wrote/shut down/reopened/read successfully, the composed
  AWS S3 block test passed, the fresh-process reopen test passed, and owned
  prefix cleanup emitted `AWS_S3_TEST_PASS` at
  `mount-rs-tests/aws-s3/20260921T124037Z-88398-91d73f60bde3b905c3af2cc78b38b224`.
  This is current test-account evidence, not production resource or hosted
  deployment acceptance.
- [x] The pushed W25 revision `56ef9ab` passed a fresh scoped live packet on
  2026-09-22 under the dedicated role: sibling-prefix denial, public SDK/CLI
  self-test, composed AWS S3 filesystem, process reopen, independent PGlite
  metadata, writer fencing, PGlite backup/restore and fresh-server reopen,
  and exact owned-prefix cleanup. It emitted `AWS_S3_TEST_PASS` for
  `mount-rs-tests/aws-s3/20260921T165854Z-84404-217dc2bf24fb46f9e3b96e88ba4fd4a4`
  and `AWS_S3_PGLITE_TEST_PASS` for its child prefix. The standalone
  `tests/aws/Cargo.lock` was refreshed for the current `mount-rs-fuse`
  `futures-util` dependency so the harness now passes its `--locked` gate.
  This remains qualification-account and local-metadata evidence, not
  production deployment acceptance.
- [x] A fresh current-source scoped rerun at pushed source
  `860492d8595b665361e8d9eff46498280fc8de1f` on 2026-09-22 passed the same
  dedicated-role sibling-prefix denial, public SDK/CLI self-test, composed
  AWS S3 filesystem, process reopen, independent-PGlite metadata, writer
  fencing, PGlite backup/restore, fresh-server reopen, and exact owned-prefix
  cleanup gates under
  `mount-rs-tests/aws-s3/20260921T173952Z-67696-542c6ba2bf6552e46bc85e0c0873bf8c`.
  Both `AWS_S3_TEST_PASS` and `AWS_S3_PGLITE_TEST_PASS` were emitted. This is
  refreshed qualification-account and local-metadata evidence only; production
  metadata ownership, independent backup/restore, schema migration, failure
  recovery, DR, and operational sign-off remain open.
- [x] The current shared source `2101e5553e2594ce6e24aac6e28510cce2ec0b96`
  passed a fresh authorized `myroot` qualification on 2026-09-22 under
  `mount-rs-tests/aws-s3/20260921T180316Z-14071-597ad4c6c87b5c310fe02d98120c0036`:
  sibling-prefix denial, public SDK/CLI self-test, composed AWS S3 filesystem,
  process reopen, independent PGlite metadata, writer fencing, PGlite
  backup/restore, fresh-server reopen, and exact owned-prefix cleanup all
  passed. The run emitted both `AWS_S3_TEST_PASS` and
  `AWS_S3_PGLITE_TEST_PASS`; this is qualification-account and local-metadata
  evidence only, not production deployment acceptance.
- [x] The latest pushed source
  `870348184b5a047faea01f584a68cb961b34f810` passed a fresh authorized
  `myroot` qualification on 2026-09-22 under
  `mount-rs-tests/aws-s3/20260921T183403Z-86652-1728a866a9affcd4348775aa8416f075`:
  sibling-prefix denial, public SDK/CLI self-test, composed AWS S3 filesystem,
  process reopen, independent PGlite metadata, writer fencing, PGlite
  backup/restore, fresh-server reopen, and exact owned-prefix cleanup all
  passed. Both `AWS_S3_TEST_PASS` and `AWS_S3_PGLITE_TEST_PASS` were emitted;
  this remains qualification-account and local-metadata evidence only, not
  production deployment acceptance.
- [x] The current shared-mainline qualification at pushed source
  `0d017f09453af530517a2dfef5dc251c1a827932` passed on 2026-09-22 under
  `myroot` and the dedicated test role. The scoped packet passed sibling-prefix
  denial, public SDK/CLI self-test, composed AWS S3 filesystem, process reopen,
  independent PGlite metadata, writer fencing, PGlite backup/restore,
  fresh-server reopen, and exact owned-prefix cleanup under
  `mount-rs-tests/aws-s3/20260921T190207Z-39577-3e389412508fea6c7c806b9477ffaf8f`.
  Both `AWS_S3_TEST_PASS` and `AWS_S3_PGLITE_TEST_PASS` were emitted. This is
  current qualification-account and local-metadata evidence only; production
  resource, metadata, DR, hosted release, and operational gates remain open.
- [x] W25.4 Expose and qualify the first-class AWS S3 provider through the
  public Rust SDK and versioned Rust CLI configuration. `kind: "aws-s3"`
  accepts only bucket, region, prefix, and durable fields, resolves signed
  workload credentials from the AWS environment/role chain, rejects custom
  endpoints, and remains block-only. The live gate now runs the CLI
  `sdk-self-test` plus the SDK composition against the same scoped role; the
  public AWS path is no longer only a direct integration-test construction.
- [x] The live AWS packet is present in the provider/test crates: immutable
  block/range/conditional/CAS, composed SQLite metadata, fresh-process reopen,
  the public SDK and CLI configuration paths, nonce-owned cleanup and
  credential-safe validation, and a non-mutating sibling-prefix authorization
  denial check. The live packet also includes a bounded repeated write/read
  workload sample; a production load/soak result remains a deployment gate.
  The harness emits the secret-free
  `AWS_S3_TEST_BLOCKED` result when local credentials are absent, while the
  renewed `myroot` run above provides the live Rust acceptance.
- [x] The harness accepts an optional `AWS_S3_TEST_ROLE_ARN`, assumes that
  caller-provided role with a one-hour session, and uses the resulting temporary
  credentials for all S3 requests and cleanup. It does not create IAM resources
  or access keys; W25.2 is provisioned in `myroot`, and the live local Rust
  run is now recorded above.
- [x] A fresh end-to-end qualification rerun at audit commit `d7ccdc7` on
  2026-09-22 passed scoped-role sibling-prefix denial, the public SDK/CLI
  self-test, composed AWS S3 filesystem, process reopen, independent PGlite
  metadata, writer fencing, restored-PGlite reopen, and exact owned-prefix
  cleanup under
  `mount-rs-tests/aws-s3/20260921T152108Z-66596-90bba238142882ef153e6e3c246d0003`.
  The read-only resource audit at the same source passed account/region
  binding, public-access blocks, BucketOwnerEnforced ownership, AES256,
  versioning readback, seven-day lifecycle, and one-day multipart-abort checks.
  This remains qualification-account evidence, not production deployment
  acceptance.
- [x] Repository qualification after the AWS packet passed on 2026-09-21 at
  local `4a72d85` (an ancestor of current `origin/main` `7488aa8`):
  `CARGO_NET_OFFLINE=true ./scripts/cargo-shared test --workspace
  --all-targets --locked --offline` and strict workspace Clippy with
  `--all-targets --locked --offline -- -D warnings` both exited 0. The
  all-features variant remains an explicit host prerequisite boundary because
  this macOS runner does not provide native `libfdb_c`; the site typecheck also
  needs a network-backed dependency install and is not claimed from the
  offline run.
- [x] The current pushed head `da4d36c` also passed the full locked offline
  workspace test gate and strict workspace Clippy with `-D warnings`. The
  passing run includes the 14-test S3 gateway suite, 13 AWS-provider unit
  tests, both signed HTTP interop tests, and the public SDK/CLI tests; the
  workspace's explicitly ignored native/service rows remain separate gates.
- [x] Final current-head qualification rerun on 2026-09-21 at local
  `b720696` passed `cargo fmt --all -- --check`, the full locked offline
  workspace test gate, and strict workspace Clippy with `-D warnings`. This
  confirms the current source and workflow head before rollout review; the
  explicitly ignored native/service rows and all production deployment gates
  remain separate and are not claimed by this local result.
- [x] The current security-remediation head `3fca802` passed the full locked
  offline workspace test gate and strict workspace Clippy with `-D warnings`.
  The same source passed the authenticated `myroot` AWS S3 CLI/self-test,
  composed filesystem, fresh-process reopen, independent PGlite metadata,
  writer-fencing, restored-PGlite reopen, and exact cleanup gates under
  `mount-rs-tests/aws-s3/20260921T144535Z-15427-5ae13eaf019b31185a11d784fdfdcf52`.
  This remains qualification-account and isolated-metadata evidence, not
  production deployment acceptance.
- [x] The integrated W25 evidence boundary `e168315` passed
  `cargo fmt --all -- --check`, the full locked offline workspace test gate,
  and strict workspace Clippy with `-D warnings` on 2026-09-22. This rerun
  covered the AWS provider, public SDK/CLI, S3 gateway, and the other
  workstream changes already on that commit; the later unrelated `de6514c`
  N-API test-only change landed after the gate and requires a post-rebase
  verification before it can be included in current-head release evidence.
- [x] The latest W25 repository-gate audit at `ae33e4c` passed
  `cargo fmt --all -- --check`, the full locked offline workspace test gate,
  and strict workspace Clippy with `-D warnings` on 2026-09-22. This includes
  the AWS S3 provider, SDK/CLI, gateway, CI-safety changes, and the integrated
  workstream code present at that commit; later unrelated NFS/W01 commits are
  outside this evidence boundary.
- [x] The latest tested repository boundary `8004999` passed on 2026-09-22:
  `cargo fmt --all -- --check`,
  `CARGO_NET_OFFLINE=true ./scripts/cargo-shared test --workspace
  --all-targets --locked --offline`, and strict workspace Clippy with
  `--all-targets --locked --offline -- -D warnings`. The passing test gate
  includes the AWS provider, SDK/CLI, S3 gateway, policy/preflight support, and
  current integrated source. Subsequent W25 workflow and evidence-documentation
  commits do not change the provider source covered by that gate. Explicitly
  ignored native/service rows remain separate prerequisites and are not promoted
  to production evidence.
- [x] Current W25 repository-gate boundary `daf2a51` passed on 2026-09-22:
  `cargo fmt --all -- --check`, the full locked offline workspace test gate,
  and strict workspace Clippy with `-D warnings`. The test and Clippy runs used
  an explicitly isolated Cargo target so concurrent worktrees could not supply
  stale package metadata; the isolated run included the AWS provider's 15 unit
  tests, S3 gateway tests, SDK/CLI tests, and the current W01/W26 workspace
  changes. Explicitly ignored native/service rows remain separate prerequisites
  and are not promoted to production evidence.
- [x] The latest tested integrated boundary
  `0bb628b0323901a1632b05dd8321c32fcabca7d8` passed on 2026-09-22 after
  the documentation chunk was rebased onto and pushed to `origin/main`:
  `cargo fmt --all -- --check`, the full locked offline workspace/all-target
  test gate, and strict workspace Clippy with `-D warnings` using the
  explicitly isolated Cargo target and required local loopback permission.
  The gate includes the 9P loopback integration and the AWS provider, S3
  gateway, SDK/CLI, N-API, W01, W04, W26, and other non-ignored workspace rows.
  Explicitly ignored native/service rows remain separate prerequisites and are
  not promoted to production evidence.
- [x] Current credential-free W25 rollout-contract fixtures at pushed source
  `70d37fefe26e41a2406bde32b27de5f563c3ab2f` passed on 2026-09-22:
  CloudFormation template structure (`AWS_S3_TEMPLATE_CONTRACT_PASS`),
  synthetic bucket-policy contract and tamper cases
  (`AWS_S3_BUCKET_POLICY_TEST_PASS cases=2`), the seven-case CI-input
  validator (`AWS_S3_CI_CONFIG_TEST_PASS cases=7`), and the three-case
  protected-environment fixture (`AWS_S3_CI_ENVIRONMENT_TEST_PASS cases=3`).
  These are credential-free fail-closed safeguards only; they do not approve
  the external GitHub environment, IAM trust, or production parameters.
- [x] The current credential-free W25 rollout-contract fixtures at pushed
  source `f950e5b87092504cc43a8f68a7fcc07098abc345` passed on 2026-09-22:
  `AWS_S3_TEMPLATE_CONTRACT_PASS`, `AWS_S3_BUCKET_POLICY_TEST_PASS cases=2`,
  `AWS_S3_CI_CONFIG_TEST_PASS cases=7`, and
  `AWS_S3_CI_ENVIRONMENT_TEST_PASS cases=3`. These are fail-closed local
  safeguards only; they do not approve the external GitHub environment, IAM
  trust, or production parameters.
- [x] The exact pushed S3 observability source
  `eed34234b7706f490dcfe91d8316bc20fc1fe1e1` passed on 2026-09-22:
  `cargo fmt --all -- --check`, the full locked offline workspace/all-target
  test gate, and strict workspace Clippy with `-D warnings` using the isolated
  Cargo target and required local loopback permission. The gate includes the
  streamed request/response byte accounting tests in the 17-case S3 gateway
  suite. Explicitly ignored native/service rows remain separate prerequisites
  and are not promoted to production evidence.
- [x] The current shared source
  `984e070b1568e50c7e30962a7064049a7c95f846` passed on 2026-09-22:
  `cargo fmt --all -- --check`, the full locked offline workspace/all-target
  test gate with the required local loopback permission, and strict workspace
  Clippy with `-D warnings` on the isolated Cargo target
  `/private/tmp/mount-rs-w25-current-shared-gate`. The gate included the 18-case
  S3 gateway suite and the current W01/S3 source. Ignored native/service rows
  remain explicit prerequisites and are not promoted to production evidence.
- [x] The latest pushed current-source boundary
  `43a34d28c2173c2429bcfa1f653dd048dc150b22` passed on 2026-09-22:
  `./scripts/cargo-shared fmt --all -- --check`, the full locked offline
  workspace/all-target test gate with the required local loopback permission,
  and strict workspace Clippy with `-D warnings` on the isolated Cargo target
  `/private/tmp/mount-rs-w25-current-stats-gate`. The gate included the 9P
  loopback integration, the 16-case R2 provider suite including the new
  bounded block-store diagnostics test, the 18-case S3 gateway suite, and all
  other non-ignored workspace rows. Explicitly ignored native/service rows
  remain separate prerequisites and are not production acceptance.
- [x] The current shared `origin/main` boundary at
  `31e122bc2e5790bb3568c01aaea4b236d89dce96` passed on 2026-09-22 after the
  security-evidence rebase: `./scripts/cargo-shared fmt --all -- --check`,
  the full locked offline workspace/all-target test gate with the required
  local loopback permission, and strict workspace Clippy with `-D warnings`
  on the isolated Cargo target `/private/tmp/mount-rs-w25-current-main-gate`.
  The credential-free template, bucket-policy, CI-config, and CI-environment
  contract fixtures also passed. Explicitly ignored native/service rows and
  all production deployment gates remain separate prerequisites.
- [x] The current credential-free W25 harness contract chunk added a
  fail-closed validator and synthetic regression matrix for W25.2/W25.3.
  `AWS_S3_TEST_CONFIG_TEST_PASS cases=10` passed without invoking AWS, Cargo,
  or a provider. The matrix covers profile and explicit temporary-credential
  inputs, role/account binding, missing or mismatched expected accounts,
  incomplete or ambiguous credential sources, endpoint overrides, unsafe
  prefixes, and secret-safe output. The local live harness now runs this guard
  before AWS CLI access and requires `AWS_S3_TEST_EXPECTED_ACCOUNT_ID` for
  every live run, matching an optional role ARN when one is supplied.
- [x] The hosted AWS workflow now triggers and hashes the harness validator
  and its offline test, and runs the 10-case contract preflight before
  authentication. Existing credential-free rollout fixtures also passed in
  the same verification set: `AWS_S3_CI_CONFIG_TEST_PASS cases=7`,
  `AWS_S3_CI_ENVIRONMENT_TEST_PASS cases=3`,
  `AWS_S3_BUCKET_POLICY_TEST_PASS cases=2`, and
  `AWS_S3_TEMPLATE_CONTRACT_PASS statements=5`. These are fail-closed
  safeguards only; they do not replace live W25.3 service acceptance or
  production deployment approval.
- [x] The follow-up offline acceptance audit tightened account binding for
  W25.2/W25.3: every local or hosted harness run now requires the reviewed
  `AWS_S3_TEST_EXPECTED_ACCOUNT_ID`, and the live harness compares the actual
  STS caller account before claiming any prefix. Credential-free regressions
  passed as `AWS_S3_TEST_CONFIG_TEST_PASS cases=11` and
  `AWS_S3_TEST_IDENTITY_TEST_PASS cases=6`; no AWS CLI, Cargo, or provider was
  contacted by these tests. The source audit also confirmed cleanup remains
  ownership-gated before deletion and verifies both current objects and all
  versions/delete markers after cleanup.
- [ ] Optional hosted W25.3 PGlite pairing remains a separate deployment-
  confidence row, not a library/runtime release blocker. The
  current `.github/workflows/aws-s3.yml` runs the base AWS harness but does not
  install `tests/pglite` dependencies or set
  `MOUNT_RS_RUN_AWS_S3_PGLITE=1`; the local authorized packet has passed the
  PGlite pairing, writer-fencing, backup/restore, and fresh-server reopen rows,
  but hosted PGlite acceptance still requires user-authorized AWS OIDC/test
  bucket access plus the pinned Node dependency setup. No hosted PGlite pass is
  claimed from this offline audit; the row blocks only a hosted AWS+PGlite
  reference-deployment claim.
- [ ] W25.5 (deployment track) Define and approve the production rollout contract: AWS account,
  region and bucket ownership; IaC or an equivalent reviewable change; bucket
  policy, Block Public Access, Object Ownership, encryption/KMS, versioning,
  retention/lifecycle, prefix ownership, runtime/maintenance roles, and no
  long-lived credentials. A read-only resource audit script now checks the
  qualification controls without mutation. It passed the `myroot` test bucket
  for all four public-access blocks, BucketOwnerEnforced ownership, AES256
  default encryption, seven-day `mount-rs-tests/` expiry, and one-day
  incomplete-multipart abort in the current `myroot` rerun; the W25 bucket and
  role are test resources, so production resource review remains open. A
  fresh read-only resource audit rerun on 2026-09-22 at audit commit
  `2f13354` also passed the account/region binding, all four public-access
  blocks, BucketOwnerEnforced ownership, AES256 encryption, seven-day
  lifecycle, and one-day incomplete-multipart abort checks for that
  qualification bucket. The current pushed audit at `0010246` repeated the
  same read-only checks and additionally enforced the reviewed versioning
  status (`None`) before reporting pass; no production resource was changed.
  A fresh current-source audit at pushed head `74b5150` repeated those
  account/region, public-access, ownership, encryption, lifecycle, multipart,
  and expected-versioning checks without mutating the qualification bucket.
  The
  reviewable [`infra/aws-s3-production.yaml`](infra/aws-s3-production.yaml)
  contract now expresses retained state, versioning, encryption choice,
  lifecycle and multipart cleanup, transport denial, prefix-scoped runtime
  access, and separately governed maintenance access. AWS CloudFormation
  syntax validation passed on 2026-09-21 without creating a stack or change
  set; after the `OwnedPrefix` regex was tightened to reject empty and dot
  components, the revised template also passed the read-only validation API
  on 2026-09-22 without creating a stack or change set. The audit still fails
  closed on inherited endpoint/service-profile
  overrides, requires an expected caller account, verifies bucket location,
  and checks noncurrent-version retention whenever versioning is enabled
  before reporting controls; approved production parameters, role
  trust, change-set review, and live production audit remain open. The bucket
  policy transport-deny resource now covers every object key in the bucket,
  not only the owned prefix; the revised template passed the read-only
  CloudFormation validation API on 2026-09-22 without creating a stack or
  change set. Its versioning lifecycle now also expires noncurrent versions
  using the same reviewed retention parameter, avoiding an unbounded version
  accumulation path. The template now fails closed on reused runtime and
  maintenance role parameters, requires `KmsKeyArn` for SSE-KMS, and rejects
  an unused key ARN for SSE-S3. The resource audit can also enforce the
  reviewed live versioning status with
  `AWS_S3_AUDIT_EXPECTED_VERSIONING_STATUS`; the hosted workflow path set now
  includes the resource audit, PGlite harness, and shared provider/metadata
  packages. These are reviewable safeguards only; production parameters,
  role trust, change-set review, and live production audit remain open.
  A separate read-only bucket-policy audit now verifies the full-bucket
  transport deny and exact prefix-scoped runtime/maintenance statements from
  the CloudFormation contract; its synthetic valid/tampered policy tests are
  wired into the hosted preflight. No production bucket policy has been
  changed or claimed as audited. The credential-free
  `scripts/test-aws-s3-production-template.rb` gate now structurally asserts
  the retained bucket controls, KMS/versioning rules, lifecycle, and all five
  policy statements; it is wired into the hosted preflight and passed locally
  without AWS credentials. The CloudFormation bucket-name constraint and the
  read-only resource audit now reject consecutive dots and invalid length or
  edge characters consistently with the hosted preflight.
- [x] A fresh read-only resource audit at pushed source `56ef9ab` on
  2026-09-22 again passed the selected `myroot` qualification bucket's
  account/region binding, all four public-access blocks,
  `BucketOwnerEnforced` ownership, AES256 encryption, `None` versioning,
  seven-day `mount-rs-tests/` lifecycle, and one-day incomplete-multipart
  abort. It did not mutate the bucket or rerun service acceptance; this is
  qualification-account evidence only.
- [x] A current-source read-only resource audit at pushed source
  `313fb2f2a6bd6e68305a86bc55036d77c563ca8c` on 2026-09-22 passed the same
  qualification-bucket account/region binding, all four public-access blocks,
  `BucketOwnerEnforced` ownership, AES256 default encryption, `None`
  versioning, seven-day `mount-rs-tests/` lifecycle, and one-day
  incomplete-multipart abort checks. It made no AWS changes and remains
  qualification-account evidence only; the production bucket, policy, roles,
  and approved change set remain open.
- [x] A current shared-source read-only resource audit at pushed source
  `6e19c4d2388aac02c0a3278a3f524febdc4b07ec` on 2026-09-22 passed the
  qualification bucket's account/region binding, all four public-access
  blocks, BucketOwnerEnforced ownership, AES256 encryption, `None` versioning,
  seven-day `mount-rs-tests/` lifecycle, and one-day incomplete-multipart
  abort checks. It made no AWS changes and remains qualification-account
  evidence only; the production bucket, policy, roles, and approved change
  set remain open.
- [x] The latest read-only qualification-bucket audit at pushed source
  `9b5acfbac3d88d5f17a969defd447f5d44ee3023` on 2026-09-22 passed the same
  account/region, public-access, ownership, AES256 encryption, `None`
  versioning, seven-day lifecycle, and one-day incomplete-multipart abort
  controls. It made no AWS changes and remains qualification-account evidence
  only; the production bucket, policy, roles, and approved change set remain
  open.
- [x] The current shared-mainline read-only qualification-bucket audit at
  pushed source `a03edef8bbdbebb20626b5fd7e62267f34ebb720` on 2026-09-22
  passed account/region binding, all four public-access blocks,
  BucketOwnerEnforced ownership, AES256 encryption, `None` versioning,
  seven-day `mount-rs-tests/` lifecycle, and one-day incomplete-multipart
  abort checks. It made no AWS changes; this remains qualification-account
  evidence only and the production bucket, policy, roles, and approved change
  set remain open.
- [ ] W25.6 (deployment track) Qualify the production metadata pairing. Select a remote durable
  metadata provider and pass multi-writer/fencing, restart, backup/restore,
  schema-migration, and failure-recovery tests with actual AWS S3 blocks.
  The current SQLite composition is single-host reopen evidence only. Partial
  pairing evidence now exists: on 2026-09-21, the scoped AWS role passed the
  AWS CLI/block/restart gates and the new
  `live_aws_s3_blocks_with_independent_pglite_metadata` row passed with an
  isolated real PGlite socket server, a fresh metadata connection, a fresh
  signed AWS client, filesystem reopen, and exact parent-prefix cleanup at
  `mount-rs-tests/aws-s3/20260921T133321Z-23452-0493c0f8fe5454cbfd42f48dfd58f728/pglite`.
  The expanded opt-in run at `mount-rs-tests/aws-s3/20260921T134403Z-54972-b8831d9b39f99263ce764ba298b05302`
  also passed `live_aws_s3_pglite_prepare_for_restart` with independent-writer
  fencing and `live_aws_s3_pglite_reopen_after_restore` after restoring a
  temporary on-disk PGlite data directory into a fresh server process.
  The rerun after adding the explicit AWS S3 five-retry/30-second request
  budget passed the same CLI, composed, process-reopen, independent-PGlite,
  fencing, and restore/reopen gates under
  `mount-rs-tests/aws-s3/20260921T141112Z-81269-ab3a599172244316234d1f3b23181dba`.
  This is local metadata backup/restore and restart evidence only.
  A fresh current-source rerun at `3fca802` passed the same gates and exact
  cleanup under
  `mount-rs-tests/aws-s3/20260921T144535Z-15427-5ae13eaf019b31185a11d784fdfdcf52`.
  This does not close W25.6: production metadata ownership, multi-writer
  fencing, backup/restore, schema migration, failure recovery, and DR evidence
  remain open.
- [ ] W25.7 (deployment track) Add deployment observability and operations: S3 latency/error and
  retry metrics, conditional-conflict and orphan/cleanup signals, credential
  expiry detection, capacity/cost alerts, SLOs, incident runbooks, and
  canary/rollback procedures. The adjacent S3 gateway now exposes a bounded
  `S3Session::stats()` snapshot for latency, buffered and consumed streaming
  request/response bytes, operation counts, and
  authentication/conditional/throttling/client/server error classes; the
  public SDK's optional observability path records provider block latency,
  errors, bytes, and bounded reconciliation scanned/protected/recent/deleted
  counts through local snapshots, tracing, and OTLP counters. The new
  [`docs/aws-s3-operations-runbook.md`](docs/aws-s3-operations-runbook.md)
  maps those signals to alerts, identity/expiry checks, retention/cost review,
  failure drills, and canary/rollback evidence. The AWS provider now pins a
  five-retry, 30-second internal retry budget below the temporary-credential
  safety boundary. The AWS/R2 immutable block adapter now also exposes a
  bounded `R2BlockStore::stats()` snapshot shared across clones, covering
  logical operation counts, latency, bytes, conditional ID-collision counts,
  terminal retry-exhaustion markers, and bounded not-found,
  authentication/permission, throttling, client, server, and conditional
  error classes. Authentication and terminal retry counters are implementation
  diagnostics only; successful internal retry-attempt measurement is not
  inferred from a terminal error string. These are implementation and runbook
  surfaces only. Exporter wiring, object-store retry measurement,
  credential-expiry detection, cost/retention alerts, approved SLO thresholds,
  and exercised staging procedures remain deployment gates.
- [ ] W25.8 (deployment track) Add hosted release evidence: locked build/artifact provenance,
  approved OIDC or equivalent short-lived role credentials, security scan,
  load/soak/fault/restore drills, staged canary, rollback, and post-deploy
  smoke. The sealed current-source Standard scan
  `02d2c6eb-66e1-41f8-be59-d14aab9fde87` targets exact source
  `4ebba4926045de28e9f03ac75b938947f4487a4b` and reports zero reportable
  findings across six W25 surfaces: AWS S3 credential/endpoint/transport,
  block prefix/immutability, S3 gateway authentication/path handling,
  gateway resource bounds/cleanup, the CI/production contract, and release/
  supply-chain controls. Its canonical coverage is partial: six W25 review
  rows closed against a 650-file repository inventory; unrelated non-W25
  surfaces and live AWS/GitHub deployment state remain explicitly deferred.
  This is current source security evidence, not a production approval. Hosted
  OIDC trust, the protected versioning-status input, and the deployment
  evidence remain open. The latest observed hosted run `35629600687` at
  `62383df` passed root and standalone
  AWS manifest provenance capture, including the standalone
  `tests/aws/Cargo.lock` hash, plus the seven-case validator, bucket-policy,
  CloudFormation, and environment-approval contract suites. It then stopped
  safely at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; AWS credentials,
  identity, and acceptance were skipped. Its non-expired artifact is
  `aws-s3-qualification-35629600687-1` (7,649 bytes). This is a successful
  safety refusal and provenance-contract result, not hosted AWS acceptance.
  The newer observed hosted run `35635498647` at `24408f8` also stopped before
  AWS authentication at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; its protected
  bucket, region, account, versioning, and role inputs were blank. This is a
  current safety refusal rather than an implementation failure or AWS
  acceptance result.
  The provenance-hash expansion now binds the
  policy, preflight, resource/OIDC audit, CloudFormation contract, acceptance,
  PGlite harness, AWS test manifest, and standalone AWS test lockfile inputs
  in this artifact; the workflow also validates the standalone AWS manifest
  with `cargo metadata --locked` before any AWS authentication. This improves
  evidence integrity but does not create AWS authentication or deployment
  evidence. The existing
  test role trust policy allows only the selected SSO administrator role and does
  not trust GitHub's OIDC provider, so an approved IAM trust-policy change and
  protected environment configuration are required before rerunning hosted
  evidence. The read-only
  [`scripts/audit-aws-s3-ci-oidc.sh`](scripts/audit-aws-s3-ci-oidc.sh) now
  checks the immutable GitHub subject, OIDC provider, exact single GitHub
  federation trust statement, protected environment branch policy, and a
  non-self-approvable required reviewer plus required input names without
  mutating either system; additional or broad GitHub federation trust
  statements fail closed. Its credential-free three-case environment fixture
  test is wired into the hosted preflight. The
  fresh read-only audit at pushed source `56ef9ab` on 2026-09-22 returned
  `AWS_S3_OIDC_AUDIT_BLOCKED` for the missing environment protection rules,
  non-self-approvable reviewer, protected-environment inputs and secret,
  missing GitHub OIDC provider, and missing immutable-subject role trust; it
  made no changes. A current-source rerun at pushed source
  `cf18d93d7fdd1656d853f208db81b8b133265fe5` returned the same blocked set and
  made no changes.
- [x] A current read-only rerun at pushed source
  `ce7b365a45f718009f557035d2549fd0faf2c8a8` through the authenticated
  `myroot` profile on 2026-09-22 returned the same fail-closed blocker set:
  `environment_missing_protection_rule`,
  `environment_missing_protected_branch_policy`,
  `environment_missing_non_self_review_required_reviewer`, the four missing
  protected environment inputs/secret, `missing_github_oidc_provider`, and
  `role_missing_immutable_github_subject_trust`. It made no GitHub or AWS
  changes; hosted OIDC evidence remains blocked until the deployment owner
  configures and approves those controls.
- [x] The latest read-only OIDC audit at pushed source
  `9b5acfbac3d88d5f17a969defd447f5d44ee3023` on 2026-09-22 returned the same
  fail-closed blocker set: missing environment protection, protected-branch
  policy, non-self-approvable reviewer, the four protected environment
  inputs/secret, GitHub OIDC provider, and immutable-subject role trust. It
  made no GitHub or AWS changes; hosted OIDC evidence remains blocked until
  the deployment owner configures and approves those controls.
  The workflow now has a secret-safe preflight validator that blocks
  before AWS authentication when those inputs are absent or malformed. The
  validator's secret-free seven-case regression matrix covers valid, missing,
  account-mismatch, endpoint, unsafe-prefix, static-credential, and profile-
  override inputs; it also enforces bucket length/edges, region edges, and
  safe prefix characters. It rejects a role ARN whose account does not match
  the protected `MOUNT_RS_AWS_S3_ACCOUNT_ID` value. The
  adjacent S3 gateway now refuses
  non-loopback binds without a TLS boundary and now stages streaming PUT and
  multipart publication behind bounded atomic rename. CopyObject now uses the
  same bounded cross-driver staging and atomic publication path. ListObjectsV2
  now uses bounded continuation-aware traversal with prefix pruning. Multipart
  and temporary staging now have a configured byte quota, request-boundary TTL
  reaper, and backing-file-aware DeleteObjects behavior. The scan's deferred
  non-W25 repository and external deployment coverage remain open. Do not place AWS
  secrets in the repository or CI logs. The hosted workflow now captures the
  exact source SHA, lockfile/template/script hashes, Rust toolchain metadata,
  and bounded acceptance log as a pinned 14-day artifact even when the
  preflight safely refuses to authenticate; a successful run is still
  required before this becomes release evidence.
- [x] The sealed W25 Standard scan
  `4ba52479-904a-41d2-82d2-9afc20a82931` at pushed source
  `aa529c58ce6801c69d3e6cc0ed8bb5ac7a8d9cf8` on 2026-09-22 reported zero
  reportable findings across six W25 surfaces with partial coverage of the
  659-file repository inventory. Independent baseline/architecture coverage
  did not complete within the bounded review window and is explicitly
  deferred; non-W25 repository surfaces and live AWS/GitHub deployment state
  remain open. This closes the source-review evidence item only; hosted
  release, production identity, load/soak/fault/restore, canary, rollback, and
  post-deploy smoke gates remain open.
- [x] The current shared-mainline read-only OIDC audit at pushed source
  `a03edef8bbdbebb20626b5fd7e62267f34ebb720` on 2026-09-22 returned
  `AWS_S3_OIDC_AUDIT_BLOCKED` for the missing environment protection rule,
  protected-branch policy, non-self-approvable reviewer, four protected
  environment inputs/secret, GitHub OIDC provider, and immutable-subject role
  trust. It made no GitHub or AWS changes; hosted OIDC evidence remains blocked
  until the deployment owner configures and approves those controls.
- [x] The hosted qualification provenance boundary is now fail-closed. The
  workflow records the checked-out source SHA, commit subject, toolchain,
  repository/workflow/ref/event/run identity, and clean-tree state, then
  `scripts/validate-aws-s3-ci-provenance.sh` requires the SHA to equal
  `GITHUB_SHA`, the exact `refs/heads/main` ref, a supported push or manual
  dispatch event, unique required fields, and a clean checkout before AWS
  authentication. Credential-free regression coverage passed
  `AWS_S3_CI_PROVENANCE_TEST_PASS cases=5`; this validates evidence binding
  only and does not claim hosted AWS acceptance.
- [ ] W25.9 (deployment track) Production sign-off: record the exact released commit/image,
  reviewed configuration, live smoke result, rollback owner, and evidence for
  every W25.5-W25.8 gate before calling the AWS workstream production-ready.

## W26 — Apache Ozone S3 backend

### W26 current authoritative status — 2026-09-22

#### Latest W26 authority override — terminal exact-SHA packet

Run `35691451007` is terminal for every W26 producer and the aggregate. It
selected exact code revision
`dccd8351690ba21b4ea01ab8680369c76c442041`, the parent of the documentation
only push that followed. Base Ozone `106629129201` passed; compositions
`106629129102` failed, TiDB `106629129183` failed, FoundationDB
`106629129105` failed and aggregate `106631474430` failed closed. The retained
artifact IDs/digests are recorded in `docs/w26-progress-ledger.md`.

The provider rows completed their full 1,200/1,200 lifecycle with zero
timeouts and cleanup failures: SQLite/R2 `213.947686` IOPS (write/read/delete
p95 `1751.446370/105.174762/202.276166` ms), PGlite/R2 `2065.669446`
(`106.518128/7.567658/2.705368` ms), TiDB/R2 `333.356025`
(`629.659611/36.006496/29.342427` ms) and FoundationDB/R2 `363.254472`
(`528.842275/97.605869/85.797321` ms). PGlite clears the hard target on this
packet; SQLite, TiDB and FoundationDB do not. The aggregate emitted
`W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-log-missing-marker=OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000`.
Production remains **NO-GO**. The next bounded implementation candidate is
TiDB's successful metadata CAS autocommit path, while SQLite hosted variance
and FoundationDB/provider latency remain explicit qualification work; no
threshold reduction, provider skip or averaging is allowed.

#### Historical W26 authority override — exact-SHA qualification dispatch

The PGlite implementation and progress ledger are published. Manual CI run
`35691451007` selected exact shared SHA
`dccd8351690ba21b4ea01ab8680369c76c442041` after the ledger push. At dispatch
capture, base Ozone `106629129201`, TiDB `106629129183` and FoundationDB
`106629129105` were in progress, compositions `106629129102` was queued and
the aggregate was not yet created. This run is the first hosted packet that
can exercise `e0180c75`, but queued/in-progress state is not evidence and
production remains **NO-GO** until every producer and the aggregate are
terminally successful on that one revision.

#### Historical W26 authority override — PGlite publication chunk

The current shared build-on tip is `origin/main` at
`e0180c75a190340f2c0a45265803de9df5605d59`. The published W26 chunk
`e0180c75` moves successful PGlite metadata publication to one parameterized
autocommit fenced CAS update and retains the explicit locked classification
transaction only for zero-row outcomes. Missing rows, stale leases, revision
conflicts, unexplained zero-row outcomes, rollback handling and fail-closed
errors remain unchanged. Full locked workspace tests, strict workspace Clippy,
formatting, diff checks and the focused PGlite compile/test gate passed locally;
five server-dependent focused tests were ignored because no isolated PGlite
server was available on this host. Security scan
`6906585f-77c3-41fa-afe0-06ab4df9e2c6` is sealed with complete changed-file
coverage and zero reportable findings.

The latest hosted W26 packet is run `35689474986`, which selected exact SHA
`1891c36375296bc3695a9d71233c624cad46445c` before `e0180c75`, so it is
diagnostic and cannot qualify this chunk. W26 jobs are terminal: base Ozone
`106623193674` passed; compositions `106623193668` failed with SQLite/R2
`1,325.636778` and PGlite/R2 `766.631418` IOPS; TiDB
`106623193474` failed at `292.606794`; FoundationDB `106623193564` failed at
`410.703639`; aggregate `106625464560` failed closed on the missing
`OZONE_IOPS_PASS` marker. Every row completed 1,200/1,200 lifecycle operations
with zero timeouts and cleanup failures. The parent workflow remains
`in_progress` only for unrelated jobs. Production remains **NO-GO** until a
fresh exact-SHA packet closes every configured provider, all end-to-end
markers and the aggregate on one revision.

The detailed [W26 progress ledger](docs/w26-progress-ledger.md) now records
every W26.1–W26.15 and P14 work item with status, completion percentage,
evidence, remaining action, provisional engineering-time estimate, external
blocker and session time. The current boundary remains: customers deploy and
operate Ozone; W26 owns provider/client correctness and CI qualification;
Ozone/customer teams own secure topology, capacity, 99.99% availability,
five-minute RPO/RTO and backup/DR; another stream owns releases; CI is the
only available qualification environment.

| Current W26 gate | Status / completion | Evidence and next action |
| --- | --- | --- |
| W26.1–W26.2 gateway, immutable blocks, failure/restart/cleanup | **PASS / 100%** | Base job `106623193674` passed; preserve markers on the next exact SHA. |
| W26.3a SQLite/R2 | **PASS in diagnostic packet / 100% current row** | `1,325.636778` IOPS; rerun on `e0180c75` beside all other providers. |
| W26.3b PGlite/R2 | **FAIL / 45% performance qualification** | `766.631418` IOPS; fresh post-`e0180c75` packet required, then further safe optimization if still below target. |
| W26.3c FoundationDB/R2 | **FAIL / 41% performance qualification** | `410.703639` IOPS; preserve durable restart/lockfile/bounded-listing evidence and requalify. |
| W26.3d TiDB/R2 | **FAIL / 29% performance qualification** | `292.606794` IOPS; requalify and isolate remaining provider latency without changing fencing/commit semantics. |
| W26.4–W26.14 implementation and packet controls | **IMPLEMENTED / 100%** | Policy, no-skip, fixed-profile, retention, integrity, one-revision and complete-surface controls remain fail closed; current aggregate correctly failed. |
| W26.15 / P8 1,000 IOPS per drive | **OPEN / 95% implementation, 55% hosted qualification** | All four provider rows and the aggregate must pass on one exact revision; no averaging, skipping or threshold reduction. |
| P14 integration-readiness review | **NO-GO / 40%** | Re-audit the complete terminal packet, then issue an explicit readiness decision; no deployment or release claim. |

#### Historical W26 authority override — published SQLite chunk

The latest shared build-on tip is `origin/main` at
`1e7e75716349f1eff09fd9b77bdeb652c8d0c1b2`. It includes the tested
`1e7e7571` SQLite autocommit fenced-CAS publication optimization: successful
publication is one parameterized conditional UPDATE, while only zero-row
classification opens the `Immediate` transaction. Full locked workspace tests,
strict workspace Clippy, formatting and diff checks passed. SQLite has 11/11
focused package tests passing. Security diff scan
`1b6c1c72-1e6c-4d85-a008-5a8fced9e7c6` completed with complete changed-file
coverage and zero reportable findings. Production remains **NO-GO**.

The latest complete hosted packet is run `35688061634`, selected SHA
`06fc70612b9387a281ab050f711fc878713177ea` before `1e7e7571`, and is therefore
diagnostic rather than qualification of the current tip. Base Ozone job
`106619031921` passed; compositions `106619031684`, TiDB `106619031746`,
FoundationDB `106619031804` and aggregate `106621589545` failed. Provider rows
completed 1,200/1,200 lifecycle operations with zero timeouts and zero cleanup
failures, but measured SQLite/R2 `940.817629`, PGlite/R2 `1,004.326920`,
TiDB/R2 `332.247378` and FoundationDB/R2 `399.594055` IOPS against the hard
1,000 target. The aggregate failed closed on the missing composition
`OZONE_IOPS_PASS` marker. The next matrix must select the exact newly published
tip (or a later exact descendant), and no provider may be skipped or averaged.

| Current W26 work item | Status / completion | Evidence | Remaining action | Provisional estimate | External blocker / gate |
| --- | --- | --- | --- | ---: | --- |
| W26.1–W26.2 / Ozone gateway and block contract | PASS / 100% | Base job `106619031921` passed readiness, policy, block, failure-window, restart/reopen and cleanup markers | Preserve on the next exact-SHA packet | 0–1 h review | Hosted runner, pinned Ozone image and customer topology |
| W26.3a / SQLite and PGlite compositions | Functional pass; performance **open** / 55% hosted qualification | SQLite/R2 `940.817629` IOPS; PGlite/R2 `1,004.326920`; each 1,200/1,200, timeouts 0, cleanup failures 0 | Re-run on `1e7e7571` and close SQLite >=1,000 plus composition/aggregate markers | 1.5–4 d per cycle | Ozone capacity, runner variability, PGlite/R2 latency and W26 SQLite path |
| W26.3c / FoundationDB composition | Functional markers pass; performance **open** / 44% | FoundationDB/R2 `399.594055` IOPS; 1,200/1,200, timeouts 0, cleanup failures 0; bounded-listing/reopen markers passed | Requalify exact current SHA with strict lockfile, durability, cleanup and IOPS gates | 0.5–1.5 d | Hosted FoundationDB image/client and customer topology |
| W26.3d / TiDB composition | Functional markers pass; performance **open** / 44% | TiDB/R2 `332.247378` IOPS; 1,200/1,200, timeouts 0, cleanup failures 0; durable/bounded/reopen markers passed | Requalify after the SQLite chunk; continue provider-specific optimization if still below target | 1–3 d | Hosted TiDB/PD/TiKV, Ozone topology and provider latency |
| W26.4–W26.14 / evidence, security and end-to-end controls | Implemented / 100% W26-owned implementation | Exact-profile/no-skip/artifact-retention/marker/one-revision verifier remains fail closed; aggregate `106621589545` rejected missing `OZONE_IOPS_PASS`; security scan zero findings | Preserve every control and obtain a terminal all-provider aggregate pass on one SHA | 0.5–1.5 d review | CI scheduling, artifact service and hosted fixtures |
| W26.15 / P8 1,000 IOPS per drive | **OPEN / NO-GO** / 95% implementation, 44% hosted qualification | Terminal run is diagnostic: SQLite, TiDB and FoundationDB below target; PGlite just above; no accepted all-provider packet | Dispatch fresh exact-SHA matrix after this push; do not lower target or convert failure to skip | 1.5–4 d per cycle plus external queue | Ozone/provider capacity and hosted CI |
| P14 integration-readiness review | **NO-GO** / 40% | Aggregate failed closed; current code/local/security evidence is not production acceptance | Re-audit terminal end-to-end packet, then state explicit readiness decision | 1–2 d after W26.15 | Customer secure Ozone topology, 99.99%/5-minute RPO/RTO, DR and release stream |

Current hosted dispatch after the published SQLite chunk: manual run
`35689474986` selected exact SHA
`1891c36375296bc3695a9d71233c624cad46445c`. At capture, base Ozone job
`106623193674` and compositions `106623193668` were queued; TiDB
`106623193474` and FoundationDB `106623193564` were in progress; the
aggregate was not yet created. This packet is **NO-GO / not evidence** until
all producers and the aggregate are terminally successful on that one SHA.

Provider-result update for the same run: base Ozone `106623193674` passed;
compositions `106623193668` failed because PGlite/R2 measured `766.631418`
IOPS even though SQLite/R2 measured `1,325.636778`; TiDB `106623193474`
measured `292.606794` IOPS and FoundationDB `106623193564` measured
`410.703639` IOPS. Every provider completed 1,200/1,200 lifecycle operations
with zero timeouts and zero cleanup failures. Aggregate `106625464560` remains
queued, so this is still diagnostic and **NO-GO**; the next implementation
focus is the remaining PGlite/TiDB/FoundationDB performance gap, not lowering
the target or weakening the verifier.

The current W26 source of truth is the detailed [progress ledger](docs/w26-progress-ledger.md).
`origin/main` is `1626d5381625d81696fa28624342f07087593760`, including the
published W26 publication-barrier implementation `c7f0e6d0`, bounded mutation
collection `183660a4`, R2 content-addressed cache, single-flight R2 upload
coalescing `d05548e8`, metadata mutation batching and queue cancellation
hardening, same-revision concurrent-create inode rebasing, the PGlite/TiDB
conditional metadata publication fast path `214b9a6b`, plus the corrected
Ozone content-addressed block test and deduplicated content-addressed cleanup.
The W26-owned code is locally green: focused
`ChunkedFs` tests 20/20, full locked workspace tests, strict workspace Clippy
and diff checks pass, and the Ozone test crate compile-check passes. Security
scans `6d5a7665-d8cd-46b7-a9bd-73fec54f5431`,
`46d4cf32-9d57-4f7e-975f-2616ddc53dc5` and
`85c31e86-1fc8-4a6d-bb95-ef2d956b9ed8` and
`b8f6e846-e497-4ae6-b972-7ae3701ec722` and
`e44de27d-96d8-4330-a831-b995d6458a6c` and
`5a8fcd70-71d4-461b-bb2a-dec8461c22bc` found zero reportable findings within
their local scopes; hosted Ozone TLS/IAM, customer isolation, capacity, 99.99%
availability, five-minute RPO/RTO, backup/DR, native and release gates remain
external.

The production decision remains **NO-GO**. The last terminal W26 packet is
manual run `35683158821` on exact revision
`14dbf2c61b5d86606b165692e0ba1e0e7af545dc`: SQLite/R2 754.59, PGlite/R2
817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17 IOPS, each with
1,200/1,200 successful lifecycle operations, zero timeouts and zero cleanup
failures, but all below the hard 1,000-IOPS target; aggregate job
`106606577835` failed closed on missing `OZONE_IOPS_PASS`. The new provider
publication fast path is included in current merged tip `1626d538`, but the
hosted packet tested its earlier exact revision and remains diagnostic.
Queued, in-progress, canceled, failed or partial jobs are not acceptance
evidence. Do not lower the target or convert failed rows to skips.

- [x] Land isolated, digest-pinned Apache Ozone 2.2.1 gateway harness and
  an Ubuntu CI gate. Main's real Linux-arm64 Docker run passed immutable
  blocks, conditional create/read/write and CAS, concurrent publication,
  service restart/reopen, and owned-resource cleanup. Hosted Linux-amd64
  run `35585066458` passed the Ozone gateway, mixed metadata-provider/Node/CLI,
  and Ozone-backed durable TiDB jobs. The
  all-in-one non-secure test deployment is loopback-only, not production auth
  or replicated-durability acceptance.
- [x] W26.1 Pin Apache Ozone 2.2.1 and architecture-specific container digests;
  the isolated macOS/Linux harness owns loopback readiness, SigV4 bucket
  bootstrap, bounded Docker/test actions, restart/failure windows and
  ownership-checked cleanup. Its single all-in-one service has anonymous
  volumes and no replication, Kerberos/TLS or power-loss durability claim.
- [x] W26.2 Run the immutable block contract through its actual S3 gateway:
  the 2026-09-21 arm64 run passed create-only publication and duplicate
  rejection, ETag stale-read/stale-write rejection and successful CAS,
  concurrent writers, full/range reads, missing objects, binary payloads,
  bounded stopped-gateway failure, service restart/reopen and cleanup via
  `./scripts/test-ozone.sh`. The hosted Linux result remains a separate,
  revision-specific evidence boundary.
- W26.3 partial evidence: SQLite and disk-backed PGlite compositions passed
  on 2026-09-21
  through the real Ozone gateway with seven-byte ChunkedFs chunks, partial
  writes, shrink/extend truncation, ranges, revision CAS, stale-writer
  fencing, fresh reopen and scoped block/metadata cleanup. A separate
  single-node TiDB/Ozone run also passed the direct TiDB contract, the
  ChunkedFs partial/truncate/CAS/stale-fencing/reopen path, ambiguous-commit
  handling and cleanup; it is explicitly not replicated-durability evidence.
- W26.3 FoundationDB durable evidence passed on 2026-09-21 arm64 after the
  harness recovered from the earlier Docker layer-registration failure. The
  run created three fixed-address FoundationDB 7.4.7 server containers with
  three coordinators, separate persistent Docker volumes and `double`/SSD
  configuration. The transactional readiness probe passed both before and
  after restarting replicated node 2; the first and fresh-client Ozone
  compositions emitted `FOUNDATIONDB_RUSTFS_CHUNKED_PASS` and
  `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS`, followed by
  `FOUNDATIONDB_TEST_PASS topology=durable` and owned Ozone cleanup. This is
  real multi-node restart evidence, but remains loopback/non-secure test
  deployment evidence rather than production auth, TLS or power-loss proof;
  hosted CI remains revision-specific and pending.
- A dedicated hosted `ozone-foundationdb` CI job is now wired for the durable
  three-node FoundationDB metadata path over the live Ozone gateway. It must
  reach a terminal pass with the durable restart and cleanup markers before
  the all-feasible-provider production gate can close; no queued, canceled or
  diagnostic result is promoted to evidence.
- Historical hosted W26 validation run `35581168122` on `63dbdbd` passed the actual
  Linux-amd64 Ozone gateway (`ozone`, job `106274147767`) and the mixed
  SQLite/PGlite Ozone composition (`ozone-compositions`, job
  `106274147763`). Its durable TiDB job (`tidb`, `106274147942`) passed the
  three direct TiDB provider tests and the ambiguous-commit test, then timed
  out for 300 seconds at the first `TiDB restart readiness` phase. The
  matching TiDB/RustFS job (`tidb-rustfs`, `106274147791`) passed its direct
  provider, `TIDB_CHUNKED_RUSTFS_SEED_PASS`, and its ambiguous-commit phase,
  then hit the same frontend restart timeout. The published 30-second
  graceful shutdown change (`63dbdbd`) did not resolve this boundary. W26.3
  therefore remained open. The bounded isolation patch was committed as
  `ecc1106`, moving the deliberate dropped-COMMIT failure injection after
  durable restart/reopen so the restart gate is not contaminated by that
  test's intentionally unknown client outcome. Its first rerun
  `35582936271` was canceled when concurrent main commit `210c9cd` landed;
  replacement run `35583109781` was superseded before terminal completion and
  no result from the canceled runs is treated as evidence. Final W26
  acceptance is recorded below.
- [x] W26.3 Extend the real Ozone ChunkedFs composition gate to independent
  TiDB and FoundationDB metadata, including partial writes/truncation,
  revision CAS and stale-writer fencing. SQLite, PGlite, single-node TiDB and
  durable three-node FoundationDB are covered above. Final run `35585066458`
  on `9c098e5` passed hosted `ozone-compositions` job `106286459622` and
  Ozone-backed durable `ozone-tidb` job `106286459540`; the latter emitted
  `TIDB_ACCEPTANCE evidence=durable-multinode-restart`, Ozone integration and
  cleanup markers. Generic durable TiDB job `106286459436` also passed.
- [x] W26.4 Cover Node factories and CLI configuration; add required CI gates
  and document verified versions, limitations and platform evidence. The
  2026-09-21 arm64 live Ozone run passed the Node provider matrix (including
  PGlite-to-S3 partial/truncate/reopen), Node CLI self-test/reopen, matching
  Rust CLI self-test/reopen and owned cleanup; the summary was `pass=7
  skip=1 fail=0`. The CI job now builds the NAPI addon and installs PGlite;
  hosted results remain revision-specific. The same Ozone composition now
  exercises the shipped HTTP server/client binary, range reads, cross-drive
  authorization, graceful reopen and scoped object cleanup. Ozone remains
  loopback-only, non-secure and not production replicated-durability
  acceptance.
- W26.4 follow-up expands the per-drive IOPS qualification matrix to explicit
  SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2 benchmark rows. The Ozone
  composition invokes every row; each configured provider is held to the
  1,000-IOPS minimum and absent provider prerequisites are retained as explicit
  skips. Published code chunk `de9d267` (reconciled/pushed at `414a469`) now
  adds dedicated durable TiDB and FoundationDB Ozone Node paths with early
  positive-integer validation, hard `--min-iops 1000`, retained JSON artifacts
  and provider-specific pass markers. Local benchmark unit/syntax checks,
  no-credential skip output, YAML parsing and a rebuilt current N-API chunked
  lifecycle pass; terminal hosted IOPS artifacts and provider-specific
  topologies remain open.
- [x] W26.7 Add a credential-free production configuration policy gate for the
  customer-deployed Ozone contract. `scripts/verify-w26-ozone-production-config.mjs`
  validates SQLite, PGlite, TiDB and FoundationDB metadata shapes, durable
  provider settings, HTTPS R2 blocks, scoped prefixes and exact external secret
  references without opening a provider connection or printing credential
  values. The `ozone` CI job runs four positive fixtures and requires four
  independent negative fixtures to fail: insecure HTTP, inline credentials,
  unsafe FoundationDB lease authority and a TiDB TLS verification downgrade.
  Local four-positive/four-negative execution passed; implementation commit
  `b8d8fb1` was published in `4d8b5fd` and expansion `7018b59` in reconciled
  tip `6a1b94b`. Focused scans `d74e3e86-2e0a-45cf-9819-e31f428eb5d4` and
  `7addeeb5-4601-4951-aca9-becffb9bd4b9` found zero reportable findings with
  customer/provider controls explicitly deferred. Terminal CI, customer Ozone
  certificates/IAM/rotation and runtime configuration readback remain open.
- [x] W26.8 Make IOPS qualification fail closed on missing requested providers.
  `benchmarks/storage/runner.mjs` adds `--require-configured`; strict mode turns
  skipped requested providers into failed JSON/status with machine-readable
  missing-variable names. The generic Ozone composition selects only SQLite/R2
  and PGlite/R2, while dedicated TiDB/FoundationDB paths are strict. Local
  benchmark unit/missing-provider regression, Node/shell/YAML/diff checks pass.
  Commit `d46e091` was published in `74fe4c7`; focused scan
  `60269206-bb22-4b78-aaf7-f05d16ffcca0` found zero reportable findings.
  Terminal run `35635486040` exercised all four configured provider rows without
  skipping any requested provider. SQLite/R2 measured 61.97 IOPS, PGlite/R2
  63.56, TiDB/R2 14.14 and FoundationDB/R2 23.07 against the hard 1,000
  target; all lifecycle calls completed, but no provider pass marker was
  emitted. Provider TLS/IAM and customer capacity evidence remain open.
- [x] W26.9 Make IOPS pass markers depend on an intact production qualification
  artifact and fixed workload profile. `scripts/verify-w26-ozone-iops-artifact.mjs`
  requires the exact requested provider set, `requireConfigured=true`, 4 KiB
  payloads, 400 iterations, concurrency 64, a target of at least 1,000 IOPS,
  zero skipped/configuration-failed rows, successful cleanup and per-size
  lifecycle success. Generic, TiDB and FoundationDB wrappers reject weakened
  profile/target settings and run the verifier before emitting their pass
  markers. Local unit/verifier negative cases, Node/shell syntax and diff checks
  pass. Commit `66f3670` was published in `00d2b80`; focused scan
  `1d97028f-e153-4b49-9fac-c3a8c1fc1117` found zero reportable findings.
  Terminal hosted artifacts were retained for diagnosis, but the four provider
  rows measured 61.97, 63.56, 14.14 and 23.07 IOPS respectively and were
  correctly rejected before their pass markers. Provider TLS/IAM and customer
  capacity evidence remain open.
- [x] W26.10 Make W26 IOPS artifact retention fail closed. The generic,
  TiDB and FoundationDB Ozone upload steps in `.github/workflows/ci.yml` now
  use `if-no-files-found: error`, so missing expected JSON evidence fails the
  evidence job even on the `always()` upload path. CI YAML parsing, shell
  syntax, benchmark unit tests and diff checks pass. Commit `c0f8370` was
  published in `12ba117`; focused scan
  `18010cad-ed69-4da3-b0a9-57163091e878` found zero reportable findings.
  Hosted artifact retention/access and terminal provider results remain open.
- [x] W26.11 Aggregate the complete Ozone evidence packet on one revision.
  `scripts/verify-w26-ozone-evidence-packet.mjs` validates the exact
  SQLite/PGlite, TiDB and FoundationDB provider sets, clean matching source
  revisions, all positive/negative production-policy markers, provider
  acceptance, Ozone fault/integration and cleanup markers. The CI jobs retain
  policy/base/provider logs and IOPS JSON artifacts, and the `always()`
  `w26-ozone-evidence` job downloads every artifact and fails closed on any
  missing or cross-revision evidence. Local benchmark/evidence tests, Node and
  shell syntax, YAML parsing and diff checks pass. Commit `08f4530` was merged
  with concurrent mainline changes and published at `097ed00`; security scan
  `c67ae8e0-99af-4ade-8284-a612d599b5e4` found zero reportable findings.
  Terminal run `35635486040` retained all four producer artifacts and the
  aggregate job `106458415293` downloaded them, then failed closed because the
  provider logs lacked `OZONE_IOPS_PASS` after missing the hard target. This is
  diagnostic evidence only; customer security/capacity evidence remains open.
- [x] W26.12 Make the IOPS artifact verifier reject incomplete performance
  evidence. It now requires the fixed payload-size mapping, 100% lifecycle
  success, finite elapsed/operation statistics, zero timeout and cleanup
  failures, and exact operation/statistic sample counts. Local positive and
  negative benchmark/evidence tests, Node/shell syntax, YAML parsing and diff
  checks pass. Commit `e875ba6` was merged with concurrent mainline changes
  and published at `1775895`; security scan
  `04c7ba9d-ad9f-40aa-a5b2-d28b9a46a564` found zero reportable findings.
  The terminal artifacts had complete lifecycle and metric maps, but reported
  SQLite/R2 61.97, PGlite/R2 63.56, TiDB/R2 14.14 and FoundationDB/R2 23.07
  IOPS, so the verifier correctly rejected them. Artifact review and customer
  capacity remain open; no performance pass is promoted.
- [x] W26.13 Add the credential-free customer production-rollout contract.
  `scripts/verify-w26-ozone-rollout-contract.mjs` requires the Tier-1
  99.99%-availability/5-minute-RPO/5-minute-RTO envelope, qualified Ozone
  version and secure TLS/SigV4 references, tenant-scoped prefixes, three-node/
  three-replica/three-failure-domain durable topology, all four advertised
  metadata providers, operational controls and customer-owned backup/restore
  drills. The positive declaration and independent weak-RTO and inline-secret
  negative fixtures run without provider connections or credential values; the
  policy log and one-revision evidence packet require their markers. Local
  contract, packet, benchmark, YAML and syntax checks passed. Commit `8d2cfbb`
  was reconciled with concurrent mainline changes and published at `0398d94`;
  security diff scan `33c09a35-77f5-417c-862c-e5848185f50e` found zero
  reportable findings. The PASS is declaration-only; customer topology,
  certificates/IAM/rotation, hosted provider evidence, measured SLO/RPO/RTO and
  backup/DR remain external or pending.
- [x] W26.14 Make the one-revision W26 Ozone evidence packet cover every wired
  end-to-end surface. `scripts/verify-w26-ozone-evidence-packet.mjs` now
  requires gateway health/ready/restart, SQLite/PGlite composition and bounded
  listing, the live Rust CLI, Node provider matrix and Node CLI, remote HTTP
  CLI, TiDB Rust composition plus N-API seed/reopen, and FoundationDB Rust
  composition/restart plus N-API seed/reopen markers. The synthetic packet
  test proves a missing Node CLI marker fails closed. Local benchmark/evidence
  tests, Node/shell syntax, YAML parsing, diff checks and the full locked
  workspace test command `./scripts/cargo-shared test --workspace --all-targets
  --locked` exited 0; environment-gated native/provider rows remain explicit
  skips. Commit
  `20a06b8` was reconciled with concurrent mainline changes and published at
  `0e0454d`; focused security diff scan
  `e4ce1aab-c6cd-44e4-b20d-3130ca357412` found zero reportable findings.
  Replacement qualification run `35641941218` on exact W26 code head
  `88b707ba0e8f91949eec47bca04af16313f56deb` passed the base Ozone job
  `106473112763`, but `ozone-compositions` `106473112920`, `ozone-tidb`
  `106473112915`, `ozone-foundationdb` `106473112409` and aggregate
  `w26-ozone-evidence` `106477164582` failed. Every configured provider
  completed 1,200/1,200 lifecycle operations with zero timeout/cleanup
  failures, but SQLite/R2 85.70, PGlite/R2 94.04, TiDB/R2 14.50 and
  FoundationDB/R2 30.46 IOPS missed the hard 1,000 target; no failed artifact
  or missing pass marker is promoted. The parent workflow remains active only
  for unrelated native jobs. Native/mount qualification, customer
  secure-runtime evidence and measured SLO/RPO/RTO remain open.
- [ ] W26.15 Resolve the terminal per-drive IOPS qualification blocker. The
  initial hosted packet exposed that `ChunkedFs` held its volume-wide async gate
  across remote block I/O and full namespace publication. W26-owned remediation
  now includes atomic `FsDriver::write_file` publication (`96a25f17`), lazy
  atime/EOF handling (`d1bc8fb9`), content-addressed R2 blocks/cache, metadata
  mutation batching (`c4e9a253`, published through `a1cb4ca9`), queue
  hardening (`d152fa1a`) and same-revision concurrent-create inode rebasing
  (`d3d6299e`, published through `059f801e` and included in current origin
  descendants). The provider publication-barrier capability (`c7f0e6d0`) now
  skips only the redundant post-publish metadata flush probe for SQLite,
  PGlite, TiDB and FoundationDB; custom providers remain conservative by
  default and explicit `syncfs` still flushes. The batcher publishes eligible whole-file/unlink mutations once
  under the lease, bounds pending requests at 1,024, drains canceled-runner
  requests with `EIO`, skips closed replies before applying a queued mutation,
  rebases new-file mutations to unique inodes in one publication, preserves
  conflicts for stale existing-file mutations and gives prepared remote
  operations a fixed eight-round cooperative collection window. The PGlite/TiDB
  conditional publication fast path (`214b9a6b`) attempts the parameterized
  lease/fence/revision CAS update before taking the locked classification read;
  missing rows, stale leases, revision conflicts and unexplained zero-row
  outcomes remain fail-closed. The `ChunkedFs` suite in the full run passed
  21/21; full locked workspace tests, strict workspace Clippy and diff checks
  pass. Security scan `6d5a7665-d8cd-46b7-a9bd-73fec54f5431`, cleanup-fix scan
  `85c31e86-1fc8-4a6d-bb95-ef2d956b9ed8` and concurrent-create scan
  `b8f6e846-e497-4ae6-b972-7ae3701ec722` and barrier scan
  `e44de27d-96d8-4330-a831-b995d6458a6c` and
  `47641152-6539-48e1-96b6-bf2201033486` and SQL publication scan
  `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` found zero reportable findings within
  their local scopes and explicitly defer hosted/customer gates. Terminal run
  `35678993571` on exact revision `4b4fe43a` remains diagnostic: SQLite/R2
  802.37, PGlite/R2 614.90, TiDB/R2 86.53 and FoundationDB/R2 360.14 IOPS;
  every row completed 1,200/1,200 operations with zero timeout/cleanup
  failures but failed `IOPS_TARGET_NOT_MET`, and aggregate `106595737360`
  failed closed. The test corrections are `edb6a43e` and `0282df64`, included
  in current origin. The single-flight R2 upload-coalescing implementation
  `d05548e8` and SQL publication fast path `214b9a6b` are included in current
  `origin/main` `1626d538`; terminal run `35683158821` selected exact tested
  revision `14dbf2c6` and remains diagnostic because every provider missed the
  hard target.
  Preserve fencing,
  revision CAS, immutable-block ordering, POSIX semantics, cleanup and
  fail-closed artifact verification. Do not lower the 1,000 target or convert
  failed rows into skips.
- [x] W26.5 Add explicit opt-in immutable-block reconciliation before production
  use. `BlockStore::reconcile` fails closed by default; `ChunkedFs` renews the
  writer lease, rejects zero grace at the coordinator, and protects committed
  namespace roots plus open-unlinked handles; R2/Ozone streams only its
  validated prefix, retains live/recent objects, deletes only aged unreferenced
  blocks and returns scanned/protected/recent/deleted counts. Rust SDK,
  observability and fault-injection wrappers forward the capability, while
  N-API exposes `reconcileBlocks(graceMs)` with positive-range validation and
  `ENOTSUP` for providers without enumeration.
  R2 14/14, ChunkedFs 14/14, SDK 2/2, observability 4/4, strict affected-
  package Clippy, formatting, diff checks and the rebuilt N-API chunked test
  passed locally; hosted provider retention/alert/security evidence remains
  open and shutdown never performs cleanup implicitly.
- [x] W26.6 Bound remote directory enumeration before HTTP response
  materialization. `FsDriver::readdir_bounded` fails closed with `ENOTSUP` by
  default; HTTP `/entries` and directory-file routes request the bound before
  serialization and preserve the existing 413/connection-close behavior.
  Memory, host, chunked, versioned and persisted Rust paths plus observability,
  CLI and native N-API wrappers implement or forward the boundary. Local
  evidence passed for HTTP (8 unit/12 integration), core (11), host (5),
  chunked (14), CLI (44), observability (4) and persistence (4). The optional
  `KeyValueStore::get_keys_bounded` contract now lets capable key-value
  providers enforce the limit before materialization; unstorage exposes
  `getKeysBounded(prefix, maxKeys)` and N-API exposes
  `Filesystem.readdirBounded(path, maxEntries)`, including Node-style
  `EOVERFLOW` wrapping. KV integration (13), N-API Rust tests (16), release
  packaging, unstorage bridge, typecheck, chunked smoke, strict affected-
  package Clippy and formatting passed. Providers without the callback remain
  explicit `ENOTSUP`/pagination gates. Follow-up capable-provider fixtures pass
  bounded overflow/success while legacy/absent callbacks fail closed with
  `ENOTSUP`. The SQLite/R2 and PGlite/R2 Ozone composition tests now also
  assert provider-backed bounded success and `EOVERFLOW`; locked compilation,
  test discovery and strict Clippy pass. The KV adapter forwards the exact
  caller limit and its 13-test regression records provider limits `[2, 3]`.
  The published follow-up `d1c9e44` adds provider-backed bounded success and
  overflow assertions to the TiDB/RustFS and FoundationDB/RustFS composition
  seed/reopen paths. TiDB locked test compilation and strict Clippy passed;
  FoundationDB `cargo check --tests` passed, while local test-binary linking
  is blocked by missing `libfdb_c`. Current hosted results at `414a469` have
  the workflow canceled before jobs started and Live Cloudflare R2 failed;
  neither is promoted; a terminal Ozone/provider marker is still required.
  The published `b80c19c` follow-up enables the feature-built FoundationDB
  Node/N-API client in the dedicated Ozone job. Its seed/reopen test asserts
  bounded success and `EOVERFLOW` and scopes its object prefix below the owned
  Ozone run prefix so the parent cleanup can remove it.
  The published `ef6a876` follow-up adds the same feature-built public
  Node/N-API seed/reopen and bounded success/overflow test to the dedicated
  Ozone TiDB job, with its R2 prefix scoped below the owned Ozone run.
  The published `de9d267` IOPS follow-up, reconciled at `414a469`, adds the
  dedicated TiDB/FoundationDB Ozone hard-threshold benchmark and retained
  artifact paths; `MOUNTX_SOURCE` parity and live hosted provider/performance
  results remain environment-gated checks.

### W26 production-rollout readiness (post-demo; currently NO-GO)

W26 functional integration work is substantially implemented, but production
integration readiness remains a separate open track. Customers own Ozone
deployment, backup/DR and operations;
another stream owns releases. The detailed evidence ledger, product decisions,
provisional estimates and blockers are in
[`docs/w26-progress-ledger.md`](docs/w26-progress-ledger.md). Do not mark a
production/integration gate complete from the demo or from the hosted
qualification packet alone. The machine-checked customer handoff contract is
in [`docs/w26-production-rollout.md`](docs/w26-production-rollout.md); its
PASS is declaration-only. W26 has CI only and no staging environment.

| Gate | Status | Completion | Exit evidence / primary blocker |
| --- | --- | ---: | --- |
| P0 — scope, support matrix, SLO/RPO/RTO, ownership | Scope captured; machine contract gate added; CI baseline open | 68% | Convert the contract into provider/platform assertions and obtain support-owner sign-off |
| P1 — customer Ozone topology contract | Contract documented; deployment external | 20% W26 contract / 0% deployment evidence | `docs/w26-production-rollout.md` and the credential-free validator define secure endpoint, tenant, replication and ownership requirements; customer supplies and operates the actual Ozone topology |
| P2 — all-feasible-provider Ozone CI matrix | Ozone provider matrix and durable-provider hard-threshold paths expanded; terminal evidence pending | 62% | Benchmark rows cover SQLite/R2, PGlite/R2, TiDB/R2 and FoundationDB/R2. The generic Ozone composition now requests its configured SQLite/R2 and PGlite/R2 rows with `--require-configured`; dedicated TiDB/FoundationDB paths are also strict, so a requested but unavailable provider fails the qualification result instead of being promoted as a pass. The artifact verifier independently requires the exact requested provider set and rejects skipped/configuration-failed rows before any wrapper pass marker. The optional key-value bounded-listing contract is locally tested, and the SQLite/PGlite plus `d1c9e44` TiDB/RustFS and FoundationDB/RustFS composition paths contain provider-backed bounded-listing assertions. The feature-built FoundationDB and TiDB Node/N-API Ozone lanes exercise the same contract with scoped cleanup prefixes, and `de9d267` at `414a469` adds dedicated TiDB/FoundationDB Ozone IOPS artifacts and pass-marker paths. The `b8d8fb1` policy fixtures cover all four metadata-provider shapes without provider connections; `66f3670` at `00d2b80` validates the retained artifact. Hosted provider parity, terminal IOPS artifacts and each configured row still need retained evidence |
| P3 — authentication, TLS, secrets and redaction | Local transport and credential-free production-config policy hardened; secure integration open | 60% | Static and runtime R2/Ozone validation rejects malformed, credential-bearing and remote plaintext-HTTP endpoints before client construction; HTTP config/runtime now reject non-loopback binds and require a TLS reverse proxy for remote clients; the all-provider policy gate requires HTTPS R2 blocks, exact external secret references, durable metadata and TLS-required TiDB input without contacting a provider, and its four independent negative fixtures reject HTTP, inline credentials, unsafe FoundationDB authority and TLS verification downgrade; secure endpoint/auth, least privilege, rotation and customer runtime readback remain open |
| P4 — durability/storage failure contract | Lifecycle protection implemented; durability qualification open | 35% | Lease-protected root reconciliation and R2/Ozone scoped cleanup are locally tested; CI client recovery/error evidence plus customer Ozone replication/storage requirements remain open |
| P5 — fencing, ambiguous commit and failover recovery | Lease-protected reconciliation implemented; failover matrix open | 35% | Reconciliation renews the writer lease and never runs implicitly on shutdown; concurrent/retry/failover evidence across feasible Ozone/provider CI lanes remains open |
| P6 — backup, restore and DR | External Ozone/customer dependency; prerequisites documented | 10% W26 contract / 0% W26 DR evidence | The rollout contract requires customer-owned backup/restore, a 30-day restore-drill cadence and measured RPO/RTO evidence; W26 does not build or operate a competing backup system |
| P7 — integration observability and error contract | Local HTTP/OTLP and provider-boundary evidence passed; production integration open | 45% | `mount-rs-http` passed 8 unit and 12 integration tests; OTLP-enabled HTTP passed 11 integration tests; full-feature observability/local collector/exporter-failure tests and CLI observability passed locally, including bounded timeout/connection/listing behavior; `reconcileBlocks` returns scanned/protected/recent/deleted counts and fails closed when unsupported, `readdir_bounded` is forwarded through observability/CLI wrappers, and N-API unstorage preserves `EOVERFLOW`; deployed collector, retry/fencing/recovery dashboards and customer operations handoff remain open |
| P8 — 1,000 IOPS per-drive CI workload | Per-provider hard-threshold gates implemented; latest terminal packet failed hosted qualification; next safe remediation required | 60% | Benchmark measures successful write+read+delete lifecycle IOPS and fails below 1,000 for each configured Ozone-backed metadata provider. The generic Ozone composition requests SQLite/R2 and PGlite/R2 with 4 KiB payloads, 400 iterations, concurrency 64, `--min-iops 1000` and `--require-configured`; dedicated TiDB/FoundationDB Ozone jobs request their own rows with the same strict mode. Wrapper settings cannot lower the target below 1,000 or weaken the fixed profile. The verifier requires the exact provider set, zero skipped/configuration-failed rows, successful cleanup, per-size lifecycle success and a valid retained JSON artifact before a pass marker is emitted. Terminal run `35683158821` on exact revision `14dbf2c6` recorded SQLite/R2 754.59, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17 IOPS with 1,200/1,200 successful lifecycle operations and zero timeout/cleanup failures, but every row failed the hard target and aggregate `106606577835` failed closed on missing `OZONE_IOPS_PASS`. Bounded mutation-window implementation `183660a4`, single-flight R2 upload coalescing `d05548e8` and SQL publication fast path `214b9a6b` are in current `origin/main` `69c684c7`; the next run follows a new safe performance chunk on its exact pushed SHA. Local live Ozone evidence outside hosted CI remains unavailable. |
| P9 — compatibility handoff | External release/deployment dependency | 0% W26 migration evidence | W26 supplies compatibility notes; release stream owns promotion/rollback |
| P10 — security, privacy, tenancy and audit | Published-tip local security review complete; hosted/customer security remains open | 86% | Delegated architecture threat model covers provider, credential, prefix, client/native and customer/Ozone boundaries; endpoint/TLS/redaction/auth/isolation, loopback-only bind, connection-cap, stalled-request and pre-materialization directory entry/response-byte limit tests pass locally; SDK `StoreConfig` debug output now redacts provider credentials; strict affected-workspace check is green; scoped reconciliation fails closed for unsupported providers, renews the writer lease, protects live/open-unlinked roots, validates block IDs and deletes only aged objects under the configured prefix; `FsDriver::readdir_bounded` fails closed for unsupported providers and is implemented/forwarded for built-in Rust paths, while KV can opt into `get_keys_bounded` and N-API maps provider overflow back to Node `EOVERFLOW`. The credential-free Ozone policy gate covers all four metadata-provider shapes, HTTPS R2, scoped prefixes, durable settings, external secret references and TiDB TLS options with local positive/negative execution; expanded fixtures independently reject inline credentials, unsafe FoundationDB authority and TLS verification downgrade. The SQL publication diff scan `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` reviewed both changed provider files with complete coverage and zero reportable findings; prior scans also found zero within their stated scopes. The reviews explicitly record the atime, mutation and provider publication crash boundaries and defer hosted/provider/customer security. The artifact verifier rejects malformed/weak profiles without printing artifact contents, and missing IOPS artifacts now fail CI. Customer Ozone TLS/IAM/rotation, provider-native allocation, dependency/native provenance, 99.99%/recovery drills and production operations remain deferred. |
| P11 — end-to-end client/platform matrix | HTTP path and built-in/KV/durable-provider bounded-listing contracts added to Ozone CI; full matrix open | 46% | Rust/Node/CLI and the shipped HTTP server/client path now run through the Ozone composition gate with scoped cleanup; built-in Rust providers enforce the directory bound before response materialization; SQLite/PGlite plus `d1c9e44` TiDB/RustFS and FoundationDB/RustFS composition tests assert provider-backed bounded success and `EOVERFLOW`; the feature-built FoundationDB and TiDB Node/N-API Ozone lanes from `b80c19c` and `ef6a876` assert the same contract with scoped cleanup prefixes; unstorage/N-API has a provider callback, public bounded API, TypeScript declaration and Node error-shape test; TiDB test compilation/Clippy and FoundationDB `cargo check --tests` pass locally, but live provider/Ozone execution is hosted-only and FoundationDB local linking lacks `libfdb_c`; native mounts, providers without the callback, `MOUNTX_SOURCE` parity, every advertised platform and terminal hosted evidence remain open |
| P11 follow-up — complete-surface packet enforcement | Implemented; terminal hosted packet remains open | 60% W26-owned implementation/qualification | Published `20a06b8` at `0e0454d`; the aggregate verifier now requires the currently wired Ozone gateway, composition, Rust/Node/HTTP CLI, TiDB N-API and FoundationDB N-API/restart markers. This closes the local evidence-contract gap but not hosted execution, native mounts or customer runtime proof |
| P12 — release handoff | External release stream | 0% W26 release evidence | Reproducible CI inputs and evidence markers only; no W26 canary claim |
| P13 — incident/failover handoff | External customer/Ozone operations | 0% W26 rehearsal evidence | CI fault cases plus customer operator scenarios for 99.99%/5-minute RTO |
| P14 — final W26 integration-readiness review | NO-GO review active; W26.15 remains open after terminal run `35683158821` | 39% | Terminal run `35683158821` is audited: base Ozone passed, SQLite/R2 754.59, PGlite/R2 817.10, TiDB/R2 143.04 and FoundationDB/R2 345.17 IOPS all missed the hard target despite zero timeout/cleanup failures, and aggregate `106606577835` failed closed on missing `OZONE_IOPS_PASS`. Publication-barrier implementation `c7f0e6d0`, bounded mutation window `183660a4`, single-flight R2 upload coalescing `d05548e8`, mutation batching/cache/queue hardening, same-revision concurrent-create rebasing, SQL publication fast path `214b9a6b`, the corrected Ozone block-contract test and deduplicated cleanup are locally tested; security scans `e44de27d-96d8-4330-a831-b995d6458a6c`, `47641152-6539-48e1-96b6-bf2201033486`, `739f4f5b-8cc4-4154-93f7-9e486745eab1` and `5a8fcd70-71d4-461b-bb2a-dec8461c22bc` report zero findings within complete local scopes. The next safe performance chunk must be locally tested, security-reviewed, pushed and requalified before W26 can hand off an integration-ready decision. A terminal one-revision all-provider/performance/security/end-to-end audit is required before an integration-ready handoff. |

The production/integration track is **0/15 terminal gates accepted**. Its
current provisional W26 planning range is **31–82 engineering/contract days
plus external waits**; this is not a delivery commitment. Customer Ozone
deployment, backup/DR, operations and release execution are excluded from that
W26 estimate.

## W27 — Native Windows support and CI

- [ ] W27.1 Run native `windows-latest` Rust formatting, all-feature Clippy and
  workspace tests; repair platform compilation and behavior failures rather
  than adding continue-on-error. Windows CI is added, not yet verified green.
- [ ] W27.2 Build the Windows napi-rs addon and run native Node factories,
  chunked storage, just-bash/Mastra consumers and TypeScript checks in CI.
- [ ] W27.3 Expand to pinned mountx differential parity, real SQLite/PGlite,
  authenticated R2 and other required backend/service tests on Windows.
- [ ] W27.4 Qualify path/drive-letter handling, open-handle deletion, locks,
  process/service lifecycle, restart recovery and artifact installation.
- [ ] W27.5 Define and implement Windows mount support separately from Unix
  FUSE/NFS and macOS FSKit. Explicit unsupported operations are not proof of
  Windows mounting acceptance; retain capability and evidence matrices.
- [x] `d6b80f4` adds the HostFs Windows packet for unprivileged symlink flags,
  rooted absolute-target inference and metadata/lifecycle behavior. The local
  suite passed 14/14 where runnable; native `windows-latest` execution remains
  required.
- [x] `ccaf8f5` + `4cdeb58` correct the Windows WAL shared-memory mapping mask
  and add regression coverage; local focused tests pass, but `windows-latest`
  must rerun before W27.1/W27.2 can be checked.

## W28 — Deterministic fault injection

- [x] Land isolated storage-wrapper crate with explicit occurrence-based plans,
  redacted pending/completed/cancelled traces and optional delays. Main passed
  seven all-feature tests and strict Clippy locally. Seed labels evidence, not
  randomized scheduling. Dedicated Linux/macOS/Windows CI added; hosted results
  remain pending.
- [x] Register the crate in the root workspace and shared lockfile. Add four
  composed filesystem tests, each exercised with memory and SQLite metadata/block
  stores: pre-write ENOSPC, lost publish acknowledgement, failed metadata flush,
  and invalidated writer lease. Assert namespace/bytes on reopen and explicit
  temporary-directory cleanup. Local all-feature suite: 11 passed. These are
  in-process wrapper faults, not power-loss or remote-service qualification.
- [ ] W28.1 Add a separate minimal-dependency fault-injection crate with explicit
  opt-in plans, operation/occurrence selectors, seeded replay and event evidence.
  Wrap metadata and block stores without changing production defaults.
- [ ] W28.2 Cover before/after-operation failures, lost acknowledgments, IO/full/
  permission errors, delays/timeouts, missing/corrupt/torn blocks, lease expiry,
  stale fencing, CAS conflicts and failed sync barriers. Record unsupported
  hooks and add transport/process controls rather than pretending wrappers
  simulate kernel, network or power-loss behavior.
- [ ] W28.3 Wire plans through test CLI/Node/VFS/HTTP entry points; isolate test
  resources, redact data/credentials, bound execution and verify cleanup.
- [ ] W28.4 Sweep defined fault points in the SQLite reliability matrix, retain
  seed/plan and minimized failing trace, check integrity plus exact transaction
  history and acknowledged-commit durability for the selected configuration.
- [ ] W28.5 Require bounded PR fault suites and broader scheduled matrices on
  Linux/macOS/Windows; publish coverage and remaining gaps, not an unbounded
  claim that every possible fault has been tested.

## W29 — User-configurable lifecycle hooks (later)

- [ ] W29.1 Define an extensible event catalog for files/folders: creation,
  opening/closing, writes, truncation, metadata changes, rename/move and deletion;
  and drive/server/connection lifecycle: starting, started, stopping, stopped,
  connection opened, dropped, reconnecting, reconnected and failed. Distinguish
  requested operations, successful completion and failure events.
- [ ] W29.2 Design registration, filtering by drive/path/event, removal and
  event payloads for user-supplied after-event hooks. Review Rust, Node, CLI
  configuration and mount-free HTTP integration surfaces; keep integrations
  separate from core and dependency costs minimal.
- [ ] W29.3 Specify ordering, concurrency, delivery/retry/deduplication behavior,
  cancellation, bounded queues/backpressure and shutdown draining. Clearly
  define after-write versus after-durable-commit; do not imply exactly-once
  delivery or crash-surviving hooks without an implemented durable mechanism.
- [ ] W29.4 Define hook timeouts, error isolation, reentrancy/recursive-event
  prevention and permissions. Hooks must not silently corrupt file operations,
  SQLite durability, lease/fencing or transaction outcomes. Redact credentials
  and avoid exposing file contents by default.
- [ ] W29.5 Implement the agreed API later and test event payloads/order,
  registration/removal, hook failures, connection drops/reconnects, startup and
  shutdown, concurrent operations and process failures across supported entry
  points/backends/platforms. Integrate W28 fault injection and document gaps.

## W30 — OpenTelemetry observability (later)

### W30 qualification infrastructure chunk (2026-09-22)

- [x] Added the `observability-qualification` CI matrix for Ubuntu, macOS, and
  Windows. It runs the locked cross-platform command set through
  `scripts/cargo-shared`, including the observability crate lint gate.
- [ ] W30.1–W30.5 acceptance remains open until completed matrix runs retain
  exact revision, host identity, command output, and collector-boundary
  evidence; workflow configuration alone is not a platform PASS.

- [ ] W30.1 Define trace spans, metric instruments and structured log events
  across filesystem operations, metadata/block providers, chunking/compression,
  SQLite VFS, mounts, Node, CLI and HTTP services. Include lifecycle, latency,
  errors, retries, lease/fencing, cache behavior and durability boundaries.
- [ ] W30.2 Implement optional instrumentation and a separate integration crate
  where practical; keep exporters/SDK dependencies out of the minimal core and
  preserve a low-overhead disabled mode. Applications own provider/exporter setup.
- [ ] W30.3 Propagate context across Rust async tasks, napi-rs/Node, HTTP and
  backend calls; correlate traces and logs. Define sampling, resource identity,
  versioned event/attribute conventions and bounded metric cardinality.
- [ ] W30.4 Provide configurable OTLP export for traces, metrics and logs with
  bounded queues, timeouts, flush/shutdown and exporter-failure isolation. Avoid
  secrets, file contents and unbounded/sensitive paths in telemetry by default.
- [ ] W30.5 Add collector-backed integration tests for all three signals,
  context propagation, redaction, disabled mode, dropped connections, exporter
  failures and shutdown. Benchmark overhead and qualify macOS/Linux/Windows;
  document setup and dashboards/examples without claiming unverified coverage.

Implementation note (2026-09-21): the separate `mount-rs-observability` crate,
feature-gated HTTP/SDK/Node/CLI integration, bounded redaction contract, W3C
carrier helpers, signal-specific OTLP/HTTP paths, blocking-client exporter
setup, and deterministic tests are in the current implementation. W30.5
evidence on macOS arm64 (macOS 26.5.1, Darwin 25.5.0, Rust/Cargo 1.95.0)
against the then-current `origin/main` base `af31ba7`:

- `cargo fmt --all -- --check`: PASS.
- `cargo check --workspace --locked --offline`: PASS.
- `cargo test -p mount-rs-observability --all-features --locked --offline`:
  5 unit tests, one loopback collector test, and one exporter-failure/shutdown
  test PASS. The collector received non-empty `/v1/traces`, `/v1/metrics`,
  and `/v1/logs` payloads and the test found no raw secret path bytes.
- `cargo test -p mount-rs-sdk --features observability --locked --offline`:
  3 PASS; `cargo test -p mount-rs-http --features observability-otlp
  --test http_integration --locked --offline`: 7 PASS with loopback access;
  `cargo test -p mount-rs-cli --features observability --locked --offline`:
  39 unit + 9 CLI + 2 HTTP subprocess + 1 native artifact test PASS, with 3
  documented native/remote tests ignored; N-API observability check PASS.
- `cargo clippy -p mount-rs-observability --all-features --all-targets
  --locked --offline -- -D warnings`: PASS. The optimized no-exporter facade
  benchmark ran 20,000 operations and recorded baseline 2 ns/op, disabled
  9 ns/op, enabled 59 ns/op on this host; these are comparative observations,
  not a cross-platform SLO.

The local collector and failure tests do not prove reachability of an external
collector. Linux and Windows remain explicitly unverified, and the platform
runbook is `docs/w30.5-platform-qualification.md`.

## W31 — Per-drive mounts from one backing datastore (future)

This is a future workstream for exposing multiple independently mounted
drives, each with its own drive identity and namespace, while sharing one
metadata/block backing datastore. It must not be implemented by treating a
shared provider handle as an implicit global filesystem or by weakening
cross-drive isolation.

- [ ] W31.1 Define drive identity, namespace roots, ownership, quotas and
  lifecycle when several drives share one metadata and/or block provider.
- [ ] W31.2 Specify metadata schema/indexing and block reachability so each
  drive can be opened, snapshotted, copied, retained and garbage-collected
  independently without deleting another drive's live blocks.
- [ ] W31.3 Define locking, leases, revision/CAS boundaries and crash/reopen
  behavior for concurrent writers on the same drive and on different drives.
- [ ] W31.4 Expose the drive registry through the CLI config, HTTP multi-drive
  API, Node/mount-free APIs, FUSE/NFS/9P/FSKit transports and future native
  mounts without silently collapsing all drives into one namespace.
- [ ] W31.5 Test per-drive authorization, path isolation, shared-store
  cleanup, quotas, provider failures, restart, versioned views and future
  distributed-cache/copy-on-write interactions across memory, SQLite, PGlite,
  R2/RustFS and the database metadata providers.
- [ ] W31.6 Benchmark shared-store efficiency against separate backing stores;
  document supported combinations, migration/format versioning and safe
  deletion rules before enabling automatic cleanup.

## Recent landed chunks

| Commit | Scope | Evidence boundary |
| --- | --- | --- |
| 2026-09-22 WebDAV current rebuilt oracle parity refresh | Current rebuilt package passed the pinned barrel and supported session/member differentials with `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921`; `scripts/check-http-parity.mjs` passed 40 paired S3+WebDAV cases | Local pinned-oracle and loopback parity only; hosted current-package qualification is live but native WebDAV jobs remain queued, and live-provider, power-loss/crash durability, durable locks, and stronger same-resource ordering remain open |
| 2026-09-22 WebDAV current N-API package/build refresh | Current release addon built with `CI=true CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-current-napi pnpm --dir integrations/mount-rs-napi build`; WebDAV-only N-API phase, typecheck, lifecycle, 64-pair session/network concurrency, NodeFs/SQLite provider/reopen/crash/in-flight recovery, structural durability, Rust 41/41, strict Clippy, formatting, and diff checks passed at `c57e2ea3` | Manual current-package run `35714570430` at `92a6539e6a91d67a811b77cf688fc4ad2177f858` is live: macOS arm64/Intel Node jobs passed, while Ubuntu Node and both native WebDAV jobs remained queued at the audit; no terminal current hosted WebDAV result is claimable, and live-provider, power-loss/crash durability, durable locks, and stronger same-resource ordering remain open |
| 2026-09-22 WebDAV live-provider admission refresh | Latest completed protected AWS run `35712627727` stopped at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; latest completed R2 run `35713249887` stopped at `R2 CI monthly run cap already exceeded: count=362 limit=20` and skipped live integration, and newer R2 run `35713639700` failed the same admission at `count=363 limit=20` with live integration skipped | No live AWS/R2 service PASS is claimable; protected configuration, R2 budget reset, power-loss/crash durability, durable locks, and stronger same-resource ordering remain open |
| 2026-09-22 WebDAV manual hosted native qualification | The non-canceling manual CI run `35711153805` at exact SHA `232443e133abf7c8f20f6ffec5a2a22a747270e0` passed native WebDAV on macOS and Ubuntu (jobs `106691801834` and `106691802073`); no WebDAV files changed through current `fba61979f1f6c9858026cd5ebc4c5d3d357f366b` | Hosted native mounted-I/O is closed for this slice only; the aggregate run is nonterminal on unrelated jobs, and live-provider, power-loss/crash durability, durable-lock, and stronger same-resource-ordering gates remain open |
| 2026-09-22 WebDAV current mainline workspace baseline | Current mainline has no WebDAV changes after the tested `515bdc00792a62403b0ee7b94e904434d067f43b` base; `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-workspace-515 ./scripts/cargo-shared test --workspace --all-targets --locked` completed successfully, including WebDAV 41/41 with the privileged native mount target explicitly ignored | Fresh local workspace evidence only; it does not promote local tests to hosted, live-provider, power-loss, durable-lock, crash/restart, or stronger same-resource-ordering acceptance |
| 2026-09-22 WebDAV conditional ETag whitespace compatibility | Preserve the pinned entity-tag grammar at the `If-Match`/`If-None-Match` boundary: trim list-member OWS, but do not trim after `W/`, preventing malformed `W/ "etag"` from becoming a weak match | Final exact-head focused 1/1 and full 41/41 tests plus workspace warning-denied Clippy used `CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-webdav-final-40c79a5b` through `./scripts/cargo-shared`; formatting, diff checks, and the pinned 40-case HTTP differential also pass; hosted/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| 2026-09-22 WebDAV HTTP-date leap-second compatibility | Accept RFC 9110 `:60` seconds by normalizing only the valid seconds field to `:59`, matching the pinned oracle while preserving rejection of invalid minutes and malformed dates | Focused date-form regression and full WebDAV target 40/40, warning-denied workspace Clippy, formatting, diff checks, and the pinned 40-case S3+WebDAV differential pass; hosted/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| 2026-09-22 WebDAV If-parser UTF-8 boundary safety | Probe the public `Not` grammar with UTF-8-safe access so malformed non-ASCII input such as `(éé)` returns `None` instead of panicking at a code-point boundary | Focused regression reproduced the pre-fix panic and now passes; full WebDAV target 39/39, warning-denied workspace Clippy, formatting, diff checks, and the pinned 40-case S3+WebDAV differential pass; hosted/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| 2026-09-22 WebDAV XML parser character validation | Reject invalid UTF-8 and raw XML-invalid characters before tree construction, matching the pinned `invalid-character` refusal instead of preserving controls in `XmlNode` text | Focused raw-NUL parser regression and full WebDAV target 38/38, warning-denied workspace Clippy, formatting, diff checks, and the pinned 40-case S3+WebDAV differential pass; hosted/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| 2026-09-22 WebDAV XML serializer safety/parity | Escape CR as `&#13;` and replace XML-invalid controls with U+FFFD in text and namespace values, matching the pinned XML codec while retaining markup escaping | Focused serializer fixture and full WebDAV target 37/37, warning-denied workspace Clippy, formatting, diff checks, and the pinned 40-case S3+WebDAV differential pass; hosted/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| 2026-09-22 WebDAV Unicode lock-token parser safety | Replace byte-offset `Lock-Token` parsing with UTF-8-safe delimiter handling; accept the pinned oracle's Unicode token payloads without panic while retaining malformed-angle rejection | Focused protocol fixture and full WebDAV target 37/37, warning-denied workspace Clippy, formatting, diff checks, and the pinned 40-case S3+WebDAV differential pass; hosted/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| 2026-09-22 WebDAV lock coverage ordering | Preserve grant order for public covering/within lookups so lock-discovery, locked-member multistatus, and first-conflict selection do not depend on HashMap iteration | Full WebDAV target 37/37, warning-denied workspace Clippy, formatting, diff checks, and the pinned 40-case S3+WebDAV differential pass; hosted/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| 2026-09-22 WebDAV declared-length preflight observability | Route declared Content-Length size-limit rejections through session request/reply/error bookkeeping and the request-level error hook before closing the connection; preserve bodyless HEAD responses | Full WebDAV target 36/36, warning-denied workspace Clippy, formatting, diff checks, and the pinned 40-case S3+WebDAV differential pass; hosted/provider, authentication-ordering, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| `2026-09-22 WebDAV streamed mutation-handle cancellation` | Close mutation-side provider handles when streamed PUT or shared file-transfer futures are cancelled during body polling or provider I/O | Full WebDAV target 35/35, warning-denied Clippy, formatting, and diff checks pass; hosted lifecycle/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| `2026-09-22 WebDAV poisoned lock-table fail-closed behavior` | Propagate WebDAV lock-table mutex poisoning as a server error across lock-dependent request paths, while retaining poisoned lock state for public snapshots | Full WebDAV target 34/34, warning-denied Clippy, formatting, and diff checks pass; hosted lifecycle/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| `2026-09-22 WebDAV transport-neutral body cancellation` | Close the provider file handle when a direct `WebdavBody::into_bytes()` consumer is cancelled during a pending response read | Full WebDAV target 33/33, warning-denied Clippy, formatting, and diff checks pass; hosted lifecycle/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| `2026-09-22 WebDAV streamed-response shutdown lifecycle` | Make streamed file response tasks observe server shutdown, participate in bounded drain, and close provider handles after stalled-read cancellation | Full WebDAV target 32/32, warning-denied Clippy, formatting, and diff checks pass; hosted lifecycle/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| `2026-09-22 WebDAV streamed-response fault evidence` | Prove that a short driver read fails an HTTP response body and reaches the peer-qualified transport-error hook | Focused WebDAV target 28/28, warning-denied Clippy and formatting pass; hosted lifecycle/provider, power-loss, durable-lock, crash/restart and same-resource ordering remain open |
| `2026-09-22 WebDAV body-stream contract clarification` | Align the public Rust request-body documentation with the published fail-closed drain behavior | Documentation-only clarification; the 28/28 WebDAV, warning-denied Clippy and formatting evidence remains the governing local result, while hosted/provider and durability gates remain open |
| `2026-09-22 WebDAV unread-body fault packet` | Preserve framing after drainable 413 limits, but report non-recoverable unread-body faults once and close the HTTP connection | Focused WebDAV target 27/27, warning-denied Clippy, formatting, rebuilt N-API addon, generated typecheck, WebDAV-only host-enabled integration, and structural WebDAV regression pass; hosted/provider, power-loss, durable-lock, crash/restart, and same-resource ordering remain open |
| `2026-09-22 WebDAV structural bounded-listing packet` | Forward the optional structural N-API `FsDriver.readdirBounded(path, maxEntries)` callback and reject over-large callback results as `EOVERFLOW`; exercise bounded PROPFIND, recursive COPY/DELETE, provider overflow, and the explicit absent-capability boundary | Release addon, generated typecheck, WebDAV-only host-enabled server phase, and focused structural WebDAV regression pass; hosted package/provider qualification, power-loss ordering, durable locks, crash/power-loss restart, and same-resource ordering remain open |
| `2026-09-22 FUSE boundary packet` | Reject unsafe and transport-owned `MountOptions.mount_options` tokens before native Linux FUSE mount/helper invocation | Focused `mount-rs-fuse` all-target tests and strict Clippy passed on macOS; hosted `/dev/fuse`, crash/concurrency, callback-event, and FSKit gates remain open |
| `2026-09-22 FUSE forced-teardown packet` | Make forced native session-task cancellation publish inactive/closed state and wake `wait_closed()` observers | Host FUSE tests, host/Linux-target strict Clippy, Linux-target test check, formatting and diff checks pass; hosted `/dev/fuse` forced-unmount, callback-event, crash/restart and durability gates remain open |
| `2026-09-22 FUSE forced-unmount deadline packet` (published as `987c593bc08adfb161a55a7a9eee27ff82606310`) | Share the forced `umount`/lazy-detach deadline with final session-task draining so bounded teardown does not add a third full timeout | Host FUSE all-target tests, host/Linux-target strict Clippy, Linux-target test check, formatting and diff checks pass; exact-SHA CI run `35648821996` and Fault injection run `35648821873` are pending, while the Linux-gated timing test and hosted `/dev/fuse` forced-unmount and broader lifecycle gates remain open |
| `2026-09-22 FUSE active-state packet` (published as `8ddf48febaedbd78dc22d889e8f3c822a4e6ad45`) | Publish `active == false` at the start of teardown and restore it only for a retryable helper failure that leaves the kernel mount live | Host FUSE all-target tests, host/Linux-target strict Clippy, Linux-target test check, formatting and diff checks pass; exact-SHA CI run `35649715601` is pending and Fault injection run `35649715727` is queued, so the Linux-gated runtime regression and hosted native close-race/callback, crash/restart and durability gates remain open |
| `2026-09-22 FUSE forced-teardown callback packet` (published as `0d06abb10899f307ce83cd5fa198bab9c5f156a9`) | Report a forced graceful-unmount timeout once through the owned `Task` transport-error hook, preserving callback-panic isolation and existing terminal-state semantics | Host FUSE all-target tests, host/Linux-target strict Clippy, Linux-target test check, formatting and diff checks pass; exact-SHA CI workflow-dispatch run `35650347479` is queued, push CI run `35650323951` was cancelled, and Fault injection run `35650324040` is in progress, so the Linux-gated callback assertion and hosted forced-unmount/fault, crash/restart and durability execution remain open |
| `2026-09-22 FUSE mount-source parity packet` (published as `2a979191d1ef5db37be3a9a3a4bbb2c3efe44457`) | Expose the configured FUSE `fsname` through the automatic facade's shared `source` property and return no source for unsupported-platform FUSE objects | FUSE and automatic-facade host tests, host/Linux-target strict Clippy, Linux-target checks, formatting and diff checks pass; exact-SHA Fault injection run `35652258903`, CI run `35652258837`, and W08 release targets run `35652258840` are pending, while W04 production policy run `35652258913` succeeded but is unrelated, so the Linux-gated source regression and hosted native source/lifecycle execution remain open |
| `2026-09-22 FUSE frame-floor and API-scope packet` | Reject native `max_frame` below the modern `FUSE_WRITE` header plus one page (`4176` bytes), and record explicit supported-scope decisions for the remaining upstream FUSE mount options, callbacks, root members, and crash/restart ownership in `transports/mount-rs-fuse/README.md` | Host all-target FUSE tests (14 unit, 6 INIT, 0 native, 6 notify/record, 11 protocol, 20 session, 4 sync-barrier), host strict Clippy, Linux-target check/strict Clippy, formatting, and diff checks pass; the Linux-only validation regression is compile-checked but not runnable on Darwin, while hosted `/dev/fuse` callback, lifecycle, crash/restart and durability evidence remain open |
| `2026-09-22 FUSE forced-unmount cancellation ordering` | After graceful native unmount reaches its deadline, request session cancellation before the lazy-detach helper so blocked backend work cannot deadlock forced teardown; add a Linux-gated helper-ordering regression | Prior hosted native-FUSE job `106523259210` in run `35657075892` passed the panic callback case but timed out both round-trip and blocked-read unmounts; host FUSE tests, host/Linux-target strict Clippy, Linux-target check, formatting and diff checks pass for the fix, while the corrected hosted native run remains required and W01 stays NO-GO |
| `2026-09-22 FUSE forced-unmount descriptor-drain ordering` | Drain or abort the stopped FUSE session task before starting lazy detach, preserving one shared forced-teardown deadline so the helper cannot wait on an owned device descriptor | Hosted job `106530560678` in run `35659287961` still timed out both unmount cases after the first ordering fix; host FUSE tests, host/Linux-target strict Clippy, Linux-target check, formatting, and diff checks pass for this follow-up, while a new hosted native rerun remains required and W01 stays NO-GO |
| `2026-09-22 FUSE hosted callback qualification` | Manual run `35650347479` at `0d06abb` passed the FUSE prerequisite and backend-panic callback/close scenario, but ordinary and blocked-read native unmount timed out in job `106500945410` while the unrelated matrix was still in progress; no native acceptance is promoted | The current destroy-boundary packet is the locally verified follow-up for the two remaining hosted unmount failures; exact-tip hosted close-race, callback, crash/restart, concurrency, locks and durability evidence remain open |
| `2026-09-22 FUSE native destroy teardown packet` | Treat Linux kernel `FUSE_DESTROY` as a terminal no-reply boundary, abort and drain in-flight positional-read workers, and update the Unix-stream regression to require bounded session close | Hosted run `35647249560` / native-FUSE job `106491113021` reached all three native scenarios but failed their `Mounted::unmount()` deadlines before overall cancellation; host all-target tests/Clippy and Linux-target test compilation pass for the fix, while exact-tip hosted unmount, callback, crash/restart, concurrency, locks and durability evidence remain open |
| `2026-09-22 FUSE bounded terminal drain packet` | Bound the terminal positional-read worker drain to one second after aborting workers, allowing native device release even if a backend future is not cancellation-cooperative; add a Linux-gated blocking-worker regression | Host FUSE tests, host/Linux-target strict Clippy, Linux-target test compilation, formatting and diff checks pass; exact-tip hosted unmount, callback, crash/restart, concurrency, locks and durability evidence remain open |
| `2026-09-22 FUSE session cleanup bound packet` | Bound kernel-initiated `FUSE_DESTROY` handle cleanup by the configured unmount timeout and report a single owned `Task` transport error when backend `close()` or terminal read-worker cancellation does not finish | Host FUSE tests, host/Linux-target strict Clippy, Linux-target test compilation, formatting and diff checks pass; exact-tip hosted unmount, callback, crash/restart, concurrency, locks and durability evidence remain open |
| `2026-09-22 FUSE saturated read control-plane packet` | Replace the unbounded native read-permit wait with a bounded pending queue and `EAGAIN` overflow so `FUSE_DESTROY` and `FUSE_INTERRUPT` remain serviceable at the 16-worker concurrency limit; add a Linux-gated datagram regression for 16 blocked reads plus a queued 17th read | Host FUSE tests, host/Linux-target strict Clippy, Linux-target test compilation, formatting and diff checks pass; exact-tip hosted unmount, callback, crash/restart, concurrency, locks and durability evidence remain open |
| `2026-09-22 FUSE targeted interrupt packet` | Cancel only the requested positional-read worker on `FUSE_INTERRUPT`, leaving unrelated blocked reads and the session alive; extend the Linux-gated interrupt regression to prove this control-plane behavior | Host FUSE tests, host/Linux-target strict Clippy, Linux-target test compilation, formatting and diff checks pass; exact-tip hosted interrupt, unmount, callback, crash/restart, concurrency, locks and durability evidence remain open |
| `2026-09-22 FUSE unconditional terminal drain packet` | Always abort and boundedly drain registered positional-read workers after loop termination, retaining the first transport error instead of skipping cleanup when another worker or protocol path already failed | Host FUSE tests, host/Linux-target strict Clippy, Linux-target test compilation, formatting and diff checks pass; exact-tip hosted interrupt, unmount, callback, crash/restart, concurrency, locks and durability evidence remain open |
| `2026-09-22 FUSE forced-unmount mount-presence packet` | Recheck `/proc/self/mounts` after forced `umount`/lazy-detach and preserve `mounted=true` when the kernel mount remains present; keep the closed session inactive and retryable instead of falsely reporting an absent mount; add a Linux-gated stuck-helper regression against `/` | Host FUSE all-target tests (14 unit, 6 INIT, 0 native, 6 notify/record, 11 protocol, 20 session, 4 sync-barrier), formatting/diff checks, and Linux-target strict Clippy pass; the Linux-only regression is compiled but not run on this macOS host, while hosted forced-unmount, callback, crash/restart, concurrency, locks and durability evidence remain external |
| `2026-09-22 FUSE blocked-read stop-notify packet` | Hosted run `35662346488` at `b27dd2b` passed round-trip and backend-panic callback but failed only the blocked-read unmount in native-FUSE job `106540264683`; retain one stop-notify permit alongside `notify_waiters()` and add a Linux-gated blocked-positional-read stop regression | Host FUSE all-target tests, formatting/diff checks, and Linux-target strict Clippy pass; exact-tip hosted rerun must prove the blocked-read unmount before native lifecycle acceptance, and W01 remains NO-GO |
| `2026-09-22 FUSE forced-unmount grace packet` | The same hosted timeout also showed that draining a session stuck in `/dev/fuse` for the full second timeout delays lazy detach beyond the native test's 15-second bound; add a 250ms stop grace, abort the owner task to close the descriptor, then begin a fresh forced-unmount deadline | Host FUSE tests, formatting/diff checks, and Linux-target strict Clippy pass; exact-tip hosted native-FUSE rerun remains required for ordinary/blocked unmount, callback, crash/restart, concurrency, locks and durability, and W01 stays NO-GO |
| `2026-09-22 FUSE stopped-read terminal reply packet` | On stop, let each prepared positional-read worker produce a terminal `EIO` reply for its original request unique before session/device cleanup; only abort workers after the bounded drain deadline, and assert the wire error in the Linux-gated Unix-stream regression | Host FUSE all-target tests (14 unit, 6 INIT, 0 native, 6 notify/record, 11 protocol, 20 session, 4 sync-barrier), host strict Clippy, Linux-target check/strict Clippy, formatting and diff checks pass; manual hosted run `35662415701` / native-FUSE job `106540484337` passed ordinary round-trip and backend-panic close but blocked-read unmount and kernel read both timed out, so exact-tip hosted rerun remains required and W01 stays NO-GO |
| `2026-09-22 FUSE concurrent forced-detach packet` | Manual non-canceling CI `35666436803` / native-FUSE job `106553144636` passed round-trip and backend-panic callback/close but blocked-read unmount exceeded the 15s observation bound; launch lazy detach concurrently with the 250ms stop grace so closing the descriptor can release the helper without adding a serialized timeout phase | Host FUSE tests, formatting/diff checks and Linux-target strict Clippy pass; exact updated hosted blocked-read result plus callback/lifecycle, crash/restart, concurrency, locks and durability evidence remain required, and W01 stays NO-GO |
| `2026-09-22 FUSE native mount-object packet` (published as `4fd3e25e`) | Restore root N-API `Mounted[Symbol.asyncDispose]()` and record the supported-scope decision for transport-specific FUSE `session`, device `fd`, and invalidation members | Runtime/type coverage and the source audit are local PASS; final remote verification is `HEAD=origin/main=4fd3e25e`; exact-SHA CI run `35646646162` is pending and Fault injection run `35646646113` is in progress, so hosted Linux mount/callback/lifecycle evidence remains open |
| `a3795f0` (published as `a4fa70a`) | Public napi-rs FUSE `OPEN`/`OPENDIR` request codecs | Protocol 7.8/7.39/7.41 pinned differential, typed replies, malformed/truncated/trailing checks and full N-API/typecheck/Clippy gates passed; native FUSE session/device/mount remains open |
| `cc73ad5` (published as `0d8f3c3`) | Unstorage path, metadata and handle parity | 11 oracle rows passed with zero mismatches/skips; capability/edge/N-API/upstream gates passed; hardlinks, symlinks, statfs and mknod remain explicit limitations |
| `a31880d` (published as `1e45692`) | Seeded Rust SDK, Node SDK and CLI provider lifecycle matrix | Positional write, truncate, flush and reopen passed across 5 Rust SDK, 4 Node SDK and 9 CLI rows; PGlite/R2 remain explicit skips |
| `d6c8a29` (published as `89e4940`) | TiDB + RustFS durable fixture hardening | Real TiDB v8.5.7 and loopback RustFS composition passed seed, partial writes, truncate, reopen, CAS/fencing and exact block/metadata cleanup; replicated durable topology remains capacity-gated |
| `7da18fb` | FoundationDB + RustFS exact cleanup scope | Real FoundationDB/RustFS harness passed multi-chunk, partial update, truncate/extend, reopen, CAS, stale fencing and sibling/parent sentinel preservation; service restart/root/hosted composition remain open |
| `3b59dc8` | Public napi-rs FUSE `GETATTR`/`SETATTR` codecs | Protocol 7.41/7.8 pinned differential, typed replies, malformed/truncated/trailing checks, generated declarations and full N-API suite passed; native FUSE session/device/mount remains open |
| `8eac5cb` (published as `67f8b08`) | Public napi-rs FUSE `READ` request/raw-reply codecs | Protocol 7.41/7.8 pinned differential, malformed/truncated checks, declarations, distribution and full N-API suite passed; native FUSE session/device/mount remains open |
| `26427f8` (published as `2c141d5`) | Core handle and lifecycle parity packet | 72-step pinned-oracle trace passed with 63 successes, 9 expected errors, zero mismatches; cross-provider, transport and native lifecycle remain open |
| `750484d` (published as `e510bfb`) | Remaining Unstorage edge classifications | 11 edge rows passed with 11 explicit ENOSYS classifications and zero skips; hardlinks, symlinks, statfs and special nodes remain intentionally unsupported |
| `e64d161` | Public napi-rs FUSE `WRITE` request/reply body codecs | Rebuilt N-API package, malformed-input and pinned mountx differential tests passed; full oracle-enabled N-API suite and distribution checks passed. Existing unrelated strict-Clippy `js_driver.rs` type-complexity lint remains. |
| `8d52d3c` (published as `6ec7b4d`) | Unstorage special-node capability classification | FIFO, socket, character-device and block-device operations are explicitly classified as unsupported; 13-row capability parity passed with 12 oracle-classified unsupported rows and zero skips. |
| `6b9258e` (published as `5f1ec1a`) | Locked Cargo resolution for all N-API CI builds | YAML, shell, local locked release-build checks passed; hosted CI run `35528165152` was created but remains queued and is not acceptance evidence until it completes. |
| `dcc3aa4` | Rust CLI live PGlite SDK consumer reopen check | Real socket-backed Rust CLI write/shutdown/reopen/readback passed; R2 remains credential-gated |
| `10afea2` (published as `7ea3eb6`) | Fail-closed R2/S3 configuration validation | 8 provider tests plus signed HTTP reopen/CAS passed; no live Cloudflare credentials |
| `6855b2d` (published as `fad208e`) | Mountx-compatible 40-hop symlink resolution limit | 17 core behavior tests, 11 core unit tests and 100-step oracle trace passed |
| `2452463` | Server-test phase/cleanup diagnostics | Local repeats; timeout cause unresolved |
| `2085b19` | HTTP/cache requirements and SQLite VFS reference | Requirements, not server implementation |
| `cfce82a` | PGlite injected cleanup failure | Local regression passed |
| `8d1f4ad` | JS driver shutdown test ordering | 25 strict local repeats |
| `e218d18` | S3 incremental codec helpers | 27 tests and Clippy locally |
| `f5759cc` | Actual RustFS harness and CI wiring | Local baseline passed; hosted pending |
| `642e81a` | Compression review | Design, not codec implementation |
| `c4058f2` | CLI macOS NFS native lifecycle | Host-backed native lane passed |
| `29ffb3b` | PGlite cleanup/slot ordering | Local regressions; hosted rerun pending |
| `5993984` | FSKit SDK compile target | Unsigned compilation, not activation |
| `95aca9c` | FSKit Rust/Swift/XPC bridge checkpoint | Local tests and unsigned arm64 Xcode builds; signing/activation/mount pending |
| `ca57758` | TiDB metadata/block providers and pinned harness | Real single-node v8.5.7 ARM64 qualification passed; durable topology and RustFS composition remain open |
| `5d9e513` | SQLite VFS/WAL reliability checkpoint | 4 unit, 15 SQLite-engine and 15 storage-bridge tests plus strict Clippy; Windows/remote/Node acceptance remains open |
| `67498a2` | TiDB schema-test lint follow-up | Focused lint correction; no new service qualification |
| `d526c18` | CLI HTTP edge cases and Windows pinned-oracle CI | Local CLI/HTTP tests and Clippy passed; hosted Windows/oracle execution remains pending |
| `afc55cb` / `c9088aa` / `8cf0c5d` | TanStack Start site and Vercel output hardening | Production deployment and custom-domain DNS/HTTPS verified; broader project release readiness remains open |
| `a5d1dd2` | Windows HostFs and FUSE protocol parity | Focused macOS tests/Clippy; hosted Windows qualification pending |
| `7508a56` | Scoped Cloudflare R2 CLI gate and credential redaction | Runner added; object-count compatibility was fixed in `00e96ce` |
| `00e96ce` | Cloudflare R2 CLI object-count compatibility | Live bucket-scoped CLI, N-API factory, parity and smoke benchmark passed; cleanup readback passed |
| `ae7c4cb` | Isolate inherited PGlite URL from the preflight trace lane | Focused stale-URL regression passed; pushed to `origin/main` |
| `73c33e0` | Isolate PGlite lifecycle from all preflight suites | Full macOS acceptance with live PGlite/R2 exited 0; native/hosted gates remain open |
| `0de1832` + `6ba3d62` | W01 core in-memory parity harness | 101-step pinned-oracle trace passed with 79 successes, 22 expected errors and zero mismatches/skips; later/provider/concurrency behavior remains open |
| `7238c829` | W01 remaining Unstorage skip inventory | 14 rows classified: 4 PASS, 10 ENOSYS, 0 ENOTSUP, 0 skipped; capability-limited operations remain explicit |
| `5c16164` | Public napi-rs FUSE `CREATE` request/reply codecs | Protocol 7.41/7.39/7.12/7.8 differential, malformed/truncated/trailing checks, typecheck, Clippy and full N-API suite passed; native session/device/mount remains open |
| `ae2f12d` | W01 skip inventory and deterministic trace evidence | 88 skips classified; five memory seeds passed; full matrix remains open |
| `dd65770` | Rust-backed N-API FUSE codec subpath | Rebuilt-addon smoke, TypeScript declarations and codec test passed; native session remains open |
| `0d7f1f4` | Rust CLI native end-to-end demo | Actual macOS NFS plus Rust/Node mounted-path I/O and cleanup passed |
| `0dca1d1` | Node SDK CLI example and integration test | Argument checks plus opt-in actual macOS NFS SDK self-test passed |
| `b6800f7` | W01 concurrency, provider/consumer and SQLite acceptance packets | Full root gate exit 0; PGlite rows passed; R2/live native and hosted platform gates remain open |
| `29337f7` | Public Rust SDK facade, Rust CLI routing and Node SDK CLI self-test | SDK unit/example, Rust/Node/provider matrix and CLI self-tests passed locally; live remote/native/hosted lanes remain open |
| `655cf19` | SDK-backed Rust and Node CLI consumer packet | Actual Rust binary and Node CLI examples, SQLite reopen checks, provider matrix and FUSE READDIRPLUS differential pass locally; live PGlite/R2/native/hosted lanes remain open |
| `d6b80f4` | Chunked persistence, SQLite VFS failure handling and Windows HostFs acceptance packets | Focused local chunked, SQLite and HostFs gates passed; hosted Windows and remote-provider lanes remain open |
| `3042d09` | Rust-backed FUSE inode state in napi-rs | Rust/Node inode parity, generated package checks and complete local N-API suite passed; native mount remains open |
| `71e826f` | Pinned-oracle W01 parity audit and closure queue | Core/concurrency traces passed with zero mismatches; structural native, NFS handle, Unstorage and durability gaps remain explicit |
| `21803fd` (published as `9868e93`..`b43a4e9`) | Public-SDK provider matrices and Node CLI config/reopen | Rust/Node/PGlite and CLI local gates passed; memory durability guard added; R2 credentials and hosted CI remain pending |
| `fd5eb04` (published as `88f8f9b` / `f7e630e`) | Structural N-API open flags and native lifecycle | macOS NFS mounted write/readback passed; Linux, other transports, Windows hosted and full FSKit/FUSE acceptance remain open |
| `31d62df` (published as `968119a` / `3c98aa6` / `6b4e8b4`) | Linux structural-driver FUSE CI lifecycle | Prerequisite-gated CI wiring and local harness pass; hosted Linux result pending |
| `3355cc1` (published as `eaba478` / `6a4ec2a` / `3aac826`) | Unstorage timestamp metadata parity | 11/11 Rust tests plus direct and N-API oracle parity passed |
| `323231f` (published as `75d2cae` / `6db5dd9` / `890389f` / `34c6cb0`) | Public FUSE READDIR body codec | Pinned-oracle differential, typecheck and distribution/export checks passed |
| `pending` | W01-FUSE blocked-read teardown follow-up | Non-canceling CI run `35668748366` at `dda0e9c` / native-FUSE job `106560234631` passed round-trip and backend-panic callback/close but still timed out `native_unmount_interrupts_a_blocked_read` after 30.03s. The new bounded graceful-stop path closes the serving task after 250ms while `fusermount3 -u` remains pending; focused FUSE tests and strict checks are required before publishing a fresh hosted rerun. W01 remains NO-GO. |
| `467da6a` | W01-FUSE Lima Linux qualification | Exact published commit passed the Lima Ubuntu arm64 `/dev/fuse` native Rust, automatic facade, CLI lifecycle, Node N-API/structural/SDK native FUSE, SQLite restart, and Python fault-injection gates; backend persistence/reopen was 3/4 with PGlite skipped because `PGLITE_DATABASE_URL` was unset. Local Linux runtime is green; hosted exact-tip acceptance and credentialed PGlite evidence remain open. |
| `467da6a` | W01-FUSE Lima in-process PGlite qualification | Repository `scripts/test-pglite.sh` native-FUSE scope passed mounted PGlite connection reopen and PGlite-backed SQLite compositions using the in-process test server; no external credentials were required. Hosted exact-tip native-FUSE acceptance remains open. |
| `35715585800` / `106706185799` | W01-FUSE hosted native-FUSE queue blocker | Manual CI run at `e175f80a3134e9ab8c02a19944ca036be09f111f` remained queued with no runner, completion time, conclusion, or step output; `origin/main` has since advanced to `e984c2321317e9d93b8db1ec98dc428662b17d34`. No hosted `/dev/fuse` result is claimable; exact-current-tip terminal native-FUSE evidence remains required and W01 stays NO-GO. |
| `35716364566` / `35716566393` | W01-FUSE hosted CI cancellation/queue refresh | Published-tip run `35716364566` at `4e9260ead4925d3140a758374da886f985242f47` was cancelled before any job was created by a newer mainline push; the next run `35716566393` at `924009af061d119404b2ee1f59e86506f7b1cbd2` is pending. No hosted `/dev/fuse` result is claimable; exact-current-tip terminal native-FUSE evidence remains required and W01 stays NO-GO. |
| `32b8596` | W01-FUSE Lima real CLI smoke | On Ubuntu 26.04 arm64, the published CLI mounted the memory driver through native FUSE; strict fixed-payload readback before/after rename, alpha absence/beta presence, SIGINT shutdown, and mount cleanup all passed (`CLI_FUSE_STRICT_SMOKE=PASS`). This closes local shipped-CLI usability; hosted terminal native-FUSE evidence and wider W01 gates remain open. |
