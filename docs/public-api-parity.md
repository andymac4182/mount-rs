# Public API parity ledger

This is the bounded completion ledger for the pinned `mountx` public surface and
the current Rust/N-API workspace. It covers functional API and semantic parity,
not TypeScript spelling alone. An implemented entry is not proof of complete
acceptance; use the linked gates and revision-specific evidence.

## Reading this ledger

- Upstream pointers refer to `pithings/mountx` revision
  `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` and are written
  as `upstream: path:line-range`. That source tree is outside this repository,
  so those pointers are intentionally not repo links.
- Repository links are relative to this file, so they remain valid when the
  repository is moved.
- `IMPLEMENTED / UNVERIFIED` means the current tree contains the behavior and
  usually has a focused test, but this audit did not execute it.
- `PARTIAL` means a useful native implementation exists but one or more
  upstream behaviors or public surfaces are still absent.
- `MISSING` means there is no equivalent callable behavior at the compared
  boundary.

The upstream package export map is `upstream: package.json:22-62`; its root
barrel is `upstream: src/index.ts:1-5`. The Rust/N-API additions mentioned below
are implemented in this repository; verification status remains separate from
implementation status.

## Priority ledger

### P0 — expose the upstream driver boundary to Node

**Status: MISSING.**

Upstream defines a structural `FsDriver`: only `stat`, `readdir`, and `open` are
required; optional operations, capabilities, extensions, normalized paths, and
`ENOSYS`/`ENOTSUP` behavior are part of the contract (`upstream:
src/types.ts:212-251`). The root package exports the loopback harness and
capability resolution (`upstream: src/index.ts:1-5`, `upstream:
src/harness.ts:20-193`). Mount and server factories consume that driver shape,
not a native class (`upstream: src/auto.ts:350-387`, `upstream:
src/s3/server.ts:720-746`).

