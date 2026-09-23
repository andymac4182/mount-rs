# `@mount-rs/virtual-fs`

Mount-independent adapters for the pinned `just-bash` and Mastra workspace
filesystem contracts. The adapters operate directly on one public
`@mount-rs/core` `Filesystem` instance: they do not mount a host path,
copy a drive into a second in-memory filesystem, or expose a public service.

The package is deliberately separate from the N-API package. It currently
contains only the in-process adapters. The exported `MountRsBackend` structural
boundary is the async operation surface consumed by the adapters, so a future
authenticated remote logical-drive client can target the same boundary without
changing the consumer adapters. No HTTP transport or distributed cache is
implemented here.

## Versions and installation

The runtime tests use the pinned versions `just-bash@3.4.2` and
`@mastra/core@1.67.0`. Both are optional peers because an application using one
adapter does not need to install the other. `@mount-rs/core` is the
required peer. Use the `./just-bash` or `./mastra` subpath when installing only
one consumer; the convenience root entrypoint re-exports both adapters and
therefore expects both consumer peers to be present.

## just-bash

```js
import { Filesystem } from "@mount-rs/core";
import { createBash } from "@mount-rs/virtual-fs/just-bash";

const filesystem = Filesystem.memory();
const bash = await createBash(filesystem, { cwd: "/" });

const result = await bash.exec(
  "mkdir -p /work && printf 'hello\\n' > /work/message.txt && cat /work/message.txt",
);
console.log(result.stdout);

await bash.fs.close();
await filesystem.shutdown();
```

`createJustBashFilesystem()` returns the adapter when the application already
owns a `Bash` instance. It implements the actual `IFileSystem` methods,
including binary `readFileBuffer`, byte-preserving `readFileBytes`, symlink
operations, metadata, recursive copy/remove, and synchronous `getAllPaths()`.
The glob index stores paths only. Each `getAllPaths()` call returns a sorted
snapshot array: an earlier array does not change when the adapter or backend
changes. Adapter mutations update later snapshots, while mutations made
directly through the underlying filesystem require `refreshPaths()` before
shell glob expansion observes them.

## Mastra

```js
import { Filesystem } from "@mount-rs/core";
import { Workspace } from "@mastra/core/workspace";
import { createMastraFilesystem } from "@mount-rs/virtual-fs/mastra";

const filesystem = Filesystem.memory();
const provider = createMastraFilesystem(filesystem, {
  id: "logical-drive",
  name: "Logical drive",
});
const workspace = new Workspace({ filesystem: provider });
await workspace.init();

await workspace.filesystem.mkdir("/docs");
await workspace.filesystem.writeFile("/docs/data.bin", Uint8Array.of(0, 1, 255));
const data = await workspace.filesystem.readFile("/docs/data.bin");

await workspace.destroy();
```

Mastra errors are translated to its official error classes for missing files
and directories, conflicts, wrong file kinds, non-empty directories,
permissions, read-only writes, and stale mtime writes. `readFile()` returns a
`Buffer` unless an encoding is requested. The adapter also exposes `lstat`,
`symlink`, and `readlink` so directory listings retain symlink metadata.

## Lifecycle, policy, and paths

- Paths are normalized POSIX absolute paths inside the supplied logical drive;
  no host path is accepted or resolved.
- `writeFile(..., { overwrite: false })` creates files exclusively.
  No-overwrite regular-file moves first create an exclusive hardlink on the
  same backend. When hardlinks are unavailable or cross-device, they create an
  exclusive destination and copy bytes. The copy path checks the destination
  identity and source mode before removing the source; it returns `ENOTSUP` if
  the mode cannot be preserved. A copy failure may leave an incomplete
  destination because path-based cleanup could remove a foreign replacement.
  A source-removal failure leaves the completed destination available. Symlink
  moves use an exclusive hardlink to retain the source link's identity and
  return `ENOTSUP` when hardlinks are unavailable. No-overwrite directory moves return
  `ENOTSUP` because the public backend has no atomic no-replace directory move.
  These file and symlink moves have intermediate states visible to other backend
  users. The adapter checks source identity and file kind immediately before
  removal, but a replacement after that check can still race with path-based
  unlink. Destination replacement after its identity check can race with source
  removal as well. Strict no-clobber moves across uncoordinated backend users
  need an atomic no-replace move or conditional backend deletion.
  Moves with overwrite enabled use the backend rename operation.
- `deleteFile()` and `rmdir()` check entry kind and identity within their
  operation lease before removal. A replacement after the check can still race
  with path-based unlink or rmdir; conditional backend deletion is needed to
  close that window.
- `expectedMtime` checks the visible mtime before writing. The public backend
  has no compare-and-write operation, so a concurrent change after that check
  can race with the write.
- `readOnly: true` rejects writes while leaving reads available. If omitted,
  the adapter follows the backend capability when it is present.
- `close()` waits for operations already admitted by the adapter, but has no
  timeout and cannot interrupt a backend promise that never settles. A
  cancellation/timeout contract must come from the backend or a future remote
  transport. Set `closeOnDestroy: true` to call the backend's public
  `shutdown()` after the adapter is idle; otherwise the caller retains backend
  ownership.
- `Bash.exec()` cancellation remains the just-bash cooperative command-boundary
  contract. The current public NodeRustFs methods do not accept an
  `AbortSignal`, so cancellation of an in-flight backend operation is not
  promised by this adapter.

## Verification

From this directory:

```sh
node node_modules/typescript/lib/tsc.js --noEmit
node test/just-bash.mjs
node test/mastra.mjs
```

The optional live consumer lane uses the real pinned adapters over PGlite
metadata and RustFS S3 blocks. It is explicitly opt-in and uses the portable
Node argument form; the environment variable remains available for harnesses:

```sh
pnpm test:live-rustfs
# equivalent direct invocation:
node test/live-pglite-rustfs.mjs --live
# legacy harness opt-in:
MOUNT_RS_VIRTUAL_FS_LIVE=1 node test/live-pglite-rustfs.mjs
```

From the repository root, run it through the isolated RustFS combo contract:

```sh
RUSTFS_COMBO_NAME=virtual-fs-pglite-rustfs RUSTFS_COMBO_TIMEOUT_SECONDS=900 RUSTFS_COMBO_COMMAND='pnpm --dir bindings/mount-rs-virtual-fs test:live-rustfs' ./scripts/test-rustfs.sh
```

The harness supplies a fresh PGlite server, loopback RustFS endpoint, test
bucket, credentials, and test-owned prefix, then removes the owned service and
fixtures. This proves the RustFS-backed consumer lane only; it is not evidence
against Cloudflare R2 or an HTTP remote service.

The two runtime tests execute the real pinned Bash interpreter and the real
Mastra `Workspace` against native memory, SQLite, and rooted native NodeFs
drivers. They cover text and binary I/O, directories, rename/copy/delete,
metadata, hard and symbolic links, path normalization/glob refresh, typed
errors, read-only policy, SQLite reopen persistence, shared host-file
visibility, copy safety, short writes, and lifecycle cleanup.
