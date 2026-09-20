import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import * as fuse from "../fuse.cjs";
import {
  decodeOpenIn as namedDecodeOpenIn,
  encodeOpenIn as namedEncodeOpenIn,
  decodeOpenOut as namedDecodeOpenOut,
  encodeOpenOut as namedEncodeOpenOut,
  decodeLookupIn as namedDecodeLookupIn,
  encodeLookupIn as namedEncodeLookupIn,
  decodeLookupOut as namedDecodeLookupOut,
  encodeLookupOut as namedEncodeLookupOut,
  decodeCreateIn as namedDecodeCreateIn,
  encodeCreateIn as namedEncodeCreateIn,
  decodeCreateOut as namedDecodeCreateOut,
  encodeCreateOut as namedEncodeCreateOut,
  decodeReadlinkIn as namedDecodeReadlinkIn,
  encodeReadlinkIn as namedEncodeReadlinkIn,
  decodeReadlinkOut as namedDecodeReadlinkOut,
  encodeReadlinkOut as namedEncodeReadlinkOut,
  decodeReleaseIn as namedDecodeReleaseIn,
  encodeReleaseIn as namedEncodeReleaseIn,
  decodeFlushIn as namedDecodeFlushIn,
  encodeFlushIn as namedEncodeFlushIn,
  decodeFsyncIn as namedDecodeFsyncIn,
  encodeFsyncIn as namedEncodeFsyncIn,
  decodeReadIn as namedDecodeReadIn,
  encodeReadIn as namedEncodeReadIn,
  decodeReadOut as namedDecodeReadOut,
  encodeReadOut as namedEncodeReadOut,
  decodeWriteIn as namedDecodeWriteIn,
  encodeWriteIn as namedEncodeWriteIn,
  decodeWriteOut as namedDecodeWriteOut,
  encodeWriteOut as namedEncodeWriteOut,
  decodeGetattrIn as namedDecodeGetattrIn,
  encodeGetattrIn as namedEncodeGetattrIn,
  decodeGetattrOut as namedDecodeGetattrOut,
  encodeGetattrOut as namedEncodeGetattrOut,
  decodeSetattrIn as namedDecodeSetattrIn,
  encodeSetattrIn as namedEncodeSetattrIn,
  decodeSetattrOut as namedDecodeSetattrOut,
  encodeSetattrOut as namedEncodeSetattrOut,
  decodeStatfsIn as namedDecodeStatfsIn,
  encodeStatfsIn as namedEncodeStatfsIn,
  decodeStatfsOut as namedDecodeStatfsOut,
  encodeStatfsOut as namedEncodeStatfsOut,
} from "../fuse.cjs";

for (const [name, value] of [
  ["decodeOpenIn", namedDecodeOpenIn],
  ["encodeOpenIn", namedEncodeOpenIn],
  ["decodeOpenOut", namedDecodeOpenOut],
  ["encodeOpenOut", namedEncodeOpenOut],
  ["decodeLookupIn", namedDecodeLookupIn],
  ["encodeLookupIn", namedEncodeLookupIn],
  ["decodeLookupOut", namedDecodeLookupOut],
  ["encodeLookupOut", namedEncodeLookupOut],
  ["decodeCreateIn", namedDecodeCreateIn],
  ["encodeCreateIn", namedEncodeCreateIn],
  ["decodeCreateOut", namedDecodeCreateOut],
  ["encodeCreateOut", namedEncodeCreateOut],
  ["decodeReadlinkIn", namedDecodeReadlinkIn],
  ["encodeReadlinkIn", namedEncodeReadlinkIn],
  ["decodeReadlinkOut", namedDecodeReadlinkOut],
  ["encodeReadlinkOut", namedEncodeReadlinkOut],
  ["decodeReleaseIn", namedDecodeReleaseIn],
  ["encodeReleaseIn", namedEncodeReleaseIn],
  ["decodeFlushIn", namedDecodeFlushIn],
  ["encodeFlushIn", namedEncodeFlushIn],
  ["decodeFsyncIn", namedDecodeFsyncIn],
  ["encodeFsyncIn", namedEncodeFsyncIn],
  ["decodeReadIn", namedDecodeReadIn],
  ["encodeReadIn", namedEncodeReadIn],
  ["decodeReadOut", namedDecodeReadOut],
  ["encodeReadOut", namedEncodeReadOut],
  ["decodeWriteIn", namedDecodeWriteIn],
  ["encodeWriteIn", namedEncodeWriteIn],
  ["decodeWriteOut", namedDecodeWriteOut],
  ["encodeWriteOut", namedEncodeWriteOut],
  ["decodeGetattrIn", namedDecodeGetattrIn],
  ["encodeGetattrIn", namedEncodeGetattrIn],
  ["decodeGetattrOut", namedDecodeGetattrOut],
  ["encodeGetattrOut", namedEncodeGetattrOut],
  ["decodeSetattrIn", namedDecodeSetattrIn],
  ["encodeSetattrIn", namedEncodeSetattrIn],
  ["decodeSetattrOut", namedDecodeSetattrOut],
  ["encodeSetattrOut", namedEncodeSetattrOut],
  ["decodeStatfsIn", namedDecodeStatfsIn],
  ["encodeStatfsIn", namedEncodeStatfsIn],
  ["decodeStatfsOut", namedDecodeStatfsOut],
  ["encodeStatfsOut", namedEncodeStatfsOut],
]) {
  assert.equal(value, fuse[name], `FUSE named export ${name}`);
}

assert.equal(fuse.FUSE_KERNEL_VERSION, 7);
assert.equal(fuse.FUSE_KERNEL_MINOR_VERSION, 41);
assert.equal(fuse.FUSE_ROOT_ID, 1n);