The current N-API boundary instead requires the native `Filesystem` class for
mount and every server factory ([declarations](../integrations/mount-rs-napi/index.d.ts#L197-L229),
[mount binding](../integrations/mount-rs-napi/src/lib.rs#L2142-L2155)). The
Unstorage adapter is a deliberate special case, not a general JS-driver
adapter.

**Acceptance tests:**

1. Pass a plain JS driver implementing only `stat`, `readdir`, and `open` to
   `mount`, `createP9Server`, `createNfsServer`, `createS3Server`, and
   `createWebdavServer`; each must use it without requiring a native
   `Filesystem` instance.
2. Exercise an optional operation that is absent and assert the upstream
   `ENOSYS`/`ENOTSUP` result rather than a fabricated success.
3. Exercise `createLoopback` with relative, duplicate-slash, and trailing-slash
   paths; assert normalization, capability resolution, and preserved driver
   error identity.
4. Run the same driver through the Node and Rust conformance paths and compare
   the observable errno, stats, directory, and handle behavior.

### P0 — restore real transport subpaths in Node

**Status: MISSING at the N-API boundary; PARTIAL in Rust crates.**

Upstream intentionally publishes separate, functional subpaths rather than one
facade: FUSE (`upstream: src/fuse/index.ts:34-41`), 9P (`upstream:
src/9p/index.ts:29-57`), NFS (`upstream: src/nfs/index.ts:40-195`), S3
(`upstream: src/s3/index.ts:30-82`), and WebDAV (`upstream:
src/webdav/index.ts:32-36`). These include protocol codecs, sessions, servers,
record/replay, XML, SigV4, and chunked-stream helpers.

The N-API package maps those package subpaths to the same generated facade
([package exports](../integrations/mount-rs-napi/package.json#L22-L62)); the
facade adds factories and utility functions ([facade additions](../integrations/mount-rs-napi/postbuild.mjs#L6-L21)),
but not the low-level callable transport modules. The Rust 9P and NFS crate
barrels are already broad ([9P](../transports/mount-rs-9p/src/lib.rs#L18-L38),
[NFS](../transports/mount-rs-nfs/src/lib.rs#L10-L36)); they are not yet exposed
as equivalent Node subpath APIs.

**Acceptance tests:**

1. Import each exported subpath from Node and verify it resolves to a
   subpath-specific surface, not the root facade.
2. Call representative low-level functions from every subpath: a FUSE
   request/reply codec, a 9P frame/session codec, an NFS XDR/RPC codec, an S3
   XML/SigV4/chunked codec, and a WebDAV request/document codec.
3. Typecheck those imports against subpath declarations and assert that a
   missing low-level export fails at build time rather than being silently
   absent at runtime.

### P1 — complete the Rust FUSE public layer

**Status: MISSING.**

The upstream FUSE surface includes invalidation notifications, transcript
record/replay, and the complete protocol codec in addition to mounting and
sessions (`upstream: src/fuse/notify.ts:72-164`, `upstream:
src/fuse/record.ts:42-224`, `upstream: src/fuse/protocol.ts:2740-2966`). The
current Rust crate publicly exposes only device/init/inodes/mount/session and a
small request parser ([crate barrel](../transports/mount-rs-fuse/src/lib.rs#L3-L115)).

**Acceptance tests:**

1. Round-trip every public FUSE header/body family and directory-entry packing
   through Rust, with malformed/truncated input returning the corresponding
   refusal.
2. Match the upstream `notifyInvalInode` and `notifyInvalEntry` byte layouts.
3. Record a request/reply transcript, replay it through a session without a
   kernel or mount, and compare bytes, errors, and final session state.
4. Export the same callable layer through the N-API `fuse` subpath.

### P1 — restore auto-mount options and lifecycle semantics

**Status: PARTIAL.**

Upstream auto-mount accepts signal control, driver inode selection, request and
transport error callbacks, and transport-specific option bags; it merges shared
and transport options before calling the selected transport (`upstream:
src/auto.ts:107-152`, `upstream: src/auto.ts:312-387`). Its returned tagged mount
preserves the selected transport object, including `closed`, sessions,
connections, servers, `await using`, and transport-specific methods (for
example `upstream: src/fuse/mount.ts:224-244`, `upstream:
src/9p/mount.ts:311-328`).

Rust auto-mount has transport-specific option structs but lacks the upstream
shared callbacks and signal/inode fields ([options](../transports/mount-rs-auto/src/lib.rs#L93-L100),
[lifecycle](../transports/mount-rs-auto/src/lib.rs#L190-L265)). The N-API
surface narrows this further to three fields ([options](../integrations/mount-rs-napi/index.d.ts#L256-L260));
`Mounted` exposes only transport, mountpoint, source, active, and unmount
([declaration](../integrations/mount-rs-napi/index.d.ts#L132-L138),
[binding](../integrations/mount-rs-napi/src/lib.rs#L1492-L1526)).

**Acceptance tests:**

1. Mount with `signals: false`, `useDriverIno`, `onError`, and
   `onTransportError`; assert each option reaches the selected transport and
   callbacks receive the original failure class.
2. Pass FUSE, 9P, and NFS-specific options through `auto.mount` and assert the
   transport-specific values override shared values.
3. For each transport, assert the returned object exposes its session/server
   fields, resolves `closed` after teardown, supports `await using`, and keeps
   `liveMounts`/`unmountAll` behavior consistent with direct transport mounts.

### P1 — complete embedded-server contracts at the Node boundary

**Status: PARTIAL.**

Upstream 9P exposes per-connection session, stream, peer, closed lifecycle, and
`attach(stream, options)` on the server (`upstream: src/9p/server.ts:253-325`,
`upstream: src/9p/server.ts:876`). NFS, S3, and WebDAV expose transport error
callbacks in their options (`upstream: src/nfs/server.ts:77-121`, `upstream:
src/s3/server.ts:210-236`, `upstream: src/webdav/server.ts:161-180`). S3 and
WebDAV server objects expose their session, connections, async disposal, and a
chainable `listen(): Promise<Server>` (`upstream: src/s3/server.ts:238-261`,
`upstream: src/webdav/server.ts:182-202`).

The current N-API wrappers provide useful native server lifecycle methods, but
their `listen()` methods return `Promise<void>`, P9 has no Node `attach` or
session/stream exposure, and the server classes do not expose the upstream
async-disposal/callback contract ([types](../integrations/mount-rs-napi/index.d.ts#L140-L193),
[P9 wrapper](../integrations/mount-rs-napi/src/servers.rs#L330-L543),
[server listen implementations](../integrations/mount-rs-napi/src/servers.rs#L224-L243)).

**Acceptance tests:**

1. Attach an actual framed P9 client stream, inspect the connection's session,
   stream/peer, and closed promise, then close the connection and server without
   fabricating an error for an orderly EOF.
2. Assert `await server.listen() === server` for NFS, P9, S3, and WebDAV;
   repeated `listen` and `close` calls remain idempotent.
3. Use `await using` with each server and assert listeners, connections, and
   driver/session resources are released after scope exit.
4. Inject a transport failure and assert `onTransportError` receives it once,
   with the correct peer where the upstream contract supplies one.

### P1 — support S3 bucket maps in Node

**Status: MISSING.**

Upstream accepts either one driver or a bucket map, with bucket names owned by
the map (`upstream: src/s3/server.ts:207-216`). The current N-API factory always
creates one bucket from one native filesystem ([factory](../integrations/mount-rs-napi/src/servers.rs#L729-L748),
[declaration](../integrations/mount-rs-napi/index.d.ts#L177-L184)).

**Acceptance tests:**

1. Create two independent drivers and start one S3 server from a two-bucket
   map; assert `buckets`, listing, reads, and writes remain isolated by bucket.
2. Verify duplicate, invalid, and empty bucket names fail before listening with
   the upstream error category.
3. Preserve the single-driver-plus-`bucket` convenience form and assert both
   forms expose the same session, URL, close, and drain semantics.

### P2 — expose S3 low-level APIs and match streaming semantics

**Status: PARTIAL.**

The upstream S3 barrel publishes `chunked`, constants, protocol, server, session,
SigV4, and selected XML codecs (`upstream: src/s3/index.ts:30-82`). The AWS
chunked decoder is incremental and releases signed bytes only after verification
(`upstream: src/s3/chunked.ts:18-75`). Rust has internal S3 protocol/session/
SigV4 code, but the modules are private ([crate barrel](../transports/mount-rs-s3/src/lib.rs#L7-L20));
the implementation documents that its HTTP boundary buffers request bodies
instead of matching the upstream incremental boundary ([README](../transports/mount-rs-s3/README.md#L21-L28)).

**Acceptance tests:**

1. Export and invoke the public XML, S3 error mapping, SigV4, and AWS-chunked
   codecs from Rust and Node; compare golden bytes and refusal categories with
   upstream.
2. Feed signed and unsigned chunked uploads in fragmented input, overwrite the
   caller's input buffers after each write, and assert only verified signed bytes
   reach the driver.
3. Send a body larger than one frame through a streaming driver and assert
   bounded incremental memory use, correct truncation/trailer refusal, and no
   whole-request buffering at the HTTP boundary.

### P2 — restore WebDAV public constants

**Status: MISSING at the public barrel; PARTIAL in Rust behavior.**

Upstream exports the WebDAV constants barrel (`upstream: src/webdav/index.ts:32-36`),
including the errno-to-status table and protocol literals. Rust keeps
`constants` private and re-exports only `DEFAULT_HOST` ([crate barrel](../transports/mount-rs-webdav/src/lib.rs#L30-L50)).

**Acceptance tests:**

1. Import the WebDAV constants from the Node/Rust subpath and compare every
   exported status, reason phrase, and protocol literal with upstream.
2. Assert representative `ENOENT`, `EACCES`, `EROFS`, `ENOTEMPTY`, and `EIO`
   mappings produce the same HTTP status and XML response.

### P2 — finish CLI behavioral parity

**Status: PARTIAL / UNVERIFIED.**

The Rust CLI covers the core upstream flags, transport probing, stale cleanup,
watching, Ctrl-C teardown, read-only mode, and mountpoint environment fallback
([parser](../crates/mount-rs-cli/src/parser.rs#L81-L94),
[options](../crates/mount-rs-cli/src/parser.rs#L183-L255),
[runtime](../crates/mount-rs-cli/src/runtime.rs#L183-L294)). It also adds the
Rust-specific driver/database choices. The remaining observed differences are
the upstream demo hints/README behavior and FUSE session counters: upstream
prints conditional `head`/write/read hints (`upstream: src/cli/index.ts:161-218`),
whereas Rust reports FUSE counters unavailable ([runtime](../crates/mount-rs-cli/src/runtime.rs#L279-L294)).

**Acceptance tests:**

1. Black-box test `--help`, `--probe`, positional and `--mountpoint` forms,
   `MOUNTX_MOUNTPOINT`/`MOUNT_RS_MOUNTPOINT` precedence, `--empty`,
   `--read-only`, `--allow-other`, and invalid driver combinations.
2. Start a memory mount and compare the non-empty and read-only hint sets with
   upstream; verify README seeding and external read/write examples.
3. Kill/interrupt a mounted process and assert the scoped stale cleanup and
   final unmount output; either expose FUSE counters or record an explicit,
   tested parity exception.

## Implemented in the current tree, but unverified here

These are not outstanding gaps for this checkpoint. They are recorded so later
work does not duplicate them or mistake uncommitted work for a landed API.

- **Root utilities:** path normalization/resolution, errno/error helpers,
  special-mode helpers, and generic `PathLock` are declared in
  [`types/root.d.ts`](../integrations/mount-rs-napi/types/root.d.ts#L1-L99) and
  attached by [`postbuild.mjs`](../integrations/mount-rs-napi/postbuild.mjs#L6-L21).
  The missing piece is the general `FsDriver`/loopback boundary above.
- **Unstorage:** the callback bridge, metadata overlay, read-only checks,
  flush/close, callback release, and error propagation are implemented in
  [`kv_binding.rs`](../integrations/mount-rs-napi/src/kv_binding.rs#L626-L651),
  with focused coverage in [`unstorage.mjs`](../integrations/mount-rs-napi/test/unstorage.mjs#L41-L100).
- **Fixed-size/extensible chunked storage:** the N-API factory validates
  provider kinds, fixed chunk size, ownership/lease values, identity, and
  shutdown ([implementation](../integrations/mount-rs-napi/src/lib.rs#L650-L676),
  [factory](../integrations/mount-rs-napi/src/lib.rs#L2080-L2115)). The current
  integration test covers mixed providers, SQLite reopen durability, lease
  exclusivity, shutdown behavior, and validation ([test](../integrations/mount-rs-napi/test/chunked.mjs#L49-L139)).
- **Native server wrappers:** NFS, P9, S3, and WebDAV construction, binding,
  addresses, connection counts, and close paths exist in the new
  [`servers.rs`](../integrations/mount-rs-napi/src/servers.rs#L204-L259) and
  its later transport sections. Their upstream parity omissions are listed
  above rather than counted as total absence.
- **Rust 9P/NFS low-level crates:** public protocol/session/server barrels are
  present in [9P](../transports/mount-rs-9p/src/lib.rs#L18-L38) and
  [NFS](../transports/mount-rs-nfs/src/lib.rs#L10-L36). This ledger therefore
  does not propose another broad 9P/NFS Rust re-port; the remaining P0 work is
  the Node subpath/driver boundary and the P1 server contract.

## Working-tree provenance and freeze boundary

At the audit snapshot, the relevant uncommitted/new parity work included
`integrations/mount-rs-napi/src/kv_binding.rs`, `src/servers.rs`,
`src/utilities.rs`, `postlude-utilities.cjs`, `types/root.d.ts`, the N-API
declaration/facade changes, and the focused server/type/utility/Unstorage tests.
Other dirty paths, including storage/NFS tests, CI, lockfiles, README, scripts,
and benchmarks, were observed but not edited or assigned by this ledger.

This document is the only file changed for the audit. Freeze this owned file
until the main owner commits it; no commit or push is part of this task.
