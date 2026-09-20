# W01.2 — skipped-behavior classification

Status: classification packet only. This document does not turn a skipped test
into a pass, change a capability declaration, or close W01.

The compatibility oracle is `pithings/mountx` at
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`. The local runner imports the
unmodified upstream `test/conformance.ts` for its memory, host, chunked, NFS,
9P, and Unstorage columns: [`conformance.test.mjs`](../../tests/upstream/conformance.test.mjs#L15),
[`nfs-conformance.test.mjs`](../../tests/upstream/nfs-conformance.test.mjs#L6),
[`p9-conformance.test.mjs`](../../tests/upstream/p9-conformance.test.mjs#L13),
and [`unstorage-conformance.test.mjs`](../../tests/upstream/unstorage-conformance.test.mjs#L21).
The corresponding pinned upstream sources are [`test/conformance.ts`](https://github.com/pithings/mountx/blob/85361a8212ff9bff8e69f62fa8993ef2c2ec51e8/test/conformance.ts),
[`test/nfs/v3/conformance.test.ts`](https://github.com/pithings/mountx/blob/85361a8212ff9bff8e69f62fa8993ef2c2ec51e8/test/nfs/v3/conformance.test.ts),
[`test/nfs/v4/conformance.test.ts`](https://github.com/pithings/mountx/blob/85361a8212ff9bff8e69f62fa8993ef2c2ec51e8/test/nfs/v4/conformance.test.ts),
and [`src/drivers/unstorage.ts`](https://github.com/pithings/mountx/blob/85361a8212ff9bff8e69f62fa8993ef2c2ec51e8/src/drivers/unstorage.ts).

## Runner baseline

The four-file runner has two meaningful local stages. `PGLITE_DATABASE_URL` adds
the three chunked PGlite combinations in [`conformance.test.mjs`](../../tests/upstream/conformance.test.mjs#L58).

```sh
# mountx_source must be a checkout whose HEAD is 85361a8212ff9bff8e69f62fa8993ef2c2ec51e8.
MOUNTX_SOURCE="$mountx_source" pnpm --dir tests/upstream test
# Observed without PGlite: 4 files passed; 984 passed, 85 skipped (1,069).