const request = {
  len: 40,
  opcode: fuse.FUSE_GETATTR,
  unique: 42n,
  nodeid: fuse.FUSE_ROOT_ID,
  uid: 501,
  gid: 20,
  pid: 7,
  totalExtlen: 0,
};
assert.deepEqual(fuse.decodeInHeader(fuse.encodeInHeader(request)), request);

const reply = fuse.encodeReply(42n, Buffer.from("ok"));
assert.deepEqual(fuse.decodeOutHeader(reply), { len: 18, error: 0, unique: 42n });

const transcript = new fuse.TranscriptRecorder({ now: () => 17n });
transcript.tap("in", Buffer.from([0, 255]));
assert.deepEqual(fuse.decodeTranscript(transcript.encode()), [
  { direction: "in", timestamp: 0n, bytes: Buffer.from([0, 255]) },
]);

assert.equal(fuse.opcodeName(fuse.FUSE_GETATTR), "GETATTR");
assert.equal(fuse.nameByteLength("é"), 2);
assert.throws(() => fuse.decodeInHeader(Buffer.alloc(1)), fuse.ProtocolError);

const readContext = { minor: 41, setxattrExt: false };
const readInput = {
  fh: 0x0102030405060708n,
  offset: 0x1112131415161718n,
  size: 7,
  readFlags: 2,
  lockOwner: 0x2122232425262728n,
  flags: 0x241,
};
const readBody = fuse.encodeReadIn(readInput, readContext);
assert.equal(readBody.length, fuse.readWriteInSize(readContext.minor));
assert.deepEqual(fuse.decodeReadIn(readBody, readContext), readInput);
const readReplyData = Uint8Array.from([0, 1, 2, 3, 254, 255, 9]);
const readReply = fuse.encodeReadOut({ data: readReplyData });
assert.deepEqual(fuse.decodeReadOut(readReply), { data: Buffer.from(readReplyData) });

const writeContext = { minor: 41, setxattrExt: false };
const writeInput = {
  fh: 0x0102030405060708n,
  offset: 0x1112131415161718n,
  // The oracle derives this field from data while encoding. Keep a deliberately
  // different value here so the public binding proves that behavior rather
  // than accidentally trusting a stale caller-provided size.
  size: 0,
  writeFlags: 3,
  lockOwner: 0x2122232425262728n,
  flags: 0x4000,
  data: Uint8Array.from([0, 1, 2, 255, 254]),
};
const writeBody = fuse.encodeWriteIn(writeInput, writeContext);
assert.equal(writeBody.length, fuse.readWriteInSize(writeContext.minor) + writeInput.data.length);
assert.deepEqual(fuse.decodeWriteIn(writeBody, writeContext), {
  ...writeInput,
  size: writeInput.data.length,
  data: Buffer.from(writeInput.data),
});
assert.deepEqual(fuse.decodeWriteOut(fuse.encodeWriteOut({ size: 4 })), { size: 4 });

const getattrContext = { minor: 41, setxattrExt: false };
const getattrInput = { getattrFlags: fuse.FUSE_GETATTR_FH, fh: 0x3132333435363738n };
const setattrInput = {
  valid: fuse.FATTR_MODE | fuse.FATTR_SIZE | fuse.FATTR_ATIME | fuse.FATTR_MTIME,
  fh: 0x0102030405060708n,
  size: 0x1112131415161718n,
  lockOwner: 0x2122232425262728n,
  atime: 0x3132333435363738n,
  mtime: 0x4142434445464748n,
  ctime: 0x5152535455565758n,
  atimensec: 101,
  mtimensec: 202,
  ctimensec: 303,
  mode: 0o100640,
  uid: 501,
  gid: 20,
};
for (const ctx of [getattrContext, { minor: 8, setxattrExt: false }]) {
  const getattrBody = fuse.encodeGetattrIn(getattrInput, ctx);
  assert.equal(getattrBody.length, 16);
  assert.deepEqual(fuse.decodeGetattrIn(getattrBody, ctx), getattrInput);

  const setattrBody = fuse.encodeSetattrIn(setattrInput, ctx);
  assert.equal(setattrBody.length, 88);
  assert.deepEqual(fuse.decodeSetattrIn(setattrBody, ctx), setattrInput);
}

const openContext = { minor: 41, setxattrExt: false };
const openInput = {
  // Linux wire values; the FUSE barrel keeps the complete constant loop on
  // require() but only its core constants are statically discoverable through
  // Node's CommonJS-to-ESM bridge.
  flags: 0o2 | 0o2000,
  openFlags: 1,
};
const openBody = fuse.encodeOpenIn(openInput, openContext);
assert.equal(openBody.length, 8);
assert.deepEqual(fuse.decodeOpenIn(openBody, openContext), openInput);
const openReplyValue = {
  fh: 0x0102030405060708n,
  openFlags: 1 | 2,
  backingId: -7,
};
const openReplyBody = fuse.encodeOpenOut(openReplyValue, openContext);
assert.equal(openReplyBody.length, 16);
assert.deepEqual(fuse.decodeOpenOut(openReplyBody, openContext), openReplyValue);

const classify = (fn) => {
  try {
    return { ok: true, value: fn() };
  } catch (error) {
    return {
      ok: false,
      name: error?.name,
      code: error?.code,
      offset: error?.offset,
      message: error?.message,
    };
  }
};
const classifyProtocolError = (fn) => {
  const result = classify(fn);
  assert.equal(result.ok, false);
  return {
    ok: false,
    name: result.name,
    code: result.code,
    offset: result.offset,
  };
};

