# Public API parity ledger

This ledger describes the current working tree, not a release claim. It was
recovered from `git show HEAD:docs/public-api-parity.md` after the interrupted
edit and refreshed against the current source, tests, and the pinned oracle:

- repository baseline when the deleted file was recovered:
  `ac2161d27f4a6805b87580fbee20c1302e9cd9df`
- current local integration baseline observed during this packet:
  `5b7f982`
- oracle checkout: `/tmp/mountx-source.uWiHfX`
- oracle revision: `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`

The worktree is shared and dirty. This update is restricted to this ledger and
the deterministic parity checker. A component test is evidence for that
component only; it does not close the whole transport, mount, native-host, or
live-service parity item.

## Status vocabulary

- **IMPLEMENTED (focused)** — the named surface exists and has focused source
  or test evidence, but this is not a claim that every oracle behavior is
  covered.
- **PARTIAL** — some of the surface exists, with a concrete remaining gap.
- **MISSING** — the named public surface is absent at the relevant boundary.
- **UNVERIFIED** — implementation may exist, but the required native, live, or
  oracle-backed check was not run or was intentionally skipped.

## Package and barrel snapshot

The N-API package currently exports the root, `auto`, `nfs`, `9p`, `fuse`, `s3`,
`webdav`, and the three driver helpers. The exact map is in
[`package.json`](../integrations/mount-rs-napi/package.json#L11-L60). The
`./fuse` entry is now a Rust-backed codec/inode barrel; it is deliberately not
described as a complete session or native-mount implementation.

| Boundary | Current implementation | Parity state |
| --- | --- | --- |
| Root filesystem, errors, path utilities, and driver factory | Native `Filesystem`/handle API, root utility aliases, and `createDriver` facade | **IMPLEMENTED (focused); PARTIAL** for oracle harness/type parity |
| Structural `FsDriver` accepted by mount/server APIs | `createDriver` adapts a JS object and the JS server facades accept structural drivers; the opt-in native mount lifecycle now accepts the plain structural object and passes macOS NFS read/write/unmount | **PARTIAL; UNVERIFIED** for hosted Linux/other platforms |
| `auto` and `mount` lifecycle | Native mount and typed auto options exist; option and lifecycle surface is narrower than oracle | **PARTIAL; UNVERIFIED** for native mount |
| NFS and 9P Node subpaths | NFS XDR/RPC and 9P codec facades plus NFS/P9 servers | **PARTIAL** |
| FUSE Node subpath and full protocol barrel | Rust notify/record/protocol pieces, typed `READ`/`WRITE`/`GETATTR`/`SETATTR`/`OPEN`/`OPENDIR`/`LOOKUP`/`READLINK`/`STATFS`/`BATCH_FORGET`/`INTERRUPT`/`RELEASE`/`RELEASEDIR`/`FLUSH`/`FSYNC`/`FSYNCDIR`/`SYMLINK`/`MKNOD`/`MKDIR`/`UNLINK`/`RMDIR`/`RENAME`/`RENAME2`/`LINK`/`ACCESS`/`FALLOCATE`/`LSEEK`/`GETLK`/`SETLK`/`SETLKW` bodies, directory codecs, and a Rust-backed `InodeTable` are exported through `./fuse`; Rust session `ACCESS`, validated `BATCH_FORGET`, and fail-closed `INTERRUPT` dispatch plus pure Rust INIT negotiation are tested separately; remaining body, session, and mount surfaces remain open | **IMPLEMENTED (focused); PARTIAL** |
| NFS/P9/S3/WebDAV server objects | Native servers and postbuild lifecycle facade exist; S3's Rust gateway and N-API object expose bounded drain, live connection and peer-aware transport-hook state, plus shared buffered/streaming session methods, safe effective options, bucket wrappers, and debug-gated assertions; WebDAV likewise exposes buffered and streamed direct session requests with positional response bodies, supported server/session callable members, and class 1/2/3 direct-method coverage, while several oracle members remain outside scope | **PARTIAL** |
| S3 Node API and structural bucket sources | Native Rust low-level API and native `Filesystem` bucket map exist; the supported Node `./s3` boundary is the N-API server/session facade, while the oracle's pure codec/helper barrel is explicitly Rust-owned | **PARTIAL; SCOPED** |
| WebDAV low-level public API | Rust server/session/protocol and the `./webdav` constants/status/protocol/XML/lock barrel are public; the N-API session exposes buffered and streamed request/response bindings, read-only active lock records, and direct LOCK/UNLOCK, cancellation, and body-error coverage; the full supported server/session string/symbol prototype-member and class 1/2/3 direct-method differentials pass, with native `handleRequestStream`/`lockCount` additions explicitly scoped; the oracle-only `now`, `onAssertion`, and live lock-table controls are explicitly outside the supported N-API scope | **PARTIAL** |
| CLI | Rust CLI constructs drivers through `mount-rs-sdk` and exposes a real `sdk-self-test`/durable-reopen path; Node CLI uses the N-API SDK for direct and versioned provider-config/reopen self-tests; native mount/live behavior is not established by ordinary tests | **PARTIAL; UNVERIFIED** |

The oracle source used for comparison is local and immutable for this audit;
the relevant upstream package map, types, harness, auto API, and transport
barrels are under `/tmp/mountx-source.uWiHfX/src`.

## Primary parity ledger

### P0 — structural driver and harness parity: PARTIAL

The current N-API adapter accepts a structural JS driver with required
`stat`, `readdir`, and `open` methods, maps optional filesystem methods, and
returns `ENOSYS` for an omitted optional method. See
[`js_driver.rs`](../integrations/mount-rs-napi/src/js_driver.rs#L1367-L1664) and
the [`create_driver` factory](../integrations/mount-rs-napi/src/js_driver.rs#L2103-L2145).

That adapter is not yet the oracle contract boundary:

- `mount`, `createNfsServer`, `createP9Server`, `createS3Server`, and
  `createWebdavServer` now accept native or structural drivers through the
  JavaScript facade, with owned-adapter cleanup. Real server tests pass; the
  opt-in macOS NFS structural mount passes read/write/unmount, and Linux FUSE
  prerequisites plus the equivalent CI lifecycle are wired. Hosted Linux,
  other transports and Windows execution remain unverified.
- The root facade now exports `FsDriver`, `Loopback`, `ResolvedCapabilities`,
  `createLoopback`, and `resolveCapabilities`. Main passed the pinned-oracle
  harness comparison for declarations/inference, method binding and identity,
  path arguments, optional-method errors, partial I/O, and close-error precedence.
  JavaScript wrapper identity is preserved; native path/error helpers are reused.
- `js-driver.mjs` now includes `structural-factories.mjs`: plain drivers pass
  through all server factories, with capability/missing-method comparisons,
  cleanup and eight WebDAV DELETE oracle cases. Main reran these successfully.
  A rebuilt addon also passes the opt-in macOS NFS structural mount lifecycle,
  including mounted write/readback and callback reachability; the Linux FUSE
  job is prerequisite-gated and still needs a hosted pass.
  The parent suite still skips when `MOUNTX_SOURCE` is unset.

**Required closure evidence:** a non-skipped oracle-backed test must pass the
same structural driver through mount and each server factory, compare
capabilities and missing-method errors, and verify cancellation/shutdown.

### P0 — Node transport subpaths: PARTIAL

The N-API package has focused codec aliases for NFS and 9P. The NFS test covers
XDR/RPC primitives, errors, and record assembly; the 9P test covers the CJS to
ESM alias surface and wire primitives. These are codec boundaries, not full
transport/session parity:

- [`nfs.cjs`](../integrations/mount-rs-napi/nfs.cjs) and
  [`nfs-codec.mjs`](../integrations/mount-rs-napi/test/nfs-codec.mjs#L1-L20)
  do not establish NFS v3/v4 mount/session/server parity.
- [`p9.cjs`](../integrations/mount-rs-napi/p9.cjs) and
  [`9p-codec.mjs`](../integrations/mount-rs-napi/test/9p-codec.mjs#L132-L245)
  cover the CJS-to-ESM codec aliases, wire primitives, and typed message
  bodies; [`p9-constants.mjs`](../integrations/mount-rs-napi/test/p9-constants.mjs)
  additionally differentially checks all 124 upstream 9P constants and
  `messageName`. These are still codec/barrel boundaries, not the complete
  upstream 9P server object or all protocol behavior.
- The package now has a `./fuse` export. Its codec barrel covers the bound
  notify/record/protocol helpers, its `InodeTable` facade delegates to the Rust
  transport table, and its Rust-backed `FuseSession` exposes the mount-free
  request/lifecycle boundary. Native kernel mount objects remain outside this
  subpath; root `mount` owns that platform-specific surface. See
  [`fuse.cjs`](../integrations/mount-rs-napi/fuse.cjs) and
  [`fuse_session.rs`](../integrations/mount-rs-napi/src/fuse_session.rs).

**Required closure evidence:** keep the broad request/reply and mount-free
session exports aligned with the serialized dispatch policy, then qualify the
platform-specific native mount and lifecycle surfaces. Oracle-backed subpath
tests are necessary but do not substitute for native acceptance.

### P1 — FUSE public layer: IMPLEMENTED (focused); PARTIAL

The Rust implementation has useful, tested FUSE pieces:

- notification constants, invalidation encoding/decoding, and validation are
  public through [`notify.rs`](../transports/mount-rs-fuse/src/notify.rs#L1-L174);
- transcript record/replay types and codecs are public through
  [`record.rs`](../transports/mount-rs-fuse/src/record.rs#L1-L245);
- [`notify_record.rs`](../transports/mount-rs-fuse/tests/notify_record.rs#L30-L257)
  contains upstream fixtures, malformed-input checks, and replay reports.
- the N-API `./fuse` barrel now also exposes oracle-shaped `packDirents`,
  `unpackDirents`, `packDirentsPlus` and `unpackDirentsPlus` for bounded
  `READDIR`/`READDIRPLUS` bodies. UTF-8 names, 8-byte alignment, bounded
  packing, protocol layouts, integer coercion and malformed input are
  differentially tested against the pinned oracle in
  [`fuse-codec.mjs`](../integrations/mount-rs-napi/test/fuse-codec.mjs);
- the same barrel now exposes typed Rust-backed `READ`/`WRITE`,
  `GETATTR`/`SETATTR`, `OPEN`/`OPENDIR`, `LOOKUP`, `READLINK`, and `STATFS`
  request/reply codecs. Protocol 7.8/7.39/7.41 differential checks cover
  truncation, trailing bytes, malformed inputs, typed replies, legacy
  compatibility layouts, and embedded-NUL rejection. These are still
  mount-free codec gates; they do not establish FUSE session or native mount
  parity;
- the barrel also exposes typed `BATCH_FORGET` and `INTERRUPT` request codecs.
  Pinned-oracle tests cover protocol-minor framing, count/payload validation,
  truncation, trailing bytes and the required empty-reply classification;
  these remain mount-free codec gates;
- the Rust FUSE session dispatch now validates and handles `ACCESS` requests
  with the fixed 8-byte wire shape, root handling, owner/group/other mode
  checks and invalid-mask errors. Focused locked session tests pass, but this
  does not establish a kernel FUSE device or native mount lifecycle;
- the Rust session now validates `BATCH_FORGET` count/payload lengths before
  mutating inode state and accepts `INTERRUPT` only with its exact 8-byte body,
  returning `EAGAIN` for unknown targets without guessing cancellation or
  tearing down the session. Native kernel cancellation remains unverified;
- the N-API `./fuse` barrel now exposes Rust-backed `RELEASE`/`RELEASEDIR`,
  `FLUSH`, and `FSYNC`/`FSYNCDIR` request/status codecs. Pinned protocol
  differentials cover empty status replies and malformed/truncated/trailing
  inputs; these remain mount-free codec gates;
- pure Rust INIT negotiation now gates `FUSE_INIT_EXT`/`flags2` by the
  negotiated minor and extension bit, rejects unmarked extension words, and
  has six wire-layout integration tests for downgrade/retry, clamping,
  compatibility layouts and truncation. This is not native session/mount
  acceptance;
- the Rust-backed `InodeTable` is now exposed from the N-API `./fuse` barrel;
  its facade preserves oracle-shaped `Inode` views and `Set` paths while the
  state and mutation logic remain in Rust. The focused oracle test covers
  driver-identity hardlinks, held orphaned nodes, `FORGET`, directory subtree
  remapping, replacement targets, and `useDriverIno: false`; see
  [`fuse_inodes.rs`](../integrations/mount-rs-napi/src/fuse_inodes.rs) and
  [`fuse-inodes.mjs`](../integrations/mount-rs-napi/test/fuse-inodes.mjs).

The oracle FUSE barrel also exports constants, init, inodes, mount, notify,
protocol, record, and session, including a broad request/reply body codec.
- The N-API FUSE codec now adds raw-layout `IOCTL` request/reply support: the 32-byte request header, declared input payload, 16-byte reply, signed result and protocol-context fields are differentially tested against the pinned oracle, including malformed/trailing inputs. The post-publication full N-API suite passed; this remains focused codec evidence rather than full session/native-mount parity.
- The N-API FUSE codec now also exposes typed `BMAP` request/reply bodies. Valid protocol-minor layouts, every truncation boundary, trailing bytes and wrong-shape cases are pinned-oracle differentials; the post-publication full N-API suite passed. Full FUSE session/native-mount parity remains open.
- The N-API FUSE codec now also exposes typed `GETLK`, `SETLK` and `SETLKW` request bodies plus the typed `GETLK` reply. The pinned-oracle differential covers the 48-byte request, 24-byte reply, truncation, trailing-byte and empty status-reply boundaries; generated bindings, declarations, typecheck, release build and the full N-API suite passed. This remains mount-free wire evidence; native FUSE lock/session parity remains open.
- The latest N-API FUSE codec packet exposes typed `SYMLINK`, `MKNOD`, `MKDIR`, `UNLINK`, `RMDIR`, `RENAME`, `RENAME2`, `LINK`, `ACCESS`, `FALLOCATE`, and `LSEEK` request/reply bodies with generated declarations and explicit CommonJS/ESM exports. Pinned mountx byte/decode differentials and the full artifact-aggregation suite passed; this remains mount-free codec evidence, and native session/device/mount parity remains open.
- The Rust FUSE session packet now covers the already-supported simple namespace operations at the frame boundary, including successful mutation, error-state preservation, MKNOD fallback, ACCESS credential behavior and the 255-byte POSIX name limit. The focused 16-test session suite and strict Clippy passed; native device/mount acceptance and advanced operation semantics remain open.

The N-API package now exposes the broad request/reply body codec, INIT
negotiation readback, and a Rust-backed mount-free session. It deliberately
does not expose a portable native mount object from `./fuse`; the root auto
facade owns native mounting and its platform boundary. Therefore the local
codec/session gates still do not prove full kernel FUSE or FSKit parity.

The previously known `js_driver.rs` type-complexity lint was resolved in
`dbfa2ea`; the current scoped N-API Clippy gate passes with `-D warnings`
without that exclusion. This removes a lint blocker only and does not change
the native-session, hosted-platform, or native-mount acceptance boundary.

### P1 — auto/mount option and lifecycle parity: PARTIAL; UNVERIFIED

The current root facade exposes `mount`, `liveMounts`, `unmountAll`, and
`probeTransports` through [`postbuild.mjs`](../integrations/mount-rs-napi/postbuild.mjs#L6-L22).
The native `Mounted` object exposes transport, mountpoint, source, active, and
`unmount`; the bounded 9P view additionally exposes `trans`, the adopted
server/connection, `waitClosed()`, and `closed`; see
[`index.d.ts`](../integrations/mount-rs-napi/index.d.ts#L175-L205).

The public auto options are currently `transport`, `readOnly`,
`unmountTimeout`, `onTransportError`, `nfsSqliteSingleHost`, and a bounded
`p9` bag. [`JsAutoMountOptions`](../integrations/mount-rs-napi/index.d.ts#L1148-L1168)
defines that shared shape. The direct `./9p` facade additionally exposes
`p9ClientProbe`, `p9Platform`, `socketPathRefusal`, `tcpSourceRefusal`,
`p9MountOptions`, `mount9p`, `live9pMounts`, and `unmountAll9p`. The supported
9P bag covers transport, host/port/path, scalar server policy (including
remote admission, socket mode/shared-directory policy, frame/in-flight bounds,
negotiated `msize`, inode/read-only/ownership/debug policy, lock-table
injection, and direct session `onError`/`onAssertion` callbacks), access/cache/
uname/aname, mount msize, mount options, `signals`, unmount timeout,
transport-error callback, and configured shared-server injection. The direct session callbacks
apply when the mount creates its own listener; an injected shared server retains
its configured hooks. The pinned oracle comparison found no additional
unrepresented direct `MountP9Options` fields, and `P9Mount.source` is declared
and runtime-asserted as a non-empty `string`. Automatic cross-transport signal
ownership remains explicitly outside the root automatic-mount scope. The
callback is retained by the `Mounted` lifecycle and is wired to the selected
native FUSE, 9P, or NFS transport hook; a hosted native fault event is still
required before this callback boundary can be treated as runtime-qualified.
The hosted N-API native-mount run now qualifies the supported lifecycle. The Rust auto layer has typed transport
selection and timeout handling, but this does not close the upstream option or
lifecycle surface.

No native mount, unmount, signal, or live-filesystem result should be inferred
from component tests. The CLI and integration test prerequisites remain an
explicit evidence boundary.

### P1 — embedded server object contracts: PARTIAL

The postbuild server facade supplies cached `listen`/`close`, makes `listen`
return the server, and adds `Symbol.asyncDispose`; it also supplies the P9
`connection.closed` promise. See
[`postlude-servers.cjs`](../integrations/mount-rs-napi/postlude-servers.cjs#L1-L135)
and its lifecycle assertions in [`servers.mjs`](../integrations/mount-rs-napi/test/servers.mjs#L166-L183).

Current focused behavior:

- NFS and P9 expose transport-error callbacks in the native options and have
  loopback TCP tests for malformed records/frames and orderly EOF handling;
  see [`servers.rs`](../integrations/mount-rs-napi/src/servers.rs#L297-L452)
  and [`servers.mjs`](../integrations/mount-rs-napi/test/servers.mjs#L198-L445).
- P9 exposes native clients, connection session access, and the closed promise.
  The Node facade now implements `server.attach(stream, options)` with actual
  stream-backed connections, direct `session.handleCall`/`destroy`, ownership,
  duplicate-attach rejection, bounded frame dispatch, and close tracking. A
  connection accepted by the native Tokio listener intentionally reports
  `stream: undefined`: the listener owns a Tokio stream rather than a Node
  `Duplex`; the supported Node stream-injection boundary is `attach`.
  The public `peer` type uses `null` only when the native binding has no peer;
  native Unix/TCP listeners otherwise expose their socket path or
  `address:port`, while the attached-stream contract uses `undefined` when no
  peer fallback is supplied.
- The N-API object boundary keeps serializable lifecycle views: native
  `P9Server.address()`/`path` use string-or-null representations, and effective
  `onError`/`onAssertion` hooks are omitted from `server.options` and
  `session.options` rather than pretending that live JavaScript functions can
  be round-tripped through the native getter. The focused metadata regression
  checks those snapshots plus the attached stream/peer state; the hosted direct
  mount test checks the native stream/peer representation.
- The optional session/lock return shapes follow the oracle at the JavaScript
  boundary: `P9Session.msize`, `P9Session.version`, and `P9Session.userFor()`
  return `undefined` before negotiation or for an unknown fid, while
  conflict-free `P9LockTable.getlock()` and `P9LockClient.getlock()` return
  `undefined`; the native binding's internal `null` values are normalized by
  the postlude. Exact SHA
  `0d520a1d0a9af44e08e65c5f0638a640bb3c08db` passed [Native 9P run
  `35671509538`](https://github.com/andymac4182/mount-rs/actions/runs/35671509538),
  with N-API job `106569412372` passing automatic/direct/structural mounted
  I/O and cleanup plus direct native session/lock assertions, and Rust job
  `106569412047` passing all four ignored native tests.
- The direct `./9p` probe helpers also retain the oracle's platform argument
  boundary: `p9ClientProbe(platform?)` returns deterministic override facts
  without attempting a mount, and `p9Platform(platform?)` maps the requested
  platform to `"linux" | undefined`; the zero-argument probe remains backed by
  the native host probe.
- The N-API P9 session now exposes the scalar session policy through
  `session.options` and the attach identity through `userFor(fid)`; the server
  exposes its effective scalar policy through `server.options`. These members
  are covered by generated typecheck and a live attach-only runtime check. The
  live session lock client and the standalone `P9LockTable`/`P9LockClient`
  inspection and mutation surface are now backed by the transport's shared
  lock state, including conflict, rename, release, and ownership evidence.
  The `./9p` barrel now exposes the complete upstream constants/default surface,
  including message names, masks, qid bits, wire sizes, version values, Linux
  open flags where applicable, and the six public default values; the pinned
  runtime checks pass all 124 upstream constants and all 274 upstream runtime
  barrel exports. It also exposes the Rust-backed `FidTable` alias,
  live `P9Session.fids`, mutable path/open/iounit/cursor views, qid/cursor
  helpers, detached clunk snapshots, and retained open-handle enumeration;
  focused tests cover the oracle lifecycle, hardlink/release identity,
  large-inode input, and a real opened session fid. Direct table mutation is a
  low-level inspection/testing seam; protocol clunk or session destroy remains
  the orderly production teardown path. The N-API session also exposes its live
  `driver`, debug-gated assertion readback/statistics, and request-error and
  assertion callbacks with Node error/header shapes across attached and native
  sessions. `P9Server.clients` is now a live property-shaped array combining
  native and attached connections. `P9ServerOptions.locks` accepts a
  `P9LockTable`, and the injected table is shared by native and attached
  sessions and exposed through their live option handles. The bounded `./9p`
  mount-helper facade now exposes Linux-client probing, refusal and exact
  option-string helpers, strict named `mount9p` delegation, 9P live-mount
  filtering/cleanup, and mounted transport/server/connection/closed views. It
  also accepts a configured `P9Server` and adopts its already-bound transport,
  policy, lock table, callbacks, and client set rather than creating a second
  listener. Mount-created listeners now receive the scalar server-policy
  fields, lock table, and direct session `onError`/`onAssertion` callbacks from
  the same option bag. This is not full oracle mount parity: automatic
  cross-transport signal ownership remains explicitly outside the root
  automatic-mount scope. The direct `MountP9Options` fields are complete
  against the pinned oracle, and `P9Mount.source` is narrowed to `string` and
  runtime-asserted by the hosted direct mount. Exact SHA
  `0ad4928e86af89163c8c87d08fea53ccf7f5f89b` passed the hosted N-API Linux
  automatic, direct `./9p`, and structural-driver 9P mounted-I/O/cleanup gate
  in [run `35668145703`](https://github.com/andymac4182/mount-rs/actions/runs/35668145703),
  job `106558367429`; the Rust job `106558367006` passed all four ignored
  native tests. Crash/reset/half-close recovery remains supervisor-owned.
- NFS now exposes a shared `session` view with v3/v4-aware direct `handleCall`
  routing, direct v3/v4/unified `destroy()` operations, read-only v3 and v4
  session views, server-owned driver wrappers, effective scalar options,
  write-verifier readback, synchronized v3/v4 request/reply/error/drop/procedure
  stats, mount records, destroyed-state readback, the server's active
  `connections` count, and live `clients()` objects with stable id/peer/session
  views plus `close()`/`waitClosed()` lifecycle. All three N-API session views
  expose deterministic BigInt-backed snapshots of the Rust server's shared
  v3/v4 handle table. The low-level mutable v4 state table and callback
  function values are intentionally outside the N-API inspection surface;
  wire operations, effective knobs, lease sweeping, and live callback tests
  qualify the supported behavior. Hosted `native-nfs` jobs on macOS and Ubuntu
  now pass the privileged native NFSv3/NFSv4.1 and SQLite-over-NFS checks;
  the host-backed NFSv3 process-crash/restart test rejects the old file handle
  with `NFS3ERR_STALE` and then recovers a `FILE_SYNC` payload through a
  replacement server. A forced-crash NFSv4.1 child-process test also rejects
  the old session with `NFS4ERR_BADSESSION` before dispatch and the old root
  handle with `NFS4ERR_STALE`; session IDs fold both halves of the write
  verifier to avoid the observed rapid-replacement alias. Rootless NFSv4.1
  wire coverage also drives two
  independent sessions through concurrent distinct-file OPEN/WRITE/READ
  round trips. NFSv4 lease/replay/file-handle recovery, cross-process/native
  concurrency, power-loss durability, and whole-workflow release acceptance
  remain open. Multiple server processes sharing one backend are outside the
  supported scope because session/lease/replay/handle arbitration is
  process-local. The exact published concurrency tip's CI run
  `35663954461` was cancelled before jobs were created, so it adds no newer
  hosted native result. S3 now
  exposes `S3Server.session`, bucket names, session-owned bucket wrappers,
  safe effective options, debug-gated assertions, buffered `handleRequest`,
  streaming `handleRequestStream`, async session metrics, live `connections`,
  and typed `onTransportError` delivery. WebDAV exposes `connections`, a
  session view, buffered and streamed direct requests, and positional response
bodies, and the full supported server/session string/symbol prototype-member
differential now passes; only the three oracle-only controls remain outside
scope. Compare the current
  native options and objects in
  [`servers.rs`](../integrations/mount-rs-napi/src/servers.rs#L816-L1192) with
  the declarations in [`index.d.ts`](../integrations/mount-rs-napi/index.d.ts#L1085-L1203).
- S3 and WebDAV lifecycle wrappers are covered only to the extent exercised by
  their focused tests; this is not a claim of all upstream connection/session
  behavior.

### P1 — S3 structural source and bucket-map parity: PARTIAL

The current S3 factory supports one native `Filesystem` or an object whose
values are native `Filesystem` references. It validates bucket names and has
focused two-bucket PUT/GET isolation coverage; see
[`servers.rs`](../integrations/mount-rs-napi/src/servers.rs#L893-L1057) and
[`servers.mjs`](../integrations/mount-rs-napi/test/servers.mjs#L458-L501).

The JavaScript facade now adapts structural drivers in mixed bucket maps before
the native extractor, with real server isolation coverage. It now also performs
the oracle's construction-time source and bucket-name validation, including
oracle-compatible `TypeError` messages, empty maps, single-bucket options, and
the UTF-16 bucket-name boundary. The pinned structural-factory differential
covers those cases; close/restart, native/hosted lifecycle, and complete S3
session parity remain open.

### P2 — S3 low-level and streaming parity: PARTIAL

Rust exposes public chunked, constants, protocol, server, session, SigV4, and
XML modules through [`mount-rs-s3/src/lib.rs`](../transports/mount-rs-s3/src/lib.rs#L1-L52).
The incremental chunked decoder and streamed HTTP request/response paths are
present in [`chunked.rs`](../transports/mount-rs-s3/src/chunked.rs#L1-L7) and
[`server.rs`](../transports/mount-rs-s3/src/server.rs#L146-L215). Public API,
XML, SigV4, gateway, and chunked tests exist, including
[`public_api.rs`](../transports/mount-rs-s3/tests/public_api.rs#L20-L213).

The Node `./s3` entry is currently a root server/session facade, not an
oracle-equivalent codec barrel. It now exposes buffered and incremental
`S3Session` request/response bodies through the N-API bridge, safe effective
options, bucket wrappers, debug-gated assertions, live connections, and typed
transport-error callbacks; the focused release-binding integration covers
multi-chunk PUT/GET, response cancellation, request-generator failure mapping,
bucket isolation, and async metric deltas.
The transport README records known behavior boundaries including List V1,
bucket create/delete, `partNumber`, and non-`/` delimiters; see
[`README.md`](../transports/mount-rs-s3/README.md#L21-L28). Rust component tests
and a native gateway test do not establish complete oracle parity or a live AWS
service result. The current Rust gateway packet also verifies bounded drain
timeout, accepted-connection cleanup, loopback-only credentialed binding,
assertion cleanliness, and one peer-aware reset-on-close transport event
across the 23 gateway cases. The generated package build and N-API integration
verify connection/transport-error member parity and direct Node peer-fault
injection; live AWS/R2 and native/hosted lifecycle evidence remain open. The
oracle-only pure codec/helper members are explicitly outside the supported Node
scope, and the standalone TypeScript fixture check passes against the
checked-in declarations.

The supported-scope decision is pinned by
[`test/s3-barrel-scope.mjs`](../integrations/mount-rs-napi/test/s3-barrel-scope.mjs):
with `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921`, the package
`./s3` export is compared to the oracle's runtime export set and the exact 153
oracle-only codec/helper names are asserted as Rust-owned and out of the Node
subpath. The supported N-API `S3Server`, `S3Session`, streaming body classes,
and `createS3Server` identity are asserted against the root package. This is
an explicit Node-scope boundary, not a claim that the oracle's pure codec
barrel is available from JavaScript; the Rust `mount-rs-s3` public API and its
codec tests remain the low-level implementation boundary.

### P2 — WebDAV public barrel, session, and streaming: PARTIAL

The Rust WebDAV server, session, protocol, constants, and lock modules are
public through the transport crate, and the N-API `./webdav` entry preserves
root server/class identity while exposing the low-level constants/status,
path/header/XML/lock helpers and generated declarations. The N-API session now
accepts async-iterable or Web ReadableStream request bodies and returns a
pull-based response iterator; the session also exposes read-only active lock
records with expiry cleanup and recursive namespaced owner XML trees. The
direct probe covers the class 1/2/3 method
matrix, LOCK/UNLOCK cleanup, chunked PUT, multi-chunk GET, early iterator
return, deliberate request-body failure mapping, one typed peer-aware callback
from a Node socket reset, one malformed-HTTP callback, and same-driver server
recreation preserving file bytes while resetting session locks. It also
completes 256 parallel unique-file PUT/GET requests through one direct
session with exact body readback; this is in-process same-driver evidence only.
The pinned TypeScript-vs-Rust loopback HTTP differential also passes all 40
paired S3+WebDAV cases, including 16 authenticated WebDAV cases covering
streaming PUT, XML property updates, GET/HEAD/range/conditional behavior,
PROPFIND, COPY/MOVE, refusal, missing-resource, and DELETE responses.
The Rust XML boundary accepts the five predefined and bounded numeric
references used by valid WebDAV owner documents while refusing DTD/custom
entities.
The remaining N-API session boundary is explicit: it exposes scalar effective
options, snapshot `WebdavLockView[]` records, numeric/record-shaped statistics,
and assertion readback, with `stats.methods` normalized to the oracle's
`Map<string, number>` shape and request-level `onError(error, head)` callback.
Three oracle-only controls are deliberately outside the supported N-API
surface, rather than being silently advertised as parity: `options.now` is a
synchronous JavaScript callback in the oracle, while the Rust transport keeps
the deterministic injected clock at its native boundary and does not call
arbitrary JS synchronously from an async worker; `onAssertion` has no
externally triggerable assertion site in the current Rust implementation (the
N-API readback remains empty on healthy sessions); and the live `DavLockTable`
is kept request-owned, with N-API exposing safe expiry-aware snapshots instead
of out-of-band mutators that could bypass WebDAV token/ownership checks. The
Rust clock boundary is covered by
`./scripts/cargo-shared test -p mount-rs-webdav --test webdav --locked injected_session_clock_controls_lock_expiry_deterministically`.
These are accepted supported-scope decisions for N-API; the full supported
session/server string/symbol prototype-member and class 1/2/3 direct-method
differentials now pass, while the oracle-only controls and listener,
provider/native, restart, and hosted gates remain open.
The Rust listener lifecycle itself is locally qualified by concurrent
`listen()` serialization, an immediate `listen()`/`close()` shutdown-wakeup
regression, and a bounded-drain regression: a stalled partial request keeps a
timed-out server in a terminal closing state, aborts the tracked connection
task, and rejects `listen()` until a successful retry close completes. The
focused host-enabled WebDAV target passes 18/18 with strict warning-denied
Clippy and formatting. The N-API WebDAV wrapper also serializes its
closed-state check with the transport lifecycle; a rebuilt 40-iteration
real-loopback race test passes. The opt-in
`MOUNT_RS_SERVER_PHASE=webdav node test/servers.mjs` phase also passes the
host-enabled WebDAV network/fault/restart matrix, and a focused 256-pair live
HTTP network-concurrency/authentication/streaming test; hosted network
concurrency and hosted lifecycle remain open.
The shared postbuild server facade keeps close idempotent while in flight but
clears a rejected close promise so a timed-out N-API WebDAV close can be
retried after the peer drains. The local SQLite N-API WebDAV probe also
survives an abrupt child-process termination and confirms that bytes persist
while replacement-session locks remain process-local; live-provider,
power-loss, and hosted lifecycle acceptance remain open.
The pinned pure barrel/protocol differential passes at oracle
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` when
`MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921` is supplied; the three
oracle-only controls remain outside supported scope, while listener,
provider/native, restart, and hosted gates remain open.

### P2 — CLI parity: PARTIAL; UNVERIFIED

The CLI parser and runtime cover transport selection, driver selection,
read-only/empty/allow-other flags, probe, mountpoint precedence, Ctrl-C
cleanup, and stale-mount cleanup. See
[`parser.rs`](../crates/mount-rs-cli/src/parser.rs#L1-L15) and
[`runtime.rs`](../crates/mount-rs-cli/src/runtime.rs#L289-L405).
Focused tests cover help/version/probe, parser values, transport/driver
selection, configuration validation, and actual Rust-binary SDK self-tests in
[`cli.rs`](../crates/mount-rs-cli/tests/cli.rs#L6-L121).

The CLI README explicitly separates ordinary tests from native FUSE subprocess
tests and says the latter require explicit prerequisites; see
[`README.md`](../crates/mount-rs-cli/README.md#L113-L141). Exact oracle help,
demo hints, native mount/unmount behavior, and live filesystem results remain
unverified. Do not report skipped native tests as passing.

## Next W01 closure queue after SDK-backed CLIs

The Rust and Node CLI SDK paths now have focused, mount-free read/write
evidence. They do not close the next core parity gates, which are ordered here
from the smallest contract boundary to the larger environment boundary:

1. **Structural-driver native mount (P0/P1 seam):** the opt-in macOS NFS
   lifecycle now mounts a structural driver and verifies read/write/unmount;
   Linux FUSE prerequisites and the equivalent CI lifecycle are now wired.
   Obtain a hosted Linux pass, repeat supported transport choices, and retain
   explicit unsupported results on platforms that cannot provide the mount.
2. **Capability-limited Unstorage behavior (P1):** focused oracle-backed
   capability, unsupported-operation, ownership-overlay and timestamp metadata
   checks now pass. The remaining inventory now identifies hardlink support as a
   six-row exact `ENOSYS` boundary in direct/N-API tests (generic inode-sharing
   implementation remains open), symlinks/link timestamps (16 + 2 rows), `statfs` (2 rows), special-node
   creation (16 rows), and root-only permission cases (18 rows). Each remaining
   row needs an explicit unsupported assertion or adapter implementation; a
   passing MemoryFs or ChunkedFs case cannot close an Unstorage row.
3. **Durability and race boundaries (P1):** local restart, provider fencing,
   cancellation/close, fault-injection, SQLite and PGlite gates now cover the
   main storage paths, including the SDK-backed CLI reopen flow. Hosted crash,
   live-R2 and native transport concurrency remain separate acceptance gates.
4. **Remaining public transport/API surface (P1):** the FUSE directory,
   `READ`/`WRITE`, `GETATTR`/`SETATTR`, `OPEN`/`OPENDIR`, `LOOKUP`,
   `READLINK`, `STATFS`, `BATCH_FORGET`, `INTERRUPT`, `POLL`, `FALLOCATE`,
   `RENAME2`, `LSEEK`, `GETLK`/`SETLK`/`SETLKW`, and `COPY_FILE_RANGE` body/session boundaries are now
   public or strictly validated and oracle-differentially tested where an
   oracle body exists. Expose the remaining
   request/reply bodies, session and native-mount surfaces, then run
   oracle-backed subpath tests for every exported transport rather than
   treating codec or inode fixture tests as transport completion.

The first two items are behavior or environment gaps in the current W01 slice;
the third and fourth retain broader acceptance boundaries. Provider,
transport-barrel, FSKit, and live remote-service work remains separate from
this simple-first queue.

## Implemented at the current boundary (not broad completion)

These surfaces have concrete current-tree implementation, while the primary
ledger above records where oracle parity is still incomplete:

- Native N-API filesystem/handle operations and lifecycle behavior in
  [`lib.rs`](../integrations/mount-rs-napi/src/lib.rs#L2307-L2732), exercised by
  [`contract.mjs`](../integrations/mount-rs-napi/test/contract.mjs) and
  [`smoke.mjs`](../integrations/mount-rs-napi/test/smoke.mjs).
- Root path, mode, errno, error, range, and `PathLock` utilities in
  [`utilities.mjs`](../integrations/mount-rs-napi/test/utilities.mjs#L20-L320),
  with the root aliases installed by [`postbuild.mjs`](../integrations/mount-rs-napi/postbuild.mjs#L6-L22).
- Unstorage driver binding and fixed/extensible chunked storage, with focused
  N-API tests; these are implementation surfaces, not proof of every upstream
  capability or storage-backend result.
- JS structural-driver adaptation and callback error/cancellation handling,
  subject to the P0 limitation above.
- Rust 9P and NFS public low-level barrels, including protocol/session/wire and
  NFS v4/XDR surfaces; see [`mount-rs-9p/src/lib.rs`](../transports/mount-rs-9p/src/lib.rs#L18-L41)
  and [`mount-rs-nfs/src/lib.rs`](../transports/mount-rs-nfs/src/lib.rs#L10-L38).
- Rust S3 and WebDAV server/session/protocol pieces and FUSE notify/record
  pieces, subject to their respective partial ledgers above.
- Rust-backed FUSE inode state through the `./fuse` `InodeTable` facade, with
  the pinned-oracle comparison in
  [`test/fuse-inodes.mjs`](../integrations/mount-rs-napi/test/fuse-inodes.mjs).

## Verification and evidence boundary

The relevant tests are intentionally separated by what they prove:

- `integrations/mount-rs-napi/test/utilities.mjs`, `contract.mjs`, and
  `smoke.mjs`: native core and utility components.
- `integrations/mount-rs-napi/test/js-driver.mjs` and `differential.mjs`:
  oracle-backed only when `MOUNTX_SOURCE` is set; both contain explicit skip
  paths when it is not.
- `nfs-codec.mjs` and `9p-codec.mjs`: focused wire/codec comparisons, not full
  transport parity.
- `fuse-codec.mjs` and `fuse-inodes.mjs`: focused FUSE codec and inode-table
  comparisons; they do not prove FUSE init/session/mount parity.
- `servers.mjs`: loopback NFS/P9/S3/WebDAV/server-lifecycle behavior where its
  individual cases run; it does not cover every oracle member.
- Rust FUSE, 9P, NFS, S3, WebDAV, and CLI tests: component or parser evidence;
  native mount and live service checks require their own prerequisites.

Any verification record appended to this file must name the exact command and
whether it passed, failed, or skipped. A skipped oracle/native/live test is a
blocker or limitation, never a pass.

### Verification run for this refresh (2026-09-20)

- **PASS** — `cargo test --locked -p mount-rs-fuse --test notify_record`:
  6 passed, 0 failed, 0 ignored. This closes only the FUSE notify/record
  component cases described above.
- **PASS** — `cargo test --locked -p mount-rs-s3 --test public_api`:
  5 passed, 0 failed, 0 ignored. This covers the S3 public constants, XML,
  and SigV4 fixtures; it is not a gateway or live-service result.
- **PASS** — `cargo test --locked -p mount-rs-cli --test cli`:
  7 passed, 0 failed, 0 ignored. This covers parser/configuration behavior,
  not native mount lifecycle.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX node test/js-driver.mjs`:
  the oracle-backed JS `FsDriver` adapter comparison passed.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX node test/differential.mjs`:
  the native core differential comparison passed.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX node test/nfs-codec.mjs`:
  the NFS codec differential passed.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX node test/9p-codec.mjs`:
  the 9P codec differential passed 44 typed cases.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX node test/utilities.mjs`:
  the utility differential passed.
- **BLOCKED** — `node test/servers.mjs`: NFS listen failed with
  `Operation not permitted (os error 1)` while the lifecycle assertion was
  exercising loopback TCP; no server integration result is claimed.
- **BLOCKED** — `cargo test --locked -p mount-rs-9p --test protocol_integration`:
  1 lifecycle test passed; the TCP loopback test failed while binding with
  `PermissionDenied (Operation not permitted)` in the restricted environment.
- **BLOCKED** — `cargo test --locked -p mount-rs-nfs --test rootless_wire`:
  the real TCP round-trip could not bind and failed with the same
  `PermissionDenied (Operation not permitted)` error.
- **PASS** — `git diff --check -- docs/public-api-parity.md` reported no
  whitespace errors.

### Main follow-up after workspace integration (2026-09-20)

The worker's three loopback sandbox failures above were rerun with loopback
permission against the current checkout after `6ca7832`:

- **PASS** — `node test/servers.mjs`: N-API server integration completed.
- **PASS** — `cargo test --locked -p mount-rs-9p --test protocol_integration`:
  2 tests, including the real TCP loopback test.
- **PASS** — `cargo test --locked -p mount-rs-nfs --test rootless_wire`:
  1 real TCP/filesystem round-trip test.

These supersede the sandbox-only blockers for those commands, not the broader
public API and native-mount gaps above. Structural factory changes are still
in progress and not included in this evidence.

No native mount, live TiDB, live AWS, or live WebDAV
result is claimed by this verification run. The separate integrated fault
test at `decb71f` is outside this document's evidence boundary unless it is
shown to exercise and verify one of the APIs above.

### W01 sidecar verification after SDK-backed CLI work (2026-09-21)

- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-parity-target node scripts/check-parity.mjs`:
  the deterministic Rust/TypeScript trace matched at the pinned oracle
  revision `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-parity-target node tests/core_parity/check.mjs`:
  101 steps, 79 successful results, 22 stable expected errors, and zero
  mismatches or skips. The trace includes the `lchown` symlink lifecycle row.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-parity-target node tests/core_concurrency/check.mjs`:
  six scenarios and zero mismatches; five unsupported scope classifications
  remain explicit.
- **PASS** — `git diff --check -- docs/public-api-parity.md scripts/check-parity.mjs`.

- **PASS** — the latest W01 packet checks add 14 explicit remaining Unstorage
  rows (4 PASS, 10 ENOSYS, zero ENOTSUP/skips) and Rust-backed FUSE `CREATE`
  request/reply differential coverage for protocol 7.41/7.39/7.12/7.8.

The deterministic checker now rejects a configured but non-pinned
`MOUNTX_SOURCE` and uses Cargo's locked mode. These checks still do not claim
the closure items listed above.

### SDK-backed CLI and transport follow-up (2026-09-21)

- **PASS** — `cargo test --locked --offline -p mount-rs-cli`: the actual
  `mount-rs` binary's `sdk-self-test` passed for memory and a SQLite-backed
  split store with shutdown/reopen readback; 39 unit tests and 8 CLI tests
  passed, with only the explicit native opt-ins ignored.
- **PASS** — `node integrations/mount-rs-napi/test/node-cli.mjs`: the Node
  SDK CLI passed its direct memory self-test and SQLite split-store
  shutdown/reopen test.
- **PASS** — `node tests/provider_matrix/cli.mjs`: 8 process-level CLI cases
  passed and 2 provider-gated cases were explicit skips; no failures.
- **PASS** — `cargo run --manifest-path tests/provider_matrix/Cargo.toml
  --offline --locked`: Rust SDK memfs, memory/memory, direct SQLite reopen,
  and SQLite/SQLite reopen rows passed; PGlite/R2 rows remained explicit
  skips without their environment gates.
- **PASS** — the oracle-enabled N-API suite, including public FUSE
  `READDIRPLUS`, completed its functional tests and artifact aggregation.
- **PASS** — `bash scripts/demo-end-to-end.sh` on macOS built the Rust CLI,
  mounted a HostFs through native NFS, exercised independent Rust and Node
  processes writing and reading through the same mount, verified the Node
  bytes from Rust after the write, and confirmed clean unmount plus backing
  directory persistence. This does not qualify FUSE, FSKit or remote providers.
- **PASS** — the matching Node CLI command with a temporary HostFs backing and
  `--transport nfs --self-test` mounted through the Node SDK, wrote and read
  through the native mount, unmounted, and left its backing directory clean.
  Linux FUSE, FSKit and live-provider acceptance remain separate.

### W01 parallel packet evidence (2026-09-21)

- **PASS** — `a31880d` provider matrix: seeded positional writes, truncate,
  flush and reopen passed for 5 Rust SDK, 4 Node SDK and 9 CLI cases; PGlite
  and R2 remained explicit prerequisite skips.
- **PASS** — `cc73ad5` Unstorage path/metadata/handle parity: 11 oracle rows,
  zero mismatches and zero skips, alongside the capability, edge, N-API and
  upstream conformance gates. Hardlinks, symlinks, `statfs` and special-node
  creation remain explicit capability limitations.
- **PASS** — `a3795f0` FUSE `OPEN`/`OPENDIR` request codecs: protocol 7.8,
  7.39 and 7.41 differential coverage, malformed/truncated/trailing checks,
  typecheck, Clippy and the full N-API suite. Native FUSE session/mount is
  intentionally not inferred from this mount-free evidence.
- **PASS** — `16b2180` Rust FUSE `ACCESS` session dispatch: exact 8-byte
  validation, permission-mask handling, root behavior, invalid-mask errors and
  focused locked session tests. Native device and mount lifecycle remain open.
- **PASS** — `7dd9a60` N-API FUSE `LOOKUP` request/reply codecs: protocol
  7.8/7.39/7.41 differential coverage, generated declarations/artifacts and
  malformed/truncated/trailing-input checks. The combined locked Rust and
  oracle-enabled N-API gates passed; native FUSE remains unqualified.
- **PASS** — `13c3acb` N-API FUSE lifecycle codecs: pinned differential
  coverage for `RELEASE`/`RELEASEDIR`, `FLUSH`, `FSYNC`/`FSYNCDIR`, including
  empty status framing and malformed/truncated/trailing inputs. The full
  N-API suite passed; the scoped Clippy run keeps the known pre-existing
  `js_driver.rs` type-complexity warning excluded.
- **PASS** — `24c8a6b` N-API FUSE request/reply codecs: typed
  `SYMLINK`/`MKNOD`/`MKDIR`/`UNLINK`/`RMDIR`/`RENAME`/`RENAME2`/`LINK`/
  `ACCESS`/`FALLOCATE`/`LSEEK` layouts, generated declarations and explicit
  public exports passed the pinned mountx byte/decode differentials and the
  complete N-API artifact-aggregation gate. Native FUSE session/device/mount
  acceptance remains open.
- **PASS** — `b2040f6` pure Rust FUSE INIT hardening: six locked integration
  tests pass for version retry/downgrade, extension flags, max-pages/write
  clamping, compatibility layouts and truncation; strict scoped Clippy passed.
- **PASS** — `72d570f` Windows HostFs read-only/link packet: 15 macOS tests,
  Windows-target check and target Clippy passed; native Windows runtime remains
  a hosted-CI boundary.
- **PASS** — `08731ed` pure Rust FUSE `READLINK`/`STATFS` packet: typed
  session replies, negotiated protocol context, legacy STATFS layout,
  Unicode/raw targets, empty framing, truncation, trailing bytes and embedded
  NUL rejection passed in the locked FUSE suite; strict scoped Clippy passed.
- **PASS** — `054fb95` napi-rs FUSE `READLINK`/`STATFS` packet: generated
  JavaScript/declaration artifacts and pinned-oracle byte-level differentials
  passed for supported protocol minors, malformed/truncated/trailing inputs
  and malformed NUL targets; the known pre-existing `js_driver.rs` complexity
  lint remains excluded from the scoped Clippy command.
- **PASS** — `8e1f218` Node SDK CLI native integration: on macOS, the exact
  Node CLI mounted a HostFs through NFS, an independent Node process read and
  wrote through the mount, SIGINT triggered clean unmount, and the backing root
  retained the bytes. The same test is opt-in and Linux FUSE-gated.
- **PASS** — combined current-tree gate: `cargo fmt --all -- --check`,
  `cargo test --workspace --all-targets --all-features --locked --offline`,
  `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX pnpm test` in the N-API package,
  and `MOUNT_RS_NODE_CLI_NATIVE_INTEGRATION=1 node
  examples/node-cli/native-integration.mjs` all exited 0. PGlite/R2 and
  hosted/native platform gates remain explicit boundaries.
- **PASS** — `7934062` Rust FUSE `BATCH_FORGET`/`INTERRUPT` packet: strict
  count/payload validation, no-reply behavior, exact interrupt-body checking,
  unknown-target `EAGAIN`, malformed/truncated coverage, 48 package tests and
  strict scoped Clippy passed.
- **PASS** — `4dc90d5` napi-rs FUSE `BATCH_FORGET`/`INTERRUPT` packet:
  generated bindings/declarations and pinned-oracle protocol-minor,
  malformed-count, truncation, trailing-byte and empty-reply tests passed;
  full N-API suite passed with only explicit PGlite/R2/native skips.
- **PASS** — `b13350f` Unstorage remaining-skip parity: 25 pinned-oracle
  rows passed with 4 supported results, 21 explicit `ENOSYS` classifications,
  0 `ENOTSUP` mismatches and 0 skips; exact error code, errno, syscall, path
  and message assertions are retained.
- **PASS** — second-rotation combined gate: workspace format, the full locked
  offline Rust workspace, the full oracle-enabled N-API suite, the 25-row
  Unstorage parity packet, and the prior macOS NFS Node CLI acceptance all
  exited 0. Hosted Linux/Windows, FSKit, live R2/PGlite and native kernel
  cancellation remain explicit boundaries.

### W01 third parallel rotation evidence (2026-09-21)

- **PASS** — `15026f2` Rust FUSE `POLL` session boundary: valid 24-byte
  requests reach the explicit safe `ENOSYS` boundary; truncated, malformed and
  trailing frames are rejected with `EINVAL`. The locked FUSE package suite
  passed 50 tests and the scoped `-D warnings` Clippy gate passed.
- **PASS** — `5b7f982` napi-rs FUSE `POLL` request/reply codecs: generated
  JavaScript/declaration artifacts, protocol-minor pinned-oracle differentials,
  malformed/truncated/trailing input checks, package build, TypeScript checks
  and the full N-API suite passed. Existing FUSE declarations were retained.
- **PASS** — `d28f31a` CI wiring: the Node SDK CLI native mount gate is scoped
  to Linux/macOS, opt-in, prerequisite-probed and bounded. Local macOS NFS
  acceptance passed; hosted CI results remain unverified.
- **PASS** — combined current-tree gate: `cargo fmt --all -- --check`,
  `cargo test --workspace --all-targets --all-features --locked --offline`,
  and `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX pnpm test` in the N-API package
  exited 0. FUSE `POLL` is still intentionally unsupported at the driver
  semantics boundary, and hosted/native platform gates remain open.

### W01 fourth parallel rotation evidence (2026-09-21)

- **PASS** — `1a8b112` Rust FUSE advanced-operation session boundary: valid
  `FALLOCATE`, `RENAME2`, `LSEEK`, and `COPY_FILE_RANGE` requests reach the
  explicit safe `ENOSYS` boundary; malformed, truncated, and trailing frames
  are rejected with `EINVAL`; and unsupported requests do not mutate state or
  kill the session. The focused locked FUSE suite and strict scoped Clippy
  gate passed; the packet was published through remote commit `7c6186f`.
- **PASS** — `cf7a132` napi-rs FUSE xattr packet: `SETXATTR`, `GETXATTR`,
  `LISTXATTR`, and `REMOVEXATTR` request/reply codecs match the pinned oracle
  across protocol contexts; generated artifacts/declarations, malformed,
  truncated, trailing and declared-size checks, build, typecheck and the full
  focused suite passed. The packet was published through `4f484ad`.
- **PASS** — `e0e8195` Unstorage capability-boundary packet: 14 pinned rows
  passed with 5 supported results, 9 exact `ENOSYS` classifications, zero
  `ENOTSUP` mismatches and zero skips; the packet was published through
  `9b74c87`.
- **PASS** — `99c7d32` Unstorage hardlink capability packet: four pinned
  rows passed with zero supported results, four exact `ENOSYS` classifications,
  zero `ENOTSUP` mismatches and zero skips; every row retained errno `-38`,
  syscall `link`, and null path/destination fields. The packet was published
  through `2bce444`.
- **PASS** — `66c79e0` Rust FUSE session parity packet: 16 frame-level tests
  cover simple namespace operations, MKNOD fallback, ACCESS semantics, error
  cleanup and POSIX name limits; focused FUSE tests, strict Clippy and formatting
  passed. Native device/mount acceptance and FALLOCATE/LSEEK semantics remain
  open.
- **PASS** — `6972fbe` Unstorage hardlink alias packet: six exact `ENOSYS`
  rows now cover inode-sharing, destination/source errors, alias write-through
  and unlink lifetime in direct and N-API execution, with zero `ENOTSUP`
  mismatches and zero skips. Generic hardlink inode support remains open.
- **PASS** — `462d54b` Unstorage remaining-skip evidence packet: preparations
  execute against both adapters, refusal rows verify state preservation, and
  the direct/N-API chain covers 31 rows with 5 PASS, 26 exact `ENOSYS`, zero
  `ENOTSUP` mismatches and zero skipped rows.
- **PASS** — `25644c5` Rust FUSE `RENAME2` session packet: plain-flag rename
  is supported, unsupported flags return `ENOSYS` without mutation, and the
  focused 16-test suite plus strict Clippy passed. FALLOCATE, LSEEK and
  COPY_FILE_RANGE remain intentionally unsupported; this is not native-kernel
  mount evidence.
- **PASS** — `a3d980d` Node SDK CLI cross-language native gate: the authorized
  macOS NFS run mounted through the Node CLI, exercised independent Rust and
  Node clients, verified Rust readback of Node-written bytes, unmounted and
  retained backing data. Linux FUSE and live-provider acceptance remain open.

This follow-up proves the process-level SDK consumer paths, not native mount
support or live R2/PGlite acceptance. The remaining transport/session and
hosted/live boundaries stay open below.

### W01 current FUSE oracle refresh (2026-09-22)

- **PASS** — from `integrations/mount-rs-napi`,
  `MOUNTX_SOURCE=/private/tmp/mountx-source-w01-20260921 node
  test/fuse-codec.mjs` passed all listed FUSE protocol differential families,
  including READDIR/READDIRPLUS, read/write, metadata, lookup/create, links,
  statfs, forget/interrupt/poll, BMAP/IOCTL, locking, xattrs, and release/
  flush/fsync request/status boundaries.
- **PASS** — with the same pinned source,
  `node test/fuse-inodes.mjs` passed Rust-backed FUSE inode parity.
- This refresh is focused mount-free oracle evidence; it does not claim the
  full N-API package matrix, privileged Linux FUSE, native callback/lifecycle,
  FSKit, cancellation/concurrency, crash/restart, or durability acceptance.

### W01 FUSE session unsupported-operation boundary (2026-09-22)

- **PASS** — `./scripts/cargo-shared test -p mount-rs-fuse --all-targets
  --locked` passed the complete FUSE target: 14 unit, 6 INIT, 6 notify/record,
  11 protocol, 20 session, and 3 sync-barrier tests. The new session checks
  validate codec-backed `BMAP`, legacy and negotiated `SETXATTR`, `GETXATTR`,
  `LISTXATTR`, and `REMOVEXATTR` bodies before valid unsupported requests return
  `ENOSYS`; malformed forms return `EINVAL`.
- **PASS** —
  `CARGO_TARGET_DIR=/private/tmp/mount-rs-clippy-fuse-validate-20260922
  ./scripts/cargo-shared clippy -p mount-rs-fuse --all-targets --locked --
  -D warnings` passed.
- This remains a mount-free validation boundary. Native xattr/BMAP support is
  not advertised or implemented, and hosted Linux FUSE, callback/lifecycle,
  FSKit, cancellation/concurrency, crash/restart, and durability acceptance
  remain open.

### W01 lock-codec packet evidence (2026-09-21)

- **PASS** — the N-API FUSE barrel now exports typed `GETLK`, `SETLK` and
  `SETLKW` request codecs plus the typed `GETLK` reply codec. Generated native
  bindings, root declarations, the `./fuse` facade and explicit ESM/CommonJS
  exports are covered by `node test/typecheck.mjs`.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX node
  test/fuse-codec.mjs` matched the pinned oracle for request/reply bytes and
  decoded values, all request/reply truncation boundaries, trailing bytes and
  empty status replies for `SETLK`/`SETLKW`.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX pnpm test` completed the
  full N-API suite, including server, codec, Unstorage, restart and artifact
  aggregation checks. PGlite/R2 factory rows and opt-in native mounts remained
  explicit skips.
- **PASS** — `./scripts/cargo-shared fmt --all -- --check` and
  `CARGO_TARGET_DIR=/Users/andrewmcclenaghan/Library/Caches/mount-rs/cargo-target
  ./scripts/cargo-shared test -p mount-rs-fuse --locked --offline` passed.
  This does not establish privileged native FUSE session or mount acceptance.

## Working-tree provenance and freeze boundary

- Preserve all unrelated dirty, staged, and untracked work in the shared
  checkout.
- This packet changes the Rust-backed N-API FUSE codec facade, generated
  declarations, focused tests and the associated tracker/parity evidence.
- The packet is committed and pushed only after the local gates above pass;
  native mount, hosted-platform and live-provider claims remain separate.
- The oracle SHA and current source links above are the evidence boundary for
  this refresh. Broad completion must not be inferred from a green component
  test, an ignored native test, or a missing live-service prerequisite.