# The isolated server gate supplies PGLITE_DATABASE_URL before rerunning this stage.
MOUNTX_SOURCE="$mountx_source" ./scripts/test-pglite.sh
# Observed with PGlite: 4 files passed; 1,194 passed, 88 skipped (1,282).
```

The second result matches the W01 tracker record ([`WORK_TRACKER.md`](../../WORK_TRACKER.md#L187));
the extra 210 passes and three skips are the PGlite chunked columns, not
previous skips being promoted to passes. The observed run was local macOS
arm64, uid 501, against the current shared worktree and the pinned oracle; it
is not revision-matched hosted acceptance.

## Upstream skip inventory

The verbose full run reports exactly these eight tags. Counts are skipped test
rows, not capability claims; a test with a missing capability has no behavior
result.

### `needs handles` — 12 rows

- Exact reason: every TypeScript and Rust NFSv3/NFSv4.1 conformance target
  declares `handles: false` in [`nfs-conformance.test.mjs`](../../tests/upstream/nfs-conformance.test.mjs#L24).
  NFSv3 has no open operation on the wire. NFSv4.1 has stateids, but the
  current path-keyed handle table loses the name after `REMOVE`; it does not
  advertise preserve-unlinked semantics. The upstream NFS columns describe
  this as a protocol/server limitation, while a real kernel client may hide it
  with silly-rename.
- Classification: missing implementation for a full NFS handle claim, not an
  environment prerequisite. The current false capability is honest bounded
  behavior, not a pass.
- Required for mount-rs: yes for the core MemoryFs/ChunkedFs contract and for
  full NFS parity; not required to keep the current NFS transport capability
  declaration false. W01.4 still cannot claim NFS handle parity while these
  rows remain skipped.
- Simplest next task: add or unskip one NFS `open → unlink → read` regression
  through the existing [`nfs-conformance.test.mjs`](../../tests/upstream/nfs-conformance.test.mjs)
  path, then implement stable orphan identity in the NFS handle/session layer
  ([`handles.rs`](../../transports/mount-rs-nfs/src/handles.rs),
  [`session.rs`](../../transports/mount-rs-nfs/src/session.rs)) before changing
  `handles` to true.
- Evidence boundary: core and 9P handle passes do not qualify NFS. The
  userspace TCP NFS runner is not a kernel-mounted NFS result; use the ignored
  native tests in [`native_mount.rs`](../../transports/mount-rs-nfs/tests/native_mount.rs#L385)
  for that separate boundary.

### `needs hardlinks` — 6 rows

- Exact reason: both Unstorage targets lack `link`/inode-alias semantics, so
  the upstream capability resolver reports `hardlinks: false`.
- Classification: missing capability in the generic Unstorage adapter, not an
  environment prerequisite. It is not a missing core capability: MemoryFs and
  ChunkedFs advertise hardlinks ([`memory.rs`](../../src/memory.rs#L765),
  [`chunked/src/lib.rs`](../../integrations/mount-rs-chunked/src/lib.rs#L998)).
- Required for mount-rs: yes for a full filesystem driver; conditional for the
  deliberately capability-limited Unstorage adapter. The latter must keep the
  false declaration until it has real alias and `nlink` semantics.
- Simplest next task: add the three upstream hardlink cases to the focused
  Unstorage test ([`test/unstorage.mjs`](../../integrations/mount-rs-napi/test/unstorage.mjs))
  as a failing contract, then implement alias metadata in the KV bridge or
  explicitly retain `hardlinks: false`.
- Evidence boundary: this column uses an in-memory Unstorage memory driver;
  it says nothing about remote-store durability or atomic rename.

### `needs symlinks` — 16 rows

- Exact reason: both Unstorage targets do not expose symlink target storage,
  `readlink`, or distinct `lstat` resolution. The eight symlink cases therefore
  have no meaningful target to exercise.
- Classification: missing adapter behavior, not an environment prerequisite.
- Required for mount-rs: yes for the full core contract; conditional for the
  current capability-limited Unstorage target.
- Simplest next task: add the upstream dangling-link, loop, relative-target,
  `lstat`/`stat`, and `readlink` cases to
  [`test/unstorage.mjs`](../../integrations/mount-rs-napi/test/unstorage.mjs),
  then implement the node-kind/target representation and resolver in the KV
  bridge. Do not only flip `symlinks` to true.
- Evidence boundary: the core Rust symlink implementation and its NAPI
  wrapper do not establish Unstorage parity.

### `needs statfs` — 2 rows

- Exact reason: generic Unstorage has no filesystem-capacity or block-geometry
  contract, so both the TypeScript and Rust Unstorage targets leave `statfs`
  unavailable.
- Classification: missing backend capability/contract, not an environment
  prerequisite.
- Required for mount-rs: yes for full filesystem-driver parity; conditional
  for a generic KV-backed adapter that explicitly reports `statfs: false`.
- Simplest next task: define an optional capacity/statistics callback and test
  it in [`test/unstorage.mjs`](../../integrations/mount-rs-napi/test/unstorage.mjs),
  or add a focused assertion that the adapter remains explicitly unsupported;
  do not synthesize capacity from key count.
- Evidence boundary: `statfs` passes in MemoryFs/ChunkedFs do not qualify an
  Unstorage provider.

### `needs mountx.mknod` — 16 rows

- Exact reason: the TypeScript and Rust rooted host targets, plus both
  Unstorage targets, do not advertise the optional `mountx.mknod` extension.
  Rust `HostFs` records this directly as `mknod: false` in
  [`mount-rs-host/src/lib.rs`](../../integrations/mount-rs-host/src/lib.rs#L1140);
  the NAPI capability surface forwards that declaration in
  [`integrations/mount-rs-napi/src/lib.rs`](../../integrations/mount-rs-napi/src/lib.rs#L340).
- Classification: an explicit capability boundary, or missing implementation
  if those targets are intended to be full-capability filesystems; it is not a
  missing privilege in the current rootless runner. The pinned upstream
  `node-fs` target also has no `mountx.mknod` extension.
- Required for mount-rs: required and already present for MemoryFs and
  ChunkedFs ([`memory.rs`](../../src/memory.rs#L765),
  [`chunked/src/lib.rs`](../../integrations/mount-rs-chunked/src/lib.rs#L998));
  not required for the current HostFs/Unstorage capability-limited targets
  unless mount-rs promises special-node parity there.
- Simplest next task: keep the false capability and add an explicit HostFs /
  Unstorage unsupported assertion, or implement and test `mknod` (including
  FIFO/socket/device metadata) before advertising it. The existing host parity
  runner is [`check-host-parity.mjs`](../../scripts/check-host-parity.mjs).
- Evidence boundary: a stored special-node `stat` result is not device access;
  native mount and OS privilege behavior remain separate.

### `needs mknod.anyType` — 16 rows

This tag has two different causes and must not be treated as one implementation
gap:

- **NFSv3/NFSv4.1 — 8 rows:** those targets declare `extensions: ["mknod"]`
  but `carries: []` in [`nfs-conformance.test.mjs`](../../tests/upstream/nfs-conformance.test.mjs#L74).
  The wire can carry FIFO, socket, and device kinds, but cannot carry every
  type encoded in the `mode` argument. This is a protocol-carriage boundary,
  not an environment prerequisite and not a reason to invent `EPERM` in the
  client. The wrapper's explicit refusal case is evidence of that boundary,
  not a pass of the skipped generic cases.
- **HostFs/Unstorage — 8 rows:** these targets do not have `mountx.mknod` at
  all, so the mode-typed half cannot be asked. This is the same capability
  boundary as the preceding section, not a root failure.
- Required for mount-rs: yes for the full core MemoryFs/ChunkedFs contract;
  no for the current NFS wire claim and capability-limited HostFs/Unstorage
  targets. A future full NFS claim would require a protocol/client design,
  not just a test flag.
- Simplest next task: retain the existing `carries: []` NFS declaration and
  add a focused cross-column report; for HostFs/Unstorage, choose explicitly
  between implementing the extension and preserving the false capability.
- Evidence boundary: passing `mknod.anyType` in a memory or chunked column
  does not qualify NFS, HostFs, or Unstorage.

### `needs times + symlinks` — 2 rows

- Exact reason: both Unstorage targets have ordinary timestamp support but no
  symlink object on which `lutimes` can operate.
- Classification: missing symlink representation, not an environment
  prerequisite and not a timestamp failure.
- Required for mount-rs: yes for a full driver; conditional for current
  Unstorage capability scope.
- Simplest next task: implement the symlink behavior described above, then run
  the existing upstream `lutimes` case before changing capabilities.
- Evidence boundary: ordinary `utimes` passes do not establish link-specific
  timestamp behavior.

### `needs permissions + symlinks + root` — 18 rows

- Exact reason: the pinned suite deliberately gates the interesting `lchown`
  test on `process.getuid() === 0`; the current runner is uid 501. Without
  root, changing ownership to uid/gid 65534 would either fail for the wrong
  reason or collapse into setting the existing owner. Unstorage also lacks
  symlinks, so its two rows have both a capability and root prerequisite.
- Classification: root is an environment prerequisite. The additional
  Unstorage symlink condition is a capability gap. Neither should be reported
  as an implementation pass.
- Required for mount-rs: yes for the `lchown` symlink-following contract on
  targets that claim permissions and symlinks.
- Simplest next task: run the unchanged upstream suite in a controlled Linux
  uid-0 lane. Only if that run fails should a Rust/NAPI implementation task be
  opened; do not weaken the test to the current owner.
- Evidence boundary: current non-root macOS results do not verify link-versus-
  target ownership. Ordinary `chown`, `chmod`, and timestamp passes are not
  substitutes.

## Opt-in and prerequisite gates outside the 88 rows

These gates are not emitted as the eight upstream capability tags, but they
change which W01 evidence exists.

| Gate | Exact reason when absent | Required? | Simplest next task | Evidence boundary |
| --- | --- | --- | --- | --- |
| `MOUNTX_SOURCE` | [`scripts/test-all.sh`](../../scripts/test-all.sh#L5) exits if the oracle path is absent; auxiliary NAPI differentials log explicit skips in [`differential.mjs`](../../integrations/mount-rs-napi/test/differential.mjs#L6), [`js-driver.mjs`](../../integrations/mount-rs-napi/test/js-driver.mjs#L9), [`memory-options.mjs`](../../integrations/mount-rs-napi/test/memory-options.mjs#L8), [`nfs-codec.mjs`](../../integrations/mount-rs-napi/test/nfs-codec.mjs#L5), and [`9p-codec.mjs`](../../integrations/mount-rs-napi/test/9p-codec.mjs#L132). | Yes for W01.1–W01.3 oracle evidence. | Check `git -C "$mountx_source" rev-parse HEAD` equals the pinned SHA, install the oracle and runner lockfiles, then rerun the exact commands above. | No oracle source means no parity result; local Rust tests are not a substitute. |
| `PGLITE_DATABASE_URL` | The upstream wrapper registers no PGlite chunked columns without it; the NAPI factory and chunked tests report explicit skips in [`factories.mjs`](../../integrations/mount-rs-napi/test/factories.mjs#L150) and [`chunked.mjs`](../../integrations/mount-rs-napi/test/chunked.mjs#L141). | Yes for the PGlite/P0 backend and the 1,194-test stage; no for the mount-free memory baseline. | Use [`scripts/test-pglite.sh`](../../scripts/test-pglite.sh#L1), which owns an isolated server and reruns the upstream stage. | Local PGlite is not live R2, remote durability, or native-mount evidence. |
| R2 credentials (`R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`) | [`test-all.sh`](../../scripts/test-all.sh#L54) prints a live-R2 skip and the NAPI factory does the same at [`factories.mjs`](../../integrations/mount-rs-napi/test/factories.mjs#L156). | Yes for W05/live Cloudflare acceptance; no for Tier-0 memory conformance. | Run [`scripts/test-acceptance.sh`](../../scripts/test-acceptance.sh#L1) with dedicated credentials kept outside the repository; add `MOUNT_RS_RUN_CLOUDFLARE_R2_CLI=1` only for the CLI lane. | Local object-store and PGlite results are not Cloudflare R2 evidence. |
| Seeded trace backend opt-ins | [`check-trace-parity.mjs`](../../scripts/check-trace-parity.mjs#L28) runs local backends by default, adds PGlite only with its URL, and adds R2 only with `MOUNT_RS_TRACE_R2=1`. | Yes for W01.3's required backend matrix. | Run the default five seeds, then explicitly run PGlite and R2 lanes with `MOUNT_RS_TRACE_SEEDS=4182,1,42,65535,4294967295`; retain backend, seed, operation index, and oracle revision. | A passing seed/backend pair does not cover another backend or a native transport. |
| Native mount/transport opt-ins | The NAPI mount test skips until `MOUNT_RS_NAPI_NATIVE_MOUNT=1` ([`native.mjs`](../../integrations/mount-rs-napi/test/native.mjs#L52)); ignored Rust tests additionally require Linux/macOS clients, kernel modules, `/dev/fuse`, privileges, or host consent. | Yes for W01.4 platform/native acceptance; no for Tier-0 userspace conformance. | Use the exact CI lanes in [`ci.yml`](../../.github/workflows/ci.yml#L52): FUSE (`MOUNT_RS_RUN_NATIVE_FUSE=1`), NFS (`MOUNT_RS_NFS_NATIVE_TEST=1` and `_V4_TEST=1`), 9P (`MOUNT_RS_9P_NATIVE_TEST=1`), WebDAV (`MOUNT_RS_WEBDAV_NATIVE_TEST=1`), and the NAPI mount opt-in. | An ignored test is not a pass. Userspace TCP NFS/9P tests, protocol fixtures, and transport probes do not prove a kernel-mounted result. |

Missing host prerequisites belong in the environment column. If an explicitly
enabled lane starts and fails, that is implementation evidence and must be
recorded as a failure rather than converted back into a skip.

## W01.1–W01.4 simple-to-complex sequence

1. **W01.1 — mount-free API/behavior ledger.** Start with the core memory
   driver, NAPI memory driver, and local ChunkedFs combinations. Reconcile the
   capability and export gaps in [`docs/public-api-parity.md`](../../docs/public-api-parity.md#L1)
   using the pinned-oracle commands in [`scripts/test-all.sh`](../../scripts/test-all.sh#L23).
   Do not use a provider or native result to close a simpler core gap.
2. **W01.2 — this classification and required coverage.** For each row above,
   either run the missing environment lane, keep an explicit capability/wire
   boundary with a focused assertion, or implement the required behavior and
   rerun the existing upstream case. The full upstream command must retain its
   skip count until the corresponding rows actually execute.
3. **W01.3 — seeded cross-engine traces.** Run
   [`check-trace-parity.mjs`](../../scripts/check-trace-parity.mjs#L1) from
   memory → SQLite/object-store → PGlite → live R2, using the five fixed seeds
   and the pinned oracle. Preserve the machine-readable failure context; a
   component conformance pass cannot replace a missing backend trace.
4. **W01.4 — transports and supported platforms.** Move from userspace
   NFS/9P/FUSE/WebDAV protocol tests to the explicitly enabled macOS/Linux
   native lanes in [`ci.yml`](../../.github/workflows/ci.yml#L163). Verify the
   remaining errors, paths, bytes, links, timestamps, handles, concurrency,
   cleanup, and lifecycle at each transport boundary. Native and hosted
   results remain separate from the local runner.

W01.2 is complete only when every skipped row has either (a) executed with the
required prerequisite and produced a real result, or (b) an explicit,
reviewed capability/protocol boundary backed by a focused test. The current
`1,194 passed | 88 skipped` result remains classified evidence, not completion.
