# mount-rs-fuse parity slice

This package contains a bounded Rust FUSE wire-codec and init-negotiation
slice compared with the pinned mountx oracle source at
[`pithings/mountx/src/fuse`](https://github.com/pithings/mountx/tree/85361a8212ff9bff8e69f62fa8993ef2c2ec51e8/src/fuse),
revision `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.

The compared mountx source is [MIT-licensed](https://github.com/pithings/mountx/blob/85361a8212ff9bff8e69f62fa8993ef2c2ec51e8/LICENSE);
this crate remains licensed under Apache-2.0. The checked-in oracle body
corpus is regenerated with `tests/generate_protocol_fixtures.mjs` by setting
`MOUNTX_SOURCE` to that exact revision and running Node with TypeScript
stripping enabled:

```sh
MOUNTX_SOURCE=/path/to/pinned-mountx \
  node --experimental-strip-types \
  transports/mount-rs-fuse/tests/generate_protocol_fixtures.mjs
```

The codec is deliberately separate from `FuseSession`. A decoded or encoded
operation is evidence that its wire shape is represented; it is not a claim
that the native Linux session dispatches that operation or that the package
provides complete mountx API parity.

## Implemented public wire surface

`constants.rs` exposes the protocol 7.41 opcode, notification, flag, POSIX
type, Linux-open, lock, xattr, compatibility-size, and opcode-name constants.
`init.rs` exposes joined/split init flags, version-dependent negotiation,
max-pages/max-write clamping, and conversion to and from the codec's
`FuseInitOut` shape.

`protocol.rs` provides bounded little-endian codecs for request and reply
headers, framing and 8-byte extension blocks, errno replies, versioned
attributes/entries/statfs/init/open/read/write/setxattr layouts, names and
xattr lists, locks, poll/bmap/lseek/fallocate/access/interrupt bodies, and
size-limited `READDIR`/`READDIRPLUS` packing and unpacking.

The typed codec table covers these operations:

`LOOKUP`, `FORGET`, `GETATTR`, `SETATTR`, `READLINK`, `SYMLINK`, `MKNOD`,
`MKDIR`, `UNLINK`, `RMDIR`, `RENAME`, `LINK`, `OPEN`, `READ`, `WRITE`,
`STATFS`, `RELEASE`, `FSYNC`, `SETXATTR`, `GETXATTR`, `LISTXATTR`,
`REMOVEXATTR`, `FLUSH`, `INIT`, `OPENDIR`, `READDIR`, `RELEASEDIR`,
`FSYNCDIR`, `GETLK`, `SETLK`, `SETLKW`, `ACCESS`, `CREATE`, `INTERRUPT`,
`BMAP`, `DESTROY`, `POLL`, `BATCH_FORGET`, `FALLOCATE`, `READDIRPLUS`,
`RENAME2`, and `LSEEK`.

The focused tests in `tests/protocol.rs` retain upstream byte fixtures for
`INIT`, `LOOKUP`, `READDIRPLUS`, and `WRITE`, plus the generated
`tests/fixtures/protocol-oracle.txt` body corpus for old/new attributes,
extended xattrs, locks, rename/create, old/new statfs, poll, fallocate, lseek,
batch-forget, and the remaining typed request/reply dispatch entries. The
corpus currently contains 83 deterministic valid body fixtures. Compatibility-
layout and decoder-totality regressions remain in the same test module.

## Exact remaining wire operations

The following named upstream opcodes intentionally have no typed body codec
in this slice and are returned as unsupported by the dispatch helpers:

`IOCTL`, `NOTIFY_REPLY`, `COPY_FILE_RANGE`, `SETUPMAPPING`, `REMOVEMAPPING`,
`SYNCFS`, `TMPFILE`, `STATX`, and `CUSE_INIT`.

Unknown numeric opcodes can still be framed as raw payloads, but they are not
decoded into a typed body and are not treated as supported operations.

## Exact native-session boundary

The existing `FuseSession` native path remains intact. Its current dispatch
handles `INIT` (modern 7.12+ layout), `MKNOD`, `FLUSH`, `FSYNCDIR`, `SETATTR`,
`DESTROY`, `OPENDIR`, `READDIR`, `READDIRPLUS`, `RELEASEDIR`, `STATFS`,
`FSYNC`, `CREATE`, `READLINK`, `SYMLINK`, `MKDIR`, `UNLINK`, `RMDIR`,
`RENAME`, `LINK`, `LOOKUP`, `GETATTR`, `OPEN`, `READ`, `WRITE`, `ACCESS`, and
`RELEASE`. `ACCESS` evaluates the request uid/gid against driver metadata;
supplementary groups remain an explicit boundary because this session does not
negotiate or receive them.
`FORGET` and `BATCH_FORGET` remain bookkeeping paths, while
`NOTIFY_REPLY` is ignored without a reply.

Plain `RENAME2` requests (`flags = 0`) use the same rename implementation.
Flagged exchange/whiteout variants are rejected with `ENOSYS` without mutating
the namespace because the core driver does not expose those semantics.

The native session still returns `ENOSYS` for the other codec-covered
operations, including `SETXATTR`, `GETXATTR`, `LISTXATTR`, `REMOVEXATTR`,
`GETLK`, `SETLK`, `SETLKW`, `INTERRUPT`, `BMAP`, `POLL`,
`FALLOCATE`, `LSEEK`, and `COPY_FILE_RANGE`. Native device/mount behavior and the
N-API `./fuse` boundary are outside this scoped slice.
