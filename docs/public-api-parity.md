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
| FUSE Node subpath and full protocol barrel | Rust notify/record/protocol pieces, typed `READ`/`WRITE`/`GETATTR`/`SETATTR`/`OPEN`/`OPENDIR`/`LOOKUP`/`READLINK`/`STATFS`/`BATCH_FORGET`/`INTERRUPT`/`RELEASE`/`RELEASEDIR`/`FLUSH`/`FSYNC`/`FSYNCDIR` bodies, directory codecs, and a Rust-backed `InodeTable` are exported through `./fuse`; Rust session `ACCESS`, validated `BATCH_FORGET`, and fail-closed `INTERRUPT` dispatch plus pure Rust INIT negotiation are tested separately; remaining bodies, session, and mount surfaces remain open | **IMPLEMENTED (focused); PARTIAL** |
| NFS/P9/S3/WebDAV server objects | Native servers and postbuild lifecycle facade exist; several oracle object members and callback options are absent | **PARTIAL** |
| S3 low-level Node API and structural bucket sources | Native Rust low-level API and native `Filesystem` bucket map exist; JS structural drivers and Node codec barrel do not | **PARTIAL** |
| WebDAV low-level public API | Rust server/session/protocol exist; constants are not public and Node has only the root server facade | **PARTIAL** |
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
  do not establish the complete upstream 9P server object or all protocol
  behavior.
- The package now has a `./fuse` export. Its codec barrel covers the currently
  bound notify/record/protocol helpers, and its `InodeTable` facade delegates to
  the Rust transport table. The complete request/reply body codec, session,
  and mount objects are still not exposed at the N-API boundary; see
  [`fuse.cjs`](../integrations/mount-rs-napi/fuse.cjs) and
  [`fuse_inodes.rs`](../integrations/mount-rs-napi/src/fuse_inodes.rs).

**Required closure evidence:** expose the remaining FUSE request/reply body,
session, and native-mount surfaces, then run oracle-backed subpath tests for
every exported transport rather than treating codec or inode fixture tests as
transport completion.

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
The N-API package still does not expose the complete request/reply body codec,
init negotiation, session, or native mount objects. Therefore this packet
proves focused body/inode components only, not full FUSE transport parity.

### P1 — auto/mount option and lifecycle parity: PARTIAL; UNVERIFIED

The current root facade exposes `mount`, `liveMounts`, `unmountAll`, and
`probeTransports` through [`postbuild.mjs`](../integrations/mount-rs-napi/postbuild.mjs#L6-L22).
The native `Mounted` object exposes transport, mountpoint, source, active, and
`unmount`; see [`index.d.ts`](../integrations/mount-rs-napi/index.d.ts#L135-L141).

The public auto options are currently only `transport`, `readOnly`,
`unmountTimeout`, and `nfsSqliteSingleHost`.
[`JsAutoMountOptions`](../integrations/mount-rs-napi/index.d.ts#L478-L487) does
not cover the oracle's signals, `useDriverIno`, `onError`,
`onTransportError`, or transport-specific `fuse`/`9p`/`nfs` option bags. The
Rust auto layer has typed transport selection and timeout handling, but this
does not close the upstream option or lifecycle surface.

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
- P9 exposes native clients, connection session access, and the added closed
  promise, but not the oracle connection `stream` or server `attach(stream,
  options)` contract.
- NFS and S3 do not expose the oracle's session/connections members at the
  N-API object boundary. WebDAV exposes `connections` but not the oracle
  session member. S3 lacks the oracle's `drainTimeout` and
  `onTransportError` options; WebDAV currently has `drainTimeout` but still
  lacks `onTransportError`. Compare the current native options and objects in
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
the native extractor, with real server isolation coverage. Invalid-name and empty-map behavior
also needs oracle-backed coverage before this item can be promoted.

### P2 — S3 low-level and streaming parity: PARTIAL

Rust exposes public chunked, constants, protocol, server, session, SigV4, and
XML modules through [`mount-rs-s3/src/lib.rs`](../transports/mount-rs-s3/src/lib.rs#L1-L52).
The incremental chunked decoder and streamed HTTP request/response paths are
present in [`chunked.rs`](../transports/mount-rs-s3/src/chunked.rs#L1-L7) and
[`server.rs`](../transports/mount-rs-s3/src/server.rs#L146-L215). Public API,
XML, SigV4, gateway, and chunked tests exist, including
[`public_api.rs`](../transports/mount-rs-s3/tests/public_api.rs#L20-L213).

The Node `./s3` entry is currently a root server facade, not an oracle-equivalent
codec barrel. The transport README records known behavior boundaries including
List V1, bucket create/delete, `partNumber`, and non-`/` delimiters; see
[`README.md`](../transports/mount-rs-s3/README.md#L21-L28). Rust component tests
and a native gateway test do not establish complete oracle parity or a live AWS
service result.

### P2 — WebDAV public barrel and constants: PARTIAL

The Rust WebDAV server, session, protocol, and lock modules are public, but the
constants module is private and only `DEFAULT_HOST` is re-exported from the
barrel; see [`mount-rs-webdav/src/lib.rs`](../transports/mount-rs-webdav/src/lib.rs#L30-L51).
The N-API `./webdav` entry is a root server facade with no low-level WebDAV
codec/constants/session barrel. Current server lifecycle behavior therefore
does not close the oracle WebDAV package surface.

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
   checks now pass. The remaining inventory still identifies hardlinks (6 rows),
   symlinks/link timestamps (16 + 2 rows), `statfs` (2 rows), special-node
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
   `RENAME2`, `LSEEK`, and `COPY_FILE_RANGE` body/session boundaries are now
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

This follow-up proves the process-level SDK consumer paths, not native mount
support or live R2/PGlite acceptance. The remaining transport/session and
hosted/live boundaries stay open below.

## Working-tree provenance and freeze boundary

- Preserve all unrelated dirty, staged, and untracked work in the shared
  checkout.
- This update changes only `docs/public-api-parity.md` and
  `scripts/check-parity.mjs`; it makes no implementation, root manifest,
  workflow, or tracker changes.
- No commit or push is part of this work.
- The oracle SHA and current source links above are the evidence boundary for
  this refresh. Broad completion must not be inferred from a green component
  test, an ignored native test, or a missing live-service prerequisite.
