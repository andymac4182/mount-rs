#!/usr/bin/env node
/**
 * Generate the checked-in body fixtures consumed by tests/protocol.rs.
 *
 * The values below are deliberately small, deterministic examples of the
 * public mountx protocol families that are not covered by golden.test.ts.
 * This script obtains the expected bytes from the pinned TypeScript codecs;
 * the Rust tests then compare their own encoders and decoders with those
 * bytes in both directions.
 *
 * Run with the exact pinned checkout, for example:
 *
 *   MOUNTX_SOURCE=/path/to/mountx-source \
 *     node --experimental-strip-types \
 *     transports/mount-rs-fuse/tests/generate_protocol_fixtures.mjs
 */

import { execFileSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const EXPECTED_REVISION = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
const source = process.env.MOUNTX_SOURCE;
if (!source) {
  throw new Error("MOUNTX_SOURCE must point to the pinned mountx checkout");
}

const revision = execFileSync("git", ["-C", source, "rev-parse", "HEAD"], {
  encoding: "utf8",
}).trim();
if (revision !== EXPECTED_REVISION) {
  throw new Error(`MOUNTX_SOURCE is ${revision}, expected ${EXPECTED_REVISION}`);
}

const protocol = await import(pathToFileURL(join(source, "src/fuse/protocol.ts")));
const constants = await import(pathToFileURL(join(source, "src/fuse/constants.ts")));

const { encodeReplyBody, encodeRequestBody } = protocol;
const {
  FUSE_ACCESS,
  FUSE_BATCH_FORGET,
  FUSE_BMAP,
  FUSE_CREATE,
  FUSE_DESTROY,
  FUSE_FALLOCATE,
  FUSE_FLUSH,
  FUSE_FORGET,
  FUSE_FSYNC,
  FUSE_FSYNCDIR,
  FUSE_GETATTR,
  FUSE_GETLK,
  FUSE_GETXATTR,
  FUSE_INIT,
  FUSE_INTERRUPT,
  FUSE_LINK,
  FUSE_LISTXATTR,
  FUSE_LSEEK,
  FUSE_MKDIR,
  FUSE_MKNOD,
  FUSE_OPEN,
  FUSE_OPENDIR,
  FUSE_POLL,
  FUSE_READ,
  FUSE_READDIR,
  FUSE_READDIRPLUS,
  FUSE_READLINK,
  FUSE_RENAME,
  FUSE_RENAME2,
  FUSE_RELEASE,
  FUSE_RELEASEDIR,
  FUSE_REMOVEXATTR,
  FUSE_RMDIR,
  FUSE_SETLK,
  FUSE_SETLKW,
  FUSE_SETATTR,
  FUSE_SETXATTR,
  FUSE_STATFS,
  FUSE_SYMLINK,
  FUSE_UNLINK,
} = constants;

const context = (minor, setxattrExt = false) => ({ minor, setxattrExt });

const attr = (overrides = {}) => ({
  ino: 0x1122334455667788n,
  size: 0x0102030405060708n,
  blocks: 0x1112131415161718n,
  atime: 0x2122232425262728n,
  mtime: 0x3132333435363738n,
  ctime: 0x4142434445464748n,
  atimensec: 101,
  mtimensec: 202,
  ctimensec: 303,
  mode: 0o100640,
  nlink: 3,
  uid: 1001,
  gid: 1002,
  rdev: 0x1234,
  blksize: 4096,
  flags: 0xa5a5a5a5,
  ...overrides,
});

const entry = {
  nodeid: 0x1020304050607080n,
  generation: 0x0102030405060708n,
  entryValid: 11n,
  attrValid: 13n,
  entryValidNsec: 17,
  attrValidNsec: 19,
  attr: attr(),
};

const open = { fh: 0x8877665544332211n, openFlags: 0x42, backingId: -7 };
const lock = { start: 0x0102030405060708n, end: 0x1112131415161718n, type: 1, pid: 4242 };
const lk = { fh: 0x2122232425262728n, owner: 0x3132333435363738n, lk: lock, lkFlags: 1 };
const read = {
  fh: 0x0102030405060708n,
  offset: 4096n,
  size: 7,
  readFlags: 2,
  lockOwner: 0x1112131415161718n,
  flags: 0x241,
};
const dirents = {
  entries: [{ ino: 3n, off: 4n, type: 8, name: "child" }],
};
const direntsPlus = {
  entries: [{ entry, dirent: { ino: 3n, off: 4n, type: 8, name: "child" } }],
};

const fixtures = [
  {
    name: "forget-request",
    direction: "request",
    opcode: FUSE_FORGET,
    ctx: context(41),
    value: { nlookup: 9n },
  },
  {
    name: "getattr-request",
    direction: "request",
    opcode: FUSE_GETATTR,
    ctx: context(41),
    value: { getattrFlags: 1, fh: 0x0102030405060708n },
  },
  {
    name: "setattr-request",
    direction: "request",
    opcode: FUSE_SETATTR,
    ctx: context(41),
    value: {
      valid: 0x7ff,
      fh: 0x1112131415161718n,
      size: 0x2122232425262728n,
      lockOwner: 0x3132333435363738n,
      atime: 0x4142434445464748n,
      mtime: 0x5152535455565758n,
      ctime: 0x6162636465666768n,
      atimensec: 101,
      mtimensec: 202,
      ctimensec: 303,
      mode: 0o100640,
      uid: 1001,
      gid: 1002,
    },
  },
  {
    name: "readlink-request",
    direction: "request",
    opcode: FUSE_READLINK,
    ctx: context(41),
    value: {},
  },
  {
    name: "readlink-reply",
    direction: "reply",
    opcode: FUSE_READLINK,
    ctx: context(41),
    value: { target: "target/file" },
  },
  {
    name: "symlink-request",
    direction: "request",
    opcode: FUSE_SYMLINK,
    ctx: context(41),
    value: { name: "link", target: "target/file" },
  },
  {
    name: "symlink-reply",
    direction: "reply",
    opcode: FUSE_SYMLINK,
    ctx: context(41),
    value: entry,
  },
  {
    name: "mknod-request",
    direction: "request",
    opcode: FUSE_MKNOD,
    ctx: context(41),
    value: { mode: 0o100640, rdev: 0x1234, umask: 0o022, name: "device" },
  },
  {
    name: "mknod-reply",
    direction: "reply",
    opcode: FUSE_MKNOD,
    ctx: context(41),
    value: entry,
  },
  {
    name: "mkdir-request",
    direction: "request",
    opcode: FUSE_MKDIR,
    ctx: context(41),
    value: { mode: 0o40755, umask: 0o022, name: "directory" },
  },
  {
    name: "mkdir-reply",
    direction: "reply",
    opcode: FUSE_MKDIR,
    ctx: context(41),
    value: entry,
  },
  {
    name: "unlink-request",
    direction: "request",
    opcode: FUSE_UNLINK,
    ctx: context(41),
    value: { name: "victim" },
  },
  {
    name: "unlink-empty-reply",
    direction: "reply",
    opcode: FUSE_UNLINK,
    ctx: context(41),
    value: {},
  },
  {
    name: "rmdir-request",
    direction: "request",
    opcode: FUSE_RMDIR,
    ctx: context(41),
    value: { name: "empty" },
  },
  {
    name: "rmdir-empty-reply",
    direction: "reply",
    opcode: FUSE_RMDIR,
    ctx: context(41),
    value: {},
  },
  {
    name: "link-request",
    direction: "request",
    opcode: FUSE_LINK,
    ctx: context(41),
    value: { oldnodeid: 3n, name: "hard-link" },
  },
  {
    name: "link-reply",
    direction: "reply",
    opcode: FUSE_LINK,
    ctx: context(41),
    value: entry,
  },
  {
    name: "open-request",
    direction: "request",
    opcode: FUSE_OPEN,
    ctx: context(41),
    value: { flags: 0x241, openFlags: 1 },
  },
  {
    name: "open-reply",
    direction: "reply",
    opcode: FUSE_OPEN,
    ctx: context(41),
    value: open,
  },
  {
    name: "read-request",
    direction: "request",
    opcode: FUSE_READ,
    ctx: context(41),
    value: read,
  },
  {
    name: "read-reply",
    direction: "reply",
    opcode: FUSE_READ,
    ctx: context(41),
    value: { data: new Uint8Array([0, 1, 2, 3, 254, 255, 9]) },
  },
  {
    name: "xattr-get-reply",
    direction: "reply",
    opcode: FUSE_GETXATTR,
    ctx: context(41),
    value: { data: new Uint8Array([5, 4, 3, 2, 1]) },
  },
  {
    name: "xattr-list-request",
    direction: "request",
    opcode: FUSE_LISTXATTR,
    ctx: context(41),
    value: { size: 256 },
  },
  {
    name: "statfs-request",
    direction: "request",
    opcode: FUSE_STATFS,
    ctx: context(41),
    value: {},
  },
  {
    name: "release-request",
    direction: "request",
    opcode: FUSE_RELEASE,
    ctx: context(41),
    value: { fh: 0x0102030405060708n, flags: 0x241, releaseFlags: 3, lockOwner: 9n },
  },
  {
    name: "release-empty-reply",
    direction: "reply",
    opcode: FUSE_RELEASE,
    ctx: context(41),
    value: {},
  },
  {
    name: "fsync-request",
    direction: "request",
    opcode: FUSE_FSYNC,
    ctx: context(41),
    value: { fh: 0x1112131415161718n, fsyncFlags: 1 },
  },
  {
    name: "fsync-empty-reply",
    direction: "reply",
    opcode: FUSE_FSYNC,
    ctx: context(41),
    value: {},
  },
  {
    name: "removexattr-request",
    direction: "request",
    opcode: FUSE_REMOVEXATTR,
    ctx: context(41),
    value: { name: "user.mountx" },
  },
  {
    name: "removexattr-empty-reply",
    direction: "reply",
    opcode: FUSE_REMOVEXATTR,
    ctx: context(41),
    value: {},
  },
  {
    name: "flush-request",
    direction: "request",
    opcode: FUSE_FLUSH,
    ctx: context(41),
    value: { fh: 0x2122232425262728n, lockOwner: 0x3132333435363738n },
  },
  {
    name: "flush-empty-reply",
    direction: "reply",
    opcode: FUSE_FLUSH,
    ctx: context(41),
    value: {},
  },
  {
    name: "opendir-request",
    direction: "request",
    opcode: FUSE_OPENDIR,
    ctx: context(41),
    value: { flags: 0, openFlags: 0 },
  },
  {
    name: "opendir-reply",
    direction: "reply",
    opcode: FUSE_OPENDIR,
    ctx: context(41),
    value: open,
  },
  {
    name: "readdir-request",
    direction: "request",
    opcode: FUSE_READDIR,
    ctx: context(41),
    value: read,
  },
  {
    name: "readdir-reply",
    direction: "reply",
    opcode: FUSE_READDIR,
    ctx: context(41),
    value: dirents,
  },
  {
    name: "readdirplus-request",
    direction: "request",
    opcode: FUSE_READDIRPLUS,
    ctx: context(41),
    value: read,
  },
  {
    name: "readdirplus-reply",
    direction: "reply",
    opcode: FUSE_READDIRPLUS,
    ctx: context(41),
    value: direntsPlus,
  },
  {
    name: "releasedir-request",
    direction: "request",
    opcode: FUSE_RELEASEDIR,
    ctx: context(41),
    value: { fh: 0x4142434445464748n, flags: 0, releaseFlags: 0, lockOwner: 0n },
  },
  {
    name: "releasedir-empty-reply",
    direction: "reply",
    opcode: FUSE_RELEASEDIR,
    ctx: context(41),
    value: {},
  },
  {
    name: "fsyncdir-request",
    direction: "request",
    opcode: FUSE_FSYNCDIR,
    ctx: context(41),
    value: { fh: 0x5152535455565758n, fsyncFlags: 0 },
  },
  {
    name: "fsyncdir-empty-reply",
    direction: "reply",
    opcode: FUSE_FSYNCDIR,
    ctx: context(41),
    value: {},
  },
  {
    name: "setlkw-request",
    direction: "request",
    opcode: FUSE_SETLKW,
    ctx: context(41),
    value: { ...lk, lkFlags: 1 },
  },
  {
    name: "setlkw-empty-reply",
    direction: "reply",
    opcode: FUSE_SETLKW,
    ctx: context(41),
    value: {},
  },
  {
    name: "access-request",
    direction: "request",
    opcode: FUSE_ACCESS,
    ctx: context(41),
    value: { mask: 7 },
  },
  {
    name: "access-empty-reply",
    direction: "reply",
    opcode: FUSE_ACCESS,
    ctx: context(41),
    value: {},
  },
  {
    name: "interrupt-request",
    direction: "request",
    opcode: FUSE_INTERRUPT,
    ctx: context(41),
    value: { unique: 0x6162636465666768n },
  },
  {
    name: "interrupt-empty-reply",
    direction: "reply",
    opcode: FUSE_INTERRUPT,
    ctx: context(41),
    value: {},
  },
  {
    name: "bmap-request",
    direction: "request",
    opcode: FUSE_BMAP,
    ctx: context(41),
    value: { block: 17n, blocksize: 4096 },
  },
  {
    name: "bmap-reply",
    direction: "reply",
    opcode: FUSE_BMAP,
    ctx: context(41),
    value: { block: 33n },
  },
  {
    name: "destroy-request",
    direction: "request",
    opcode: FUSE_DESTROY,
    ctx: context(41),
    value: {},
  },
  {
    name: "destroy-empty-reply",
    direction: "reply",
    opcode: FUSE_DESTROY,
    ctx: context(41),
    value: {},
  },
  {
    name: "attr-out-old",
    direction: "reply",
    opcode: FUSE_GETATTR,
    ctx: context(8),
    value: { attrValid: 23n, attrValidNsec: 29, attr: attr({ blksize: 0, flags: 0 }) },
  },
  {
    name: "attr-out-new",
    direction: "reply",
    opcode: FUSE_GETATTR,
    ctx: context(41),
    value: { attrValid: 23n, attrValidNsec: 29, attr: attr() },
  },
  {
    name: "setattr-reply",
    direction: "reply",
    opcode: FUSE_SETATTR,
    ctx: context(41),
    value: { attrValid: 23n, attrValidNsec: 29, attr: attr() },
  },
  {
    name: "xattr-set-legacy",
    direction: "request",
    opcode: FUSE_SETXATTR,
    ctx: context(41),
    value: { flags: 1, setxattrFlags: 0, name: "user.mountx", value: new Uint8Array([0, 1, 2, 3, 255]) },
  },
  {
    name: "xattr-set-ext",
    direction: "request",
    opcode: FUSE_SETXATTR,
    ctx: context(41, true),
    value: { flags: 2, setxattrFlags: 1, name: "trusted.mountx", value: new Uint8Array([9, 8, 7, 6]) },
  },
  {
    name: "xattr-set-empty-reply",
    direction: "reply",
    opcode: FUSE_SETXATTR,
    ctx: context(41),
    value: {},
  },
  {
    name: "xattr-get-request",
    direction: "request",
    opcode: FUSE_GETXATTR,
    ctx: context(41),
    value: { size: 128, name: "user.mountx" },
  },
  {
    name: "xattr-list-reply",
    direction: "reply",
    opcode: FUSE_LISTXATTR,
    ctx: context(41),
    value: { data: new Uint8Array([...new TextEncoder().encode("user.mountx"), 0, ...new TextEncoder().encode("user.other"), 0]) },
  },
  {
    name: "lock-getlk-request",
    direction: "request",
    opcode: FUSE_GETLK,
    ctx: context(41),
    value: lk,
  },
  {
    name: "lock-getlk-reply",
    direction: "reply",
    opcode: FUSE_GETLK,
    ctx: context(41),
    value: { lk: { ...lock, type: 2, pid: 777 } },
  },
  {
    name: "lock-setlk-request",
    direction: "request",
    opcode: FUSE_SETLK,
    ctx: context(41),
    value: { ...lk, lkFlags: 0 },
  },
  {
    name: "lock-setlk-empty-reply",
    direction: "reply",
    opcode: FUSE_SETLK,
    ctx: context(41),
    value: {},
  },
  {
    name: "rename-request",
    direction: "request",
    opcode: FUSE_RENAME,
    ctx: context(41),
    value: { newdir: 0x1020304050607080n, oldName: "old-name", newName: "new-name" },
  },
  {
    name: "rename2-request",
    direction: "request",
    opcode: FUSE_RENAME2,
    ctx: context(41),
    value: { newdir: 0x1020304050607080n, flags: 3, oldName: "old-name", newName: "new-name" },
  },
  {
    name: "rename-empty-reply",
    direction: "reply",
    opcode: FUSE_RENAME,
    ctx: context(41),
    value: {},
  },
  {
    name: "rename2-empty-reply",
    direction: "reply",
    opcode: FUSE_RENAME2,
    ctx: context(41),
    value: {},
  },
  {
    name: "create-request",
    direction: "request",
    opcode: FUSE_CREATE,
    ctx: context(41),
    value: { flags: 0x241, mode: 0o100640, umask: 0o022, openFlags: 1, name: "created" },
  },
  {
    name: "create-reply",
    direction: "reply",
    opcode: FUSE_CREATE,
    ctx: context(41),
    value: { entry, open },
  },
  {
    name: "mknod-request-old",
    direction: "request",
    opcode: FUSE_MKNOD,
    ctx: context(8),
    value: { mode: 0o100640, rdev: 0x1234, umask: 0, name: "device-old" },
  },
  {
    name: "create-request-old",
    direction: "request",
    opcode: FUSE_CREATE,
    ctx: context(8),
    value: { flags: 0x241, mode: 0o100640, umask: 0, openFlags: 0, name: "created-old" },
  },
  {
    name: "init-out-old",
    direction: "reply",
    opcode: FUSE_INIT,
    ctx: context(3),
    value: {
      major: 7,
      minor: 3,
      maxReadahead: 0,
      flags: 0,
      maxBackground: 0,
      congestionThreshold: 0,
      maxWrite: 0,
      timeGran: 0,
      maxPages: 0,
      mapAlignment: 0,
      flags2: 0,
      maxStackDepth: 0,
    },
  },
  {
    name: "read-request-old",
    direction: "request",
    opcode: FUSE_READ,
    ctx: context(8),
    value: { ...read, lockOwner: 0n, flags: 0 },
  },
  {
    name: "statfs-old",
    direction: "reply",
    opcode: FUSE_STATFS,
    ctx: context(3),
    value: { blocks: 101n, bfree: 202n, bavail: 303n, files: 404n, ffree: 505n, bsize: 4096, namelen: 255, frsize: 0 },
  },
  {
    name: "statfs-new",
    direction: "reply",
    opcode: FUSE_STATFS,
    ctx: context(41),
    value: { blocks: 101n, bfree: 202n, bavail: 303n, files: 404n, ffree: 505n, bsize: 4096, namelen: 255, frsize: 4096 },
  },
  {
    name: "poll-request",
    direction: "request",
    opcode: FUSE_POLL,
    ctx: context(41),
    value: { fh: 0x0102030405060708n, kh: 0x1112131415161718n, flags: 1, events: 5 },
  },
  {
    name: "poll-reply",
    direction: "reply",
    opcode: FUSE_POLL,
    ctx: context(41),
    value: { revents: 4 },
  },
  {
    name: "fallocate-request",
    direction: "request",
    opcode: FUSE_FALLOCATE,
    ctx: context(41),
    value: { fh: 0x2122232425262728n, offset: 4096n, length: 8192n, mode: 1 },
  },
  {
    name: "fallocate-empty-reply",
    direction: "reply",
    opcode: FUSE_FALLOCATE,
    ctx: context(41),
    value: {},
  },
  {
    name: "lseek-request",
    direction: "request",
    opcode: FUSE_LSEEK,
    ctx: context(41),
    value: { fh: 0x3132333435363738n, offset: 123n, whence: 3 },
  },
  {
    name: "lseek-reply",
    direction: "reply",
    opcode: FUSE_LSEEK,
    ctx: context(41),
    value: { offset: 8192n },
  },
  {
    name: "batch-forget-request",
    direction: "request",
    opcode: FUSE_BATCH_FORGET,
    ctx: context(41),
    value: { forgets: [
      { nodeid: 2n, nlookup: 3n },
      { nodeid: 4n, nlookup: 5n },
    ] },
  },
];

const hex = (bytes) => Buffer.from(bytes).toString("hex");
const rows = fixtures.map((fixture) => {
  const bytes = fixture.direction === "request"
    ? encodeRequestBody(fixture.opcode, fixture.value, fixture.ctx)
    : encodeReplyBody(fixture.opcode, fixture.value, fixture.ctx);
  return [
    fixture.name,
    fixture.direction,
    fixture.opcode,
    fixture.ctx.minor,
    fixture.ctx.setxattrExt ? 1 : 0,
    hex(bytes),
  ].join("|");
});

const output = [
  "# Generated by tests/generate_protocol_fixtures.mjs; do not edit by hand.",
  `# mountx revision: ${EXPECTED_REVISION}`,
  "# format: name|direction|opcode|minor|setxattr_ext|body_hex",
  ...rows,
  "",
].join("\n");

const target = join(dirname(fileURLToPath(import.meta.url)), "fixtures/protocol-oracle.txt");
await mkdir(dirname(target), { recursive: true });
await writeFile(target, output, "utf8");
console.log(`wrote ${fixtures.length} fixtures to ${target}`);
