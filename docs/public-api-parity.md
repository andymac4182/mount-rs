# Public API parity ledger

This ledger describes the current working tree, not a release claim. It was
recovered from `git show HEAD:docs/public-api-parity.md` after the interrupted
edit and refreshed against the current source, tests, and the pinned oracle:

- repository baseline when the deleted file was recovered:
  `ac2161d27f4a6805b87580fbee20c1302e9cd9df`
- current local integration baseline observed during this packet:
  `323231f`
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
| FUSE Node subpath and full protocol barrel | Rust notify/record/protocol pieces and a Rust-backed `InodeTable` are exported through `./fuse`; full request/reply body, session, and mount surfaces remain open | **IMPLEMENTED (focused); PARTIAL** |
| NFS/P9/S3/WebDAV server objects | Native servers and postbuild lifecycle facade exist; several oracle object members and callback options are absent | **PARTIAL** |
| S3 low-level Node API and structural bucket sources | Native Rust low-level API and native `Filesystem` bucket map exist; JS structural drivers and Node codec barrel do not | **PARTIAL** |
| WebDAV low-level public API | Rust server/session/protocol exist; constants are not public and Node has only the root server facade | **PARTIAL** |
| CLI | Rust CLI constructs drivers through `mount-rs-sdk`; Node CLI has direct SDK and versioned provider-config/reopen self-tests; native mount/live behavior is not established by ordinary tests | **PARTIAL; UNVERIFIED** |

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
- the N-API `./fuse` barrel now also exposes oracle-shaped `packDirents` and
  `unpackDirents` for bounded `READDIR` bodies. UTF-8 names, 8-byte alignment,
  bounded packing and malformed input are differentially tested against the
  pinned oracle in [`fuse-codec.mjs`](../integrations/mount-rs-napi/test/fuse-codec.mjs);
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
`READDIRPLUS`, init negotiation, session, or native mount objects. Therefore
this packet proves focused body/inode components only, not full FUSE transport
parity.

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
selection, and configuration validation in
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
4. **Remaining public transport/API surface (P1):** the FUSE `READDIR` body
   codec is now public and oracle-differentially tested. Expose the remaining
   request/reply bodies, `READDIRPLUS`, session and native-mount surfaces, then
   run oracle-backed subpath tests for every exported transport rather than
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
  56 steps, 35 successful results, 21 stable expected errors, and zero
  mismatches.
- **PASS** — `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-parity-target node tests/core_concurrency/check.mjs`:
  six scenarios and zero mismatches; five unsupported scope classifications
  remain explicit.
- **PASS** — `git diff --check -- docs/public-api-parity.md scripts/check-parity.mjs`.

The deterministic checker now rejects a configured but non-pinned
`MOUNTX_SOURCE` and uses Cargo's locked mode. These checks still do not claim
the closure items listed above.

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