const dirents = [
  { ino: 11n, off: 12n, type: 4, name: "." },
  { ino: 21n, off: 22n, type: 8, name: "café" },
  { ino: 31n, off: 32n, type: 10, name: "link" },
];
const firstDirentSize = fuse.direntSize(Buffer.byteLength(dirents[0].name));
const packed = fuse.packDirents(dirents, firstDirentSize + 1);
assert.equal(packed.packed, 1);
assert.deepEqual(fuse.unpackDirents(packed.buffer), [dirents[0]]);
const allPacked = fuse.packDirents(dirents, 4096);
assert.equal(allPacked.packed, dirents.length);
assert.deepEqual(fuse.unpackDirents(allPacked.buffer), dirents);
assert.throws(() => fuse.unpackDirents(Buffer.alloc(1)), fuse.ProtocolError);

const zeroAttr = {
  ino: 0n,
  size: 0n,
  blocks: 0n,
  atime: 0n,
  mtime: 0n,
  ctime: 0n,
  atimensec: 0,
  mtimensec: 0,
  ctimensec: 0,
  mode: 0,
  nlink: 0,
  uid: 0,
  gid: 0,
  rdev: 0,
  blksize: 0,
  flags: 0,
};
const plusEntry = {
  nodeid: 3n,
  generation: 4n,
  entryValid: 5n,
  attrValid: 6n,
  entryValidNsec: 7,
  attrValidNsec: 8,
  attr: { ...zeroAttr, ino: 3n, size: 5n, mode: 0o100644, nlink: 1 },
};
const plusEntries = [
  { entry: { ...plusEntry, nodeid: 0n, attr: zeroAttr }, dirent: { ino: 1n, off: 1n, type: 4, name: "." } },
  { entry: plusEntry, dirent: { ino: 3n, off: 2n, type: 8, name: "café" } },
];
const plusContext = { minor: 41, setxattrExt: false };
const firstPlusSize = fuse.direntPlusSize(Buffer.byteLength(plusEntries[0].dirent.name), plusContext);
const plusFirst = fuse.packDirentsPlus(plusEntries, firstPlusSize + 1, plusContext);
assert.equal(plusFirst.packed, 1);
assert.equal(plusFirst.buffer.length, firstPlusSize);
assert.deepEqual(fuse.unpackDirentsPlus(plusFirst.buffer, plusContext), [plusEntries[0]]);
const plusAll = fuse.packDirentsPlus(plusEntries, 4096, plusContext);
assert.equal(plusAll.packed, plusEntries.length);
assert.deepEqual(fuse.unpackDirentsPlus(plusAll.buffer, plusContext), plusEntries);
assert.throws(
  () => fuse.packDirentsPlus([{ dirent: plusEntries[0].dirent }], 4096, plusContext),
  (error) => error instanceof fuse.ProtocolError && error.message === "fuse_direntplus needs a fuse_entry_out",
);

const wrappedPlusEntry = {
  ...plusEntry,
  nodeid: -1n,
  entryValidNsec: -1,
  attr: { ...plusEntry.attr, mode: -1, flags: 2 ** 32 + 1 },
};
const wrappedPlus = [{ entry: wrappedPlusEntry, dirent: { ino: -2n, off: 2n ** 80n, type: -1, name: "wrap" } }];
const wrappedPlusPacked = fuse.packDirentsPlus(wrappedPlus, 4096, plusContext);
assert.deepEqual(fuse.unpackDirentsPlus(wrappedPlusPacked.buffer, plusContext), [{
  entry: {
    ...wrappedPlusEntry,
    nodeid: 2n ** 64n - 1n,
    entryValidNsec: 2 ** 32 - 1,
    attr: { ...wrappedPlusEntry.attr, mode: 2 ** 32 - 1, flags: 1 },
  },
  dirent: { ino: 2n ** 64n - 2n, off: 0n, type: 2 ** 32 - 1, name: "wrap" },
}]);

const createContext = { minor: 41, setxattrExt: false };
const createInput = {
  flags: 0o2 | 0o100,
  mode: 0o100640,
  umask: 0o22,
  openFlags: 1,
  name: "créated",
};
const createReplyValue = { entry: plusEntry, open: openReplyValue };
for (const ctx of [createContext, { minor: 39, setxattrExt: false }, { minor: 12, setxattrExt: false }, { minor: 8, setxattrExt: false }]) {
  const body = fuse.encodeCreateIn(createInput, ctx);
  const head = ctx.minor >= 12 ? 16 : 8;
  assert.equal(body.length, head + Buffer.byteLength(createInput.name) + 1);
  assert.deepEqual(fuse.decodeCreateIn(body, ctx), {
    ...createInput,
    umask: ctx.minor >= 12 ? createInput.umask : 0,
    openFlags: ctx.minor >= 12 ? createInput.openFlags : 0,
  });
  const reply = fuse.encodeCreateOut(createReplyValue, ctx);
  assert.equal(reply.length, fuse.entryOutSize(ctx.minor) + 16);
  assert.deepEqual(fuse.decodeCreateOut(reply, ctx), {
    ...createReplyValue,
    open: {
      ...createReplyValue.open,
      backingId: ctx.minor >= 40 ? createReplyValue.open.backingId : 0,
    },
  });
}

const lookupContext = { minor: 41, setxattrExt: false };
const lookupInput = { name: "café" };
for (const ctx of [lookupContext, { minor: 39, setxattrExt: false }, { minor: 8, setxattrExt: false }]) {
  const body = fuse.encodeLookupIn(lookupInput, ctx);
  assert.equal(body.length, Buffer.byteLength(lookupInput.name) + 1);
  assert.deepEqual(fuse.decodeLookupIn(body, ctx), lookupInput);
  const reply = fuse.encodeLookupOut(plusEntry, ctx);
  assert.equal(reply.length, fuse.entryOutSize(ctx.minor));
  assert.deepEqual(fuse.decodeLookupOut(reply, ctx), plusEntry);
}

const emptyRequest = {};
const readlinkValue = { target: "../café/target" };
const statfsValue = {
  blocks: 0x0102030405060708n,
  bfree: 0x1112131415161718n,
  bavail: 0x2122232425262728n,
  files: 0x3132333435363738n,
  ffree: 0x4142434445464748n,
  bsize: 4096,
  namelen: 255,
  frsize: 512,
};
for (const [name, opcode, decodeIn, encodeIn] of [
  ["READLINK", fuse.FUSE_READLINK, fuse.decodeReadlinkIn, fuse.encodeReadlinkIn],
  ["STATFS", fuse.FUSE_STATFS, fuse.decodeStatfsIn, fuse.encodeStatfsIn],
]) {
  const body = encodeIn(emptyRequest);
  assert.equal(body.length, 0, `${name} request length`);
  assert.deepEqual(decodeIn(body), emptyRequest, `${name} request round trip`);
}
const readlinkBody = fuse.encodeReadlinkOut(readlinkValue);
assert.deepEqual(fuse.decodeReadlinkOut(readlinkBody), readlinkValue);
const statfsRoundTrip = fuse.encodeStatfsOut(statfsValue, { minor: 41, setxattrExt: false });
assert.equal(statfsRoundTrip.length, fuse.kstatfsSize(41));
assert.deepEqual(fuse.decodeStatfsOut(statfsRoundTrip, { minor: 41, setxattrExt: false }), statfsValue);

const lifecycleInput = {
  release: {
    fh: 0x0102030405060708n,
    flags: 0o2,
    releaseFlags: fuse.FUSE_RELEASE_FLUSH | fuse.FUSE_RELEASE_FLOCK_UNLOCK,
    lockOwner: 0x1112131415161718n,
  },
  flush: {
    fh: 0x2122232425262728n,
    lockOwner: 0x3132333435363738n,
  },
  fsync: {
    fh: 0x4142434445464748n,
    fsyncFlags: fuse.FUSE_FSYNC_FDATASYNC,
  },
};
const lifecycleCases = [
  ["RELEASE", fuse.FUSE_RELEASE, lifecycleInput.release, fuse.decodeReleaseIn, fuse.encodeReleaseIn, 24],
  ["RELEASEDIR", fuse.FUSE_RELEASEDIR, lifecycleInput.release, fuse.decodeReleaseIn, fuse.encodeReleaseIn, 24],
  ["FLUSH", fuse.FUSE_FLUSH, lifecycleInput.flush, fuse.decodeFlushIn, fuse.encodeFlushIn, 24],
  ["FSYNC", fuse.FUSE_FSYNC, lifecycleInput.fsync, fuse.decodeFsyncIn, fuse.encodeFsyncIn, 16],
  ["FSYNCDIR", fuse.FUSE_FSYNCDIR, lifecycleInput.fsync, fuse.decodeFsyncIn, fuse.encodeFsyncIn, 16],
];
for (const [name, , input, decodeIn, encodeIn, expectedLength] of lifecycleCases) {
  const body = encodeIn(input);
  assert.equal(body.length, expectedLength, `${name} request length`);
  assert.deepEqual(decodeIn(body), input, `${name} request round trip`);
}
assert.equal(fuse.encodeReply(99n).length, fuse.FUSE_OUT_HEADER_SIZE);

const source = process.env.MOUNTX_SOURCE;
if (source) {
  const oracle = await import(pathToFileURL(`${source}/src/fuse/protocol.ts`).href);
  for (const ctx of [readContext, { minor: 8, setxattrExt: false }]) {
    const oracleRead = oracle.encodeRequestBody(fuse.FUSE_READ, readInput, ctx);
    const actualRead = fuse.encodeReadIn(readInput, ctx);
    assert.deepEqual([...actualRead], [...oracleRead], `READ request bytes match oracle for minor ${ctx.minor}`);
    const oracleDecoded = oracle.decodeRequestBody(fuse.FUSE_READ, oracleRead, ctx);
    assert.deepEqual(
      fuse.decodeReadIn(actualRead, ctx),
      oracleDecoded,
      `READ request decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualRead.length; length++) {
      const body = actualRead.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeReadIn(body, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_READ, body, ctx)),
        `READ request truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const trailing = Buffer.concat([actualRead, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeReadIn(trailing, ctx)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_READ, trailing, ctx)),
      `READ request trailing-byte classification for minor ${ctx.minor}`,
    );

    const oracleReadReply = oracle.encodeReplyBody(fuse.FUSE_READ, { data: readReplyData }, ctx);
    const actualReadReply = fuse.encodeReadOut({ data: readReplyData });
    assert.deepEqual([...actualReadReply], [...oracleReadReply], "READ raw reply bytes match oracle");
    assert.deepEqual(
      [...fuse.decodeReadOut(actualReadReply).data],
      [...oracle.decodeReplyBody(fuse.FUSE_READ, oracleReadReply, ctx).data],
      "READ raw reply decode matches oracle",
    );
    for (const body of [Buffer.alloc(0), Buffer.from([1, 2, 3]), actualReadReply]) {
      const decoded = fuse.decodeReadOut(body);
      assert.deepEqual([...decoded.data], [...body], "READ raw reply preserves arbitrary bytes");
    }
  }

  for (const [opcode, name] of [
    [fuse.FUSE_OPEN, "OPEN"],
    [fuse.FUSE_OPENDIR, "OPENDIR"],
  ]) {
    for (const ctx of [openContext, { minor: 39, setxattrExt: false }, { minor: 8, setxattrExt: false }]) {
      const oracleRequest = oracle.encodeRequestBody(opcode, openInput, ctx);
      const actualRequest = fuse.encodeOpenIn(openInput, ctx);
      assert.deepEqual([...actualRequest], [...oracleRequest], `${name} request bytes match oracle for minor ${ctx.minor}`);
      assert.deepEqual(
        fuse.decodeOpenIn(actualRequest, ctx),
        oracle.decodeRequestBody(opcode, oracleRequest, ctx),
        `${name} request decode matches oracle for minor ${ctx.minor}`,
      );
      for (let length = 0; length < actualRequest.length; length++) {
        const body = actualRequest.subarray(0, length);
        assert.deepEqual(
          classifyProtocolError(() => fuse.decodeOpenIn(body, ctx)),
          classifyProtocolError(() => oracle.decodeRequestBody(opcode, body, ctx)),
          `${name} request truncation classification at ${length} bytes for minor ${ctx.minor}`,
        );
      }
      const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeOpenIn(requestTrailing, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(opcode, requestTrailing, ctx)),
        `${name} request trailing-byte classification for minor ${ctx.minor}`,
      );

      const oracleReply = oracle.encodeReplyBody(opcode, openReplyValue, ctx);
      const actualReply = fuse.encodeOpenOut(openReplyValue, ctx);
      assert.deepEqual([...actualReply], [...oracleReply], `${name} reply bytes match oracle for minor ${ctx.minor}`);
      assert.deepEqual(
        fuse.decodeOpenOut(actualReply, ctx),
        oracle.decodeReplyBody(opcode, oracleReply, ctx),
        `${name} typed reply decode matches oracle for minor ${ctx.minor}`,
      );
      for (let length = 0; length < actualReply.length; length++) {
        const body = actualReply.subarray(0, length);
        assert.deepEqual(
          classifyProtocolError(() => fuse.decodeOpenOut(body, ctx)),
          classifyProtocolError(() => oracle.decodeReplyBody(opcode, body, ctx)),
          `${name} reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
        );
      }
      const replyTrailing = Buffer.concat([actualReply, Buffer.from([0])]);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeOpenOut(replyTrailing, ctx)),
        classifyProtocolError(() => oracle.decodeReplyBody(opcode, replyTrailing, ctx)),
        `${name} reply trailing-byte classification for minor ${ctx.minor}`,
      );
    }
  }

  for (const ctx of [createContext, { minor: 39, setxattrExt: false }, { minor: 12, setxattrExt: false }, { minor: 8, setxattrExt: false }]) {
    const oracleRequest = oracle.encodeRequestBody(fuse.FUSE_CREATE, createInput, ctx);
    const actualRequest = fuse.encodeCreateIn(createInput, ctx);
    assert.deepEqual([...actualRequest], [...oracleRequest], `CREATE request bytes match oracle for minor ${ctx.minor}`);
    assert.deepEqual(
      fuse.decodeCreateIn(actualRequest, ctx),
      oracle.decodeRequestBody(fuse.FUSE_CREATE, oracleRequest, ctx),
      `CREATE request decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualRequest.length; length++) {
      const body = actualRequest.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeCreateIn(body, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_CREATE, body, ctx)),
        `CREATE request truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeCreateIn(requestTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_CREATE, requestTrailing, ctx)),
      `CREATE request trailing-byte classification for minor ${ctx.minor}`,
    );
    const malformedInput = { ...createInput, name: "bad\0name" };
    assert.deepEqual(
      classifyProtocolError(() => fuse.encodeCreateIn(malformedInput, ctx)),
      classifyProtocolError(() => oracle.encodeRequestBody(fuse.FUSE_CREATE, malformedInput, ctx)),
      `CREATE request malformed-name classification for minor ${ctx.minor}`,
    );

    const oracleReply = oracle.encodeReplyBody(fuse.FUSE_CREATE, createReplyValue, ctx);
    const actualReply = fuse.encodeCreateOut(createReplyValue, ctx);
    assert.deepEqual([...actualReply], [...oracleReply], `CREATE reply bytes match oracle for minor ${ctx.minor}`);
    assert.deepEqual(
      fuse.decodeCreateOut(actualReply, ctx),
      oracle.decodeReplyBody(fuse.FUSE_CREATE, oracleReply, ctx),
      `CREATE reply decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualReply.length; length++) {
      const body = actualReply.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeCreateOut(body, ctx)),
        classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_CREATE, body, ctx)),
        `CREATE reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const replyTrailing = Buffer.concat([actualReply, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeCreateOut(replyTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_CREATE, replyTrailing, ctx)),
      `CREATE reply trailing-byte classification for minor ${ctx.minor}`,
    );
  }

  for (const ctx of [lookupContext, { minor: 39, setxattrExt: false }, { minor: 8, setxattrExt: false }]) {
    const oracleRequest = oracle.encodeRequestBody(fuse.FUSE_LOOKUP, lookupInput, ctx);
    const actualRequest = fuse.encodeLookupIn(lookupInput, ctx);
    assert.deepEqual([...actualRequest], [...oracleRequest], `LOOKUP request bytes match oracle for minor ${ctx.minor}`);
    assert.deepEqual(
      fuse.decodeLookupIn(actualRequest, ctx),
      oracle.decodeRequestBody(fuse.FUSE_LOOKUP, oracleRequest, ctx),
      `LOOKUP request decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualRequest.length; length++) {
      const body = actualRequest.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeLookupIn(body, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_LOOKUP, body, ctx)),
        `LOOKUP request truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeLookupIn(requestTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_LOOKUP, requestTrailing, ctx)),
      `LOOKUP request trailing-byte classification for minor ${ctx.minor}`,
    );
    const malformedInput = { name: "bad\0name" };
    assert.deepEqual(
      classifyProtocolError(() => fuse.encodeLookupIn(malformedInput, ctx)),
      classifyProtocolError(() => oracle.encodeRequestBody(fuse.FUSE_LOOKUP, malformedInput, ctx)),
      `LOOKUP request malformed-name classification for minor ${ctx.minor}`,
    );

    const oracleReply = oracle.encodeReplyBody(fuse.FUSE_LOOKUP, plusEntry, ctx);
    const actualReply = fuse.encodeLookupOut(plusEntry, ctx);
    assert.deepEqual([...actualReply], [...oracleReply], `LOOKUP reply bytes match oracle for minor ${ctx.minor}`);
    assert.deepEqual(
      fuse.decodeLookupOut(actualReply, ctx),
      oracle.decodeReplyBody(fuse.FUSE_LOOKUP, oracleReply, ctx),
      `LOOKUP typed reply decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualReply.length; length++) {
      const body = actualReply.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeLookupOut(body, ctx)),
        classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_LOOKUP, body, ctx)),
        `LOOKUP reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const replyTrailing = Buffer.concat([actualReply, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeLookupOut(replyTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_LOOKUP, replyTrailing, ctx)),
      `LOOKUP reply trailing-byte classification for minor ${ctx.minor}`,
    );
  }

  for (const [name, opcode, decodeIn, encodeIn] of [
    ["READLINK", fuse.FUSE_READLINK, fuse.decodeReadlinkIn, fuse.encodeReadlinkIn],
    ["STATFS", fuse.FUSE_STATFS, fuse.decodeStatfsIn, fuse.encodeStatfsIn],
  ]) {
    const requestContext = { minor: 41, setxattrExt: false };
    const oracleRequest = oracle.encodeRequestBody(opcode, emptyRequest, requestContext);
    const actualRequest = encodeIn(emptyRequest);
    assert.deepEqual([...actualRequest], [...oracleRequest], `${name} request bytes match oracle`);
    assert.deepEqual(
      decodeIn(actualRequest),
      oracle.decodeRequestBody(opcode, oracleRequest, requestContext),
      `${name} request decode matches oracle`,
    );
    assert.deepEqual(
      classifyProtocolError(() => decodeIn(Buffer.from([0]))),
      classifyProtocolError(() => oracle.decodeRequestBody(opcode, Buffer.from([0]), requestContext)),
      `${name} request trailing-byte classification`,
    );
  }

  const oracleReadlink = oracle.encodeReplyBody(fuse.FUSE_READLINK, readlinkValue, { minor: 41, setxattrExt: false });
  assert.deepEqual([...readlinkBody], [...oracleReadlink], "READLINK reply bytes match oracle");
  assert.deepEqual(
    fuse.decodeReadlinkOut(readlinkBody),
    oracle.decodeReplyBody(fuse.FUSE_READLINK, oracleReadlink, { minor: 41, setxattrExt: false }),
    "READLINK typed reply decode matches oracle",
  );
  // READLINK consumes all remaining bytes as the target, so a byte suffix is
  // payload rather than a trailing-field error.
  const readlinkSuffix = Buffer.concat([readlinkBody, Buffer.from("/suffix")]);
  assert.deepEqual(
    fuse.decodeReadlinkOut(readlinkSuffix),
    oracle.decodeReplyBody(fuse.FUSE_READLINK, readlinkSuffix, { minor: 41, setxattrExt: false }),
    "READLINK variable-body suffix classification matches oracle",
  );
  assert.deepEqual(
    classifyProtocolError(() => fuse.encodeReadlinkOut({ target: "bad\0target" })),
    classifyProtocolError(() => oracle.encodeReplyBody(fuse.FUSE_READLINK, { target: "bad\0target" }, { minor: 41, setxattrExt: false })),
    "READLINK embedded-NUL classification matches oracle",
  );

  for (const ctx of [
    { minor: 41, setxattrExt: false },
    { minor: 39, setxattrExt: false },
    { minor: 8, setxattrExt: false },
    { minor: 4, setxattrExt: false },
    { minor: 3, setxattrExt: false },
  ]) {
    const oracleStatfs = oracle.encodeReplyBody(fuse.FUSE_STATFS, statfsValue, ctx);
    const actualStatfs = fuse.encodeStatfsOut(statfsValue, ctx);
    assert.deepEqual([...actualStatfs], [...oracleStatfs], `STATFS reply bytes match oracle for minor ${ctx.minor}`);
    const expectedStatfs = oracle.decodeReplyBody(fuse.FUSE_STATFS, oracleStatfs, ctx);
    assert.deepEqual(
      fuse.decodeStatfsOut(actualStatfs, ctx),
      expectedStatfs,
      `STATFS typed reply decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualStatfs.length; length++) {
      const body = actualStatfs.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeStatfsOut(body, ctx)),
        classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_STATFS, body, ctx)),
        `STATFS reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const trailing = Buffer.concat([actualStatfs, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeStatfsOut(trailing, ctx)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_STATFS, trailing, ctx)),
      `STATFS reply trailing-byte classification for minor ${ctx.minor}`,
    );
  }

  const lifecycleContext = { minor: 41, setxattrExt: false };
  for (const [name, opcode, input, decodeIn, encodeIn] of lifecycleCases) {
    const oracleRequest = oracle.encodeRequestBody(opcode, input, lifecycleContext);
    const actualRequest = encodeIn(input);
    assert.deepEqual(
      [...actualRequest],
      [...oracleRequest],
      `${name} request bytes match oracle`,
    );
    assert.deepEqual(
      decodeIn(actualRequest),
      oracle.decodeRequestBody(opcode, oracleRequest, lifecycleContext),
      `${name} request decode matches oracle`,
    );
    for (let length = 0; length < actualRequest.length; length++) {
      const body = actualRequest.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => decodeIn(body)),
        classifyProtocolError(() => oracle.decodeRequestBody(opcode, body, lifecycleContext)),
        `${name} request truncation classification at ${length} bytes`,
      );
    }
    const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => decodeIn(requestTrailing)),
      classifyProtocolError(() => oracle.decodeRequestBody(opcode, requestTrailing, lifecycleContext)),
      `${name} request trailing-byte classification`,
    );

    const oracleReplyBody = oracle.encodeReplyBody(opcode, {}, lifecycleContext);
    assert.equal(oracleReplyBody.length, 0, `${name} oracle reply body is empty`);
    const actualReply = fuse.encodeReply(99n);
    const oracleReply = oracle.encodeReplyFor(99n, opcode, {}, lifecycleContext);
    assert.deepEqual([...actualReply], [...oracleReply], `${name} empty reply framing matches oracle`);
    assert.deepEqual(
      oracle.decodeReplyBody(opcode, actualReply.subarray(fuse.FUSE_OUT_HEADER_SIZE), lifecycleContext),
      {},
      `${name} oracle accepts empty reply body`,
    );
  }

  const attrReply = {
    attrValid: 9n,
    attrValidNsec: 10,
    attr: {
      ino: 11n,
      size: 12n,
      blocks: 13n,
      atime: 14n,
      mtime: 15n,
      ctime: 16n,
      atimensec: 17,
      mtimensec: 18,
      ctimensec: 19,
      mode: 0o100644,
      nlink: 1,
      uid: 501,
      gid: 20,
      rdev: 0,
      blksize: 4096,
      flags: 0,
    },
  };
  for (const [opcode, name, decodeIn, encodeIn, decodeOut, encodeOut] of [
    [fuse.FUSE_GETATTR, "GETATTR", fuse.decodeGetattrIn, fuse.encodeGetattrIn, fuse.decodeGetattrOut, fuse.encodeGetattrOut],
    [fuse.FUSE_SETATTR, "SETATTR", fuse.decodeSetattrIn, fuse.encodeSetattrIn, fuse.decodeSetattrOut, fuse.encodeSetattrOut],
  ]) {
    const input = name === "GETATTR" ? getattrInput : setattrInput;
    for (const ctx of [getattrContext, { minor: 8, setxattrExt: false }]) {
      const oracleRequest = oracle.encodeRequestBody(opcode, input, ctx);
      const actualRequest = encodeIn(input, ctx);
      assert.deepEqual([...actualRequest], [...oracleRequest], `${name} request bytes match oracle for minor ${ctx.minor}`);
      assert.deepEqual(
        decodeIn(actualRequest, ctx),
        oracle.decodeRequestBody(opcode, oracleRequest, ctx),
        `${name} request decode matches oracle for minor ${ctx.minor}`,
      );
      for (let length = 0; length < actualRequest.length; length++) {
        const body = actualRequest.subarray(0, length);
        assert.deepEqual(
          classifyProtocolError(() => decodeIn(body, ctx)),
          classifyProtocolError(() => oracle.decodeRequestBody(opcode, body, ctx)),
          `${name} request truncation classification at ${length} bytes for minor ${ctx.minor}`,
        );
      }
      const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
      assert.deepEqual(
        classifyProtocolError(() => decodeIn(requestTrailing, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(opcode, requestTrailing, ctx)),
        `${name} request trailing-byte classification for minor ${ctx.minor}`,
      );

      const oracleReply = oracle.encodeReplyBody(opcode, attrReply, ctx);
      const actualReply = encodeOut(attrReply, ctx);
      assert.deepEqual([...actualReply], [...oracleReply], `${name} reply bytes match oracle for minor ${ctx.minor}`);
      assert.deepEqual(
        decodeOut(actualReply, ctx),
        oracle.decodeReplyBody(opcode, oracleReply, ctx),
        `${name} typed reply decode matches oracle for minor ${ctx.minor}`,
      );
      for (let length = 0; length < actualReply.length; length++) {
        const body = actualReply.subarray(0, length);
        assert.deepEqual(
          classifyProtocolError(() => decodeOut(body, ctx)),
          classifyProtocolError(() => oracle.decodeReplyBody(opcode, body, ctx)),
          `${name} reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
        );
      }
      const replyTrailing = Buffer.concat([actualReply, Buffer.from([0])]);
      assert.deepEqual(
        classifyProtocolError(() => decodeOut(replyTrailing, ctx)),
        classifyProtocolError(() => oracle.decodeReplyBody(opcode, replyTrailing, ctx)),
        `${name} reply trailing-byte classification for minor ${ctx.minor}`,
      );
    }
  }

  for (const ctx of [writeContext, { minor: 8, setxattrExt: false }]) {
    const oracleWrite = oracle.encodeRequestBody(fuse.FUSE_WRITE, writeInput, ctx);
    const actualWrite = fuse.encodeWriteIn(writeInput, ctx);
    assert.deepEqual([...actualWrite], [...oracleWrite], `WRITE request bytes match oracle for minor ${ctx.minor}`);
    const oracleDecoded = oracle.decodeRequestBody(fuse.FUSE_WRITE, oracleWrite, ctx);
    const actualDecoded = fuse.decodeWriteIn(actualWrite, ctx);
    assert.deepEqual(
      { ...actualDecoded, data: [...actualDecoded.data] },
      { ...oracleDecoded, data: [...oracleDecoded.data] },
      `WRITE request decode matches oracle for minor ${ctx.minor}`,
    );

    const oracleWriteReply = oracle.encodeReplyBody(fuse.FUSE_WRITE, { size: 4 }, ctx);
    const actualWriteReply = fuse.encodeWriteOut({ size: 4 });
    assert.deepEqual([...actualWriteReply], [...oracleWriteReply], "WRITE reply bytes match oracle");
    assert.deepEqual(
      fuse.decodeWriteOut(actualWriteReply),
      oracle.decodeReplyBody(fuse.FUSE_WRITE, oracleWriteReply, ctx),
      "WRITE reply decode matches oracle",
    );

    for (let length = 0; length < actualWrite.length; length++) {
      const body = actualWrite.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeWriteIn(body, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_WRITE, body, ctx)),
        `WRITE request truncation classification at ${length} bytes`,
      );
    }
    const corruptSize = Buffer.from(actualWrite);
    corruptSize.writeUInt32LE(writeInput.data.length + 1, 16);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeWriteIn(corruptSize, ctx)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_WRITE, corruptSize, ctx)),
      "WRITE declared-size classification",
    );
  }

  for (let length = 0; length < 8; length++) {
    const body = Buffer.alloc(length);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeWriteOut(body)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_WRITE, body, writeContext)),
      `WRITE reply truncation classification at ${length} bytes`,
    );
  }

  const oraclePacked = oracle.packDirents(dirents, firstDirentSize + 1);
  assert.deepEqual([...packed.buffer], [...oraclePacked.buffer], "READDIR body bytes match oracle");
  assert.equal(packed.packed, oraclePacked.packed, "READDIR packed count matches oracle");
  assert.deepEqual(fuse.unpackDirents(packed.buffer), oracle.unpackDirents(oraclePacked.buffer));
  const oracleAllPacked = oracle.packDirents(dirents, 4096);
  assert.deepEqual([...allPacked.buffer], [...oracleAllPacked.buffer], "READDIR records match oracle");
  assert.deepEqual(fuse.unpackDirents(allPacked.buffer), oracle.unpackDirents(oracleAllPacked.buffer));
  for (const ctx of [plusContext, { minor: 8, setxattrExt: false }]) {
    const oraclePackedPlus = oracle.packDirentsPlus(plusEntries, 4096, ctx);
    const actualPackedPlus = fuse.packDirentsPlus(plusEntries, 4096, ctx);
    assert.equal(actualPackedPlus.packed, oraclePackedPlus.packed, "READDIRPLUS packed count matches oracle");
    assert.deepEqual([...actualPackedPlus.buffer], [...oraclePackedPlus.buffer], "READDIRPLUS body bytes match oracle");
    assert.deepEqual(
      fuse.unpackDirentsPlus(actualPackedPlus.buffer, ctx),
      oracle.unpackDirentsPlus(oraclePackedPlus.buffer, ctx),
      "READDIRPLUS decoded values match oracle",
    );
  }
  const oracleWrappedPlus = oracle.packDirentsPlus(wrappedPlus, 4096, plusContext);
  assert.deepEqual([...wrappedPlusPacked.buffer], [...oracleWrappedPlus.buffer], "READDIRPLUS integer coercions match oracle");
  for (let length = 0; length < plusAll.buffer.length; length++) {
    const body = plusAll.buffer.subarray(0, length);
    const expected = classify(() => oracle.unpackDirentsPlus(body, plusContext));
    const actual = classify(() => fuse.unpackDirentsPlus(body, plusContext));
    if (!expected.ok) {
      assert.deepEqual(actual, expected, `READDIRPLUS truncation classification at ${length} bytes`);
    } else {
      assert.deepEqual(actual.value, expected.value, `READDIRPLUS prefix decode at ${length} bytes`);
    }
  }
  for (const ctx of [plusContext, { minor: 8, setxattrExt: false }]) {
    const page = fuse.packDirentsPlus([plusEntries[0]], 4096, ctx).buffer;
    const malformedName = Buffer.from(page);
    malformedName.writeUInt32LE(0xffff, (ctx.minor < 9 ? 120 : 128) + 16);
    assert.deepEqual(
      classify(() => fuse.unpackDirentsPlus(malformedName, ctx)),
      classify(() => oracle.unpackDirentsPlus(malformedName, ctx)),
      `READDIRPLUS namelen classification for minor ${ctx.minor}`,
    );
    const truncatedPadding = page.subarray(0, page.length - 1);
    assert.deepEqual(
      classify(() => fuse.unpackDirentsPlus(truncatedPadding, ctx)),
      classify(() => oracle.unpackDirentsPlus(truncatedPadding, ctx)),
      `READDIRPLUS padding classification for minor ${ctx.minor}`,
    );
  }
  assert.deepEqual(
    classify(() => fuse.packDirentsPlus([{ dirent: plusEntries[0].dirent }], 4096, plusContext)),
    classify(() => oracle.packDirentsPlus([{ dirent: plusEntries[0].dirent }], 4096, plusContext)),
    "READDIRPLUS missing-entry classification matches oracle",
  );
  console.log("mount-rs N-API FUSE READDIR body differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE READDIRPLUS body differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE READ request/raw-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE WRITE request/reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE GETATTR request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE SETATTR request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE OPEN/OPENDIR request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE CREATE request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE LOOKUP request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE READLINK request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE STATFS request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE RELEASE/RELEASEDIR, FLUSH, FSYNC/FSYNCDIR request/status differential: PASS (pinned oracle)");
} else {
  console.log("mount-rs N-API FUSE READDIR body differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE READDIRPLUS body differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE READ request/raw-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE WRITE request/reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE GETATTR request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE SETATTR request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE OPEN/OPENDIR request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE CREATE request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE LOOKUP request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE READLINK request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE STATFS request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE RELEASE/RELEASEDIR, FLUSH, FSYNC/FSYNCDIR request/status differential: SKIP (MOUNTX_SOURCE unset)");
}

console.log("mount-rs N-API FUSE codec subpath: PASS");
