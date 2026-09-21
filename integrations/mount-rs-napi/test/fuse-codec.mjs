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
  decodeSymlinkIn as namedDecodeSymlinkIn,
  encodeSymlinkIn as namedEncodeSymlinkIn,
  decodeSymlinkOut as namedDecodeSymlinkOut,
  encodeSymlinkOut as namedEncodeSymlinkOut,
  decodeMknodIn as namedDecodeMknodIn,
  encodeMknodIn as namedEncodeMknodIn,
  decodeMknodOut as namedDecodeMknodOut,
  encodeMknodOut as namedEncodeMknodOut,
  decodeMkdirIn as namedDecodeMkdirIn,
  encodeMkdirIn as namedEncodeMkdirIn,
  decodeMkdirOut as namedDecodeMkdirOut,
  encodeMkdirOut as namedEncodeMkdirOut,
  decodeUnlinkIn as namedDecodeUnlinkIn,
  encodeUnlinkIn as namedEncodeUnlinkIn,
  decodeRmdirIn as namedDecodeRmdirIn,
  encodeRmdirIn as namedEncodeRmdirIn,
  decodeRenameIn as namedDecodeRenameIn,
  encodeRenameIn as namedEncodeRenameIn,
  decodeRename2In as namedDecodeRename2In,
  encodeRename2In as namedEncodeRename2In,
  decodeLinkIn as namedDecodeLinkIn,
  encodeLinkIn as namedEncodeLinkIn,
  decodeLinkOut as namedDecodeLinkOut,
  encodeLinkOut as namedEncodeLinkOut,
  decodeAccessIn as namedDecodeAccessIn,
  encodeAccessIn as namedEncodeAccessIn,
  decodeCreateIn as namedDecodeCreateIn,
  encodeCreateIn as namedEncodeCreateIn,
  decodeCreateOut as namedDecodeCreateOut,
  encodeCreateOut as namedEncodeCreateOut,
  decodeBatchForgetIn as namedDecodeBatchForgetIn,
  encodeBatchForgetIn as namedEncodeBatchForgetIn,
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
  decodeSyncfsIn as namedDecodeSyncfsIn,
  encodeSyncfsIn as namedEncodeSyncfsIn,
  decodeLkIn as namedDecodeLkIn,
  encodeLkIn as namedEncodeLkIn,
  decodeLkOut as namedDecodeLkOut,
  encodeLkOut as namedEncodeLkOut,
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
  decodeInterruptIn as namedDecodeInterruptIn,
  encodeInterruptIn as namedEncodeInterruptIn,
  decodeIoctlIn as namedDecodeIoctlIn,
  encodeIoctlIn as namedEncodeIoctlIn,
  decodeIoctlOut as namedDecodeIoctlOut,
  encodeIoctlOut as namedEncodeIoctlOut,
  decodePollIn as namedDecodePollIn,
  encodePollIn as namedEncodePollIn,
  decodePollOut as namedDecodePollOut,
  encodePollOut as namedEncodePollOut,
  decodeBmapIn as namedDecodeBmapIn,
  encodeBmapIn as namedEncodeBmapIn,
  decodeBmapOut as namedDecodeBmapOut,
  encodeBmapOut as namedEncodeBmapOut,
  decodeFallocateIn as namedDecodeFallocateIn,
  encodeFallocateIn as namedEncodeFallocateIn,
  decodeLseekIn as namedDecodeLseekIn,
  encodeLseekIn as namedEncodeLseekIn,
  decodeLseekOut as namedDecodeLseekOut,
  encodeLseekOut as namedEncodeLseekOut,
  decodeSetxattrIn as namedDecodeSetxattrIn,
  encodeSetxattrIn as namedEncodeSetxattrIn,
  decodeGetxattrIn as namedDecodeGetxattrIn,
  encodeGetxattrIn as namedEncodeGetxattrIn,
  decodeListxattrIn as namedDecodeListxattrIn,
  encodeListxattrIn as namedEncodeListxattrIn,
  decodeRemovexattrIn as namedDecodeRemovexattrIn,
  encodeRemovexattrIn as namedEncodeRemovexattrIn,
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
  ["decodeSymlinkIn", namedDecodeSymlinkIn],
  ["encodeSymlinkIn", namedEncodeSymlinkIn],
  ["decodeSymlinkOut", namedDecodeSymlinkOut],
  ["encodeSymlinkOut", namedEncodeSymlinkOut],
  ["decodeMknodIn", namedDecodeMknodIn],
  ["encodeMknodIn", namedEncodeMknodIn],
  ["decodeMknodOut", namedDecodeMknodOut],
  ["encodeMknodOut", namedEncodeMknodOut],
  ["decodeMkdirIn", namedDecodeMkdirIn],
  ["encodeMkdirIn", namedEncodeMkdirIn],
  ["decodeMkdirOut", namedDecodeMkdirOut],
  ["encodeMkdirOut", namedEncodeMkdirOut],
  ["decodeUnlinkIn", namedDecodeUnlinkIn],
  ["encodeUnlinkIn", namedEncodeUnlinkIn],
  ["decodeRmdirIn", namedDecodeRmdirIn],
  ["encodeRmdirIn", namedEncodeRmdirIn],
  ["decodeRenameIn", namedDecodeRenameIn],
  ["encodeRenameIn", namedEncodeRenameIn],
  ["decodeRename2In", namedDecodeRename2In],
  ["encodeRename2In", namedEncodeRename2In],
  ["decodeLinkIn", namedDecodeLinkIn],
  ["encodeLinkIn", namedEncodeLinkIn],
  ["decodeLinkOut", namedDecodeLinkOut],
  ["encodeLinkOut", namedEncodeLinkOut],
  ["decodeAccessIn", namedDecodeAccessIn],
  ["encodeAccessIn", namedEncodeAccessIn],
  ["decodeCreateIn", namedDecodeCreateIn],
  ["encodeCreateIn", namedEncodeCreateIn],
  ["decodeCreateOut", namedDecodeCreateOut],
  ["encodeCreateOut", namedEncodeCreateOut],
  ["decodeBatchForgetIn", namedDecodeBatchForgetIn],
  ["encodeBatchForgetIn", namedEncodeBatchForgetIn],
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
  ["decodeSyncfsIn", namedDecodeSyncfsIn],
  ["encodeSyncfsIn", namedEncodeSyncfsIn],
  ["decodeLkIn", namedDecodeLkIn],
  ["encodeLkIn", namedEncodeLkIn],
  ["decodeLkOut", namedDecodeLkOut],
  ["encodeLkOut", namedEncodeLkOut],
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
  ["decodeInterruptIn", namedDecodeInterruptIn],
  ["encodeInterruptIn", namedEncodeInterruptIn],
  ["decodeIoctlIn", namedDecodeIoctlIn],
  ["encodeIoctlIn", namedEncodeIoctlIn],
  ["decodeIoctlOut", namedDecodeIoctlOut],
  ["encodeIoctlOut", namedEncodeIoctlOut],
  ["decodePollIn", namedDecodePollIn],
  ["encodePollIn", namedEncodePollIn],
  ["decodePollOut", namedDecodePollOut],
  ["encodePollOut", namedEncodePollOut],
  ["decodeBmapIn", namedDecodeBmapIn],
  ["encodeBmapIn", namedEncodeBmapIn],
  ["decodeBmapOut", namedDecodeBmapOut],
  ["encodeBmapOut", namedEncodeBmapOut],
  ["decodeFallocateIn", namedDecodeFallocateIn],
  ["encodeFallocateIn", namedEncodeFallocateIn],
  ["decodeLseekIn", namedDecodeLseekIn],
  ["encodeLseekIn", namedEncodeLseekIn],
  ["decodeLseekOut", namedDecodeLseekOut],
  ["encodeLseekOut", namedEncodeLseekOut],
  ["decodeSetxattrIn", namedDecodeSetxattrIn],
  ["encodeSetxattrIn", namedEncodeSetxattrIn],
  ["decodeGetxattrIn", namedDecodeGetxattrIn],
  ["encodeGetxattrIn", namedEncodeGetxattrIn],
  ["decodeListxattrIn", namedDecodeListxattrIn],
  ["encodeListxattrIn", namedEncodeListxattrIn],
  ["decodeRemovexattrIn", namedDecodeRemovexattrIn],
  ["encodeRemovexattrIn", namedEncodeRemovexattrIn],
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
const compareProtocolOutcome = (actualFn, expectedFn, message) => {
  const actual = classify(actualFn);
  const expected = classify(expectedFn);
  if (actual.ok && expected.ok) {
    assert.deepEqual(actual.value, expected.value, message);
    return;
  }
  if (!actual.ok && !expected.ok) {
    assert.deepEqual(
      { ok: false, name: actual.name, code: actual.code, offset: actual.offset },
      { ok: false, name: expected.name, code: expected.code, offset: expected.offset },
      message,
    );
    return;
  }
  assert.deepEqual(actual, expected, message);
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

const symlinkInput = { name: "alias", target: "../café/target" };
const symlinkBody = fuse.encodeSymlinkIn(symlinkInput);
assert.deepEqual(fuse.decodeSymlinkIn(symlinkBody), symlinkInput);
assert.deepEqual(fuse.decodeSymlinkOut(fuse.encodeSymlinkOut(plusEntry, lookupContext), lookupContext), plusEntry);

const mknodInput = { mode: 0o100640, rdev: 0x01020304, umask: 0o22, name: "device" };
for (const ctx of [lookupContext, { minor: 8, setxattrExt: false }]) {
  const body = fuse.encodeMknodIn(mknodInput, ctx);
  assert.deepEqual(fuse.decodeMknodIn(body, ctx), {
    ...mknodInput,
    umask: ctx.minor >= 12 ? mknodInput.umask : 0,
  });
  assert.deepEqual(fuse.decodeMknodOut(fuse.encodeMknodOut(plusEntry, ctx), ctx), plusEntry);
}

const mkdirInput = { mode: 0o40750, umask: 0o27, name: "new-dir" };
const mkdirBody = fuse.encodeMkdirIn(mkdirInput);
assert.deepEqual(fuse.decodeMkdirIn(mkdirBody), mkdirInput);
assert.deepEqual(fuse.decodeMkdirOut(fuse.encodeMkdirOut(plusEntry, lookupContext), lookupContext), plusEntry);

const nameOperations = [
  ["unlink", fuse.decodeUnlinkIn, fuse.encodeUnlinkIn],
  ["rmdir", fuse.decodeRmdirIn, fuse.encodeRmdirIn],
];
for (const [name, decode, encode] of nameOperations) {
  const value = { name: `${name}-entry` };
  assert.deepEqual(decode(encode(value)), value, `${name} request round trip`);
}

const renameInput = { newdir: 0x0102030405060708n, oldName: "old name", newName: "new name" };
assert.deepEqual(fuse.decodeRenameIn(fuse.encodeRenameIn(renameInput)), renameInput);
const rename2Input = { ...renameInput, flags: 0x3 };
assert.deepEqual(fuse.decodeRename2In(fuse.encodeRename2In(rename2Input)), rename2Input);

const linkInput = { oldnodeid: 0x1112131415161718n, name: "hard-link" };
assert.deepEqual(fuse.decodeLinkIn(fuse.encodeLinkIn(linkInput)), linkInput);
assert.deepEqual(fuse.decodeLinkOut(fuse.encodeLinkOut(plusEntry, lookupContext), lookupContext), plusEntry);

const accessInput = { mask: 0x7 };
assert.deepEqual(fuse.decodeAccessIn(fuse.encodeAccessIn(accessInput)), accessInput);

const fallocateInput = {
  fh: 0x0102030405060708n,
  offset: 0x1112131415161718n,
  length: 0x2122232425262728n,
  mode: 0x3,
};
assert.deepEqual(fuse.decodeFallocateIn(fuse.encodeFallocateIn(fallocateInput)), fallocateInput);
const lseekInput = { fh: 0x3132333435363738n, offset: 0x4142434445464748n, whence: fuse.SEEK_DATA };
assert.deepEqual(fuse.decodeLseekIn(fuse.encodeLseekIn(lseekInput)), lseekInput);
const lseekReply = { offset: 0x5152535455565758n };
assert.deepEqual(fuse.decodeLseekOut(fuse.encodeLseekOut(lseekReply)), lseekReply);
const lkInput = {
  fh: 0x0102030405060708n,
  owner: 0x1112131415161718n,
  lk: {
    start: 0x2122232425262728n,
    end: 0x3132333435363738n,
    type: fuse.F_WRLCK,
    pid: 4242,
  },
  lkFlags: fuse.FUSE_LK_FLOCK,
};
assert.deepEqual(fuse.decodeLkIn(fuse.encodeLkIn(lkInput)), lkInput);
const lkReply = {
  lk: {
    start: 0x4142434445464748n,
    end: 0x5152535455565758n,
    type: fuse.F_UNLCK,
    pid: 777,
  },
};
assert.deepEqual(fuse.decodeLkOut(fuse.encodeLkOut(lkReply)), lkReply);

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

const batchForgetValue = {
  forgets: [
    { nodeid: 2n, nlookup: 3n },
    { nodeid: 0x0102030405060708n, nlookup: 0x1112131415161718n },
  ],
};
const batchForgetBody = fuse.encodeBatchForgetIn(batchForgetValue);
assert.equal(batchForgetBody.length, 8 + batchForgetValue.forgets.length * 16);
assert.deepEqual(fuse.decodeBatchForgetIn(batchForgetBody), batchForgetValue);
const emptyBatchForget = { forgets: [] };
assert.equal(fuse.encodeBatchForgetIn(emptyBatchForget).length, 8);
assert.deepEqual(fuse.decodeBatchForgetIn(fuse.encodeBatchForgetIn(emptyBatchForget)), emptyBatchForget);

const interruptValue = { unique: 0x6162636465666768n };
const interruptBody = fuse.encodeInterruptIn(interruptValue);
assert.equal(interruptBody.length, 8);
assert.deepEqual(fuse.decodeInterruptIn(interruptBody), interruptValue);

const pollInput = {
  fh: 0x0102030405060708n,
  kh: 0x1112131415161718n,
  flags: fuse.FUSE_POLL_SCHEDULE_NOTIFY,
  events: 0x05,
};
const pollReplyValue = { revents: 0x04 };
const pollContexts = [
  { minor: 41, setxattrExt: false },
  { minor: 39, setxattrExt: false },
  { minor: 8, setxattrExt: false },
  { minor: 3, setxattrExt: false },
];
for (const ctx of pollContexts) {
  const pollBody = fuse.encodePollIn(pollInput, ctx);
  assert.equal(pollBody.length, 24, `POLL request length for minor ${ctx.minor}`);
  assert.deepEqual(
    fuse.decodePollIn(pollBody, ctx),
    pollInput,
    `POLL request round trip for minor ${ctx.minor}`,
  );
  const pollReplyBody = fuse.encodePollOut(pollReplyValue, ctx);
  assert.equal(pollReplyBody.length, 8, `POLL reply length for minor ${ctx.minor}`);
  assert.deepEqual(
    fuse.decodePollOut(pollReplyBody, ctx),
    pollReplyValue,
    `POLL reply round trip for minor ${ctx.minor}`,
  );
}

const ioctlInput = {
  fh: 0x0102030405060708n,
  flags: 0x3,
  cmd: 0x80185879,
  arg: 0x1112131415161718n,
  inSize: 0x21222324,
  outSize: 0x31323334,
};
const ioctlReplyValue = {
  result: -25,
  flags: 0x5,
  inIovs: 0x41424344,
  outIovs: 0x51525354,
};
const ioctlContexts = [
  { minor: 41, setxattrExt: false },
  { minor: 39, setxattrExt: false },
  { minor: 8, setxattrExt: false },
  { minor: 3, setxattrExt: false },
];
const encodeIoctlInOracle = (value) => {
  const body = Buffer.alloc(32);
  body.writeBigUInt64LE(value.fh, 0);
  body.writeUInt32LE(value.flags, 8);
  body.writeUInt32LE(value.cmd, 12);
  body.writeBigUInt64LE(value.arg, 16);
  body.writeUInt32LE(value.inSize, 24);
  body.writeUInt32LE(value.outSize, 28);
  return body;
};
const encodeIoctlOutOracle = (value) => {
  const body = Buffer.alloc(16);
  body.writeInt32LE(value.result, 0);
  body.writeUInt32LE(value.flags, 4);
  body.writeUInt32LE(value.inIovs, 8);
  body.writeUInt32LE(value.outIovs, 12);
  return body;
};
const ioctlBody = fuse.encodeIoctlIn(ioctlInput);
const ioctlReplyBody = fuse.encodeIoctlOut(ioctlReplyValue);
assert.equal(fuse.FUSE_IOCTL, 39);
assert.equal(ioctlBody.length, 32);
assert.equal(ioctlReplyBody.length, 16);
assert.deepEqual(fuse.decodeIoctlIn(ioctlBody), ioctlInput);
assert.deepEqual(fuse.decodeIoctlOut(ioctlReplyBody), ioctlReplyValue);
assert.deepEqual([...ioctlBody], [...encodeIoctlInOracle(ioctlInput)]);
assert.deepEqual([...ioctlReplyBody], [...encodeIoctlOutOracle(ioctlReplyValue)]);
for (let length = 0; length < ioctlBody.length; length++) {
  const result = classifyProtocolError(() => fuse.decodeIoctlIn(ioctlBody.subarray(0, length)));
  assert.equal(result.name, "ProtocolError");
  assert.equal(result.code, "ERR_FUSE_PROTOCOL");
}
for (let length = 0; length < ioctlReplyBody.length; length++) {
  const result = classifyProtocolError(() => fuse.decodeIoctlOut(ioctlReplyBody.subarray(0, length)));
  assert.equal(result.name, "ProtocolError");
  assert.equal(result.code, "ERR_FUSE_PROTOCOL");
}
assert.equal(
  classifyProtocolError(() => fuse.decodeIoctlIn(Buffer.concat([ioctlBody, Buffer.from([0])]))).offset,
  32,
);
assert.equal(
  classifyProtocolError(() => fuse.decodeIoctlOut(Buffer.concat([ioctlReplyBody, Buffer.from([0])]))).offset,
  16,
);
const ioctlErrorReply = fuse.encodeErrorReply(99n, "EIO");
assert.deepEqual(fuse.decodeOutHeader(ioctlErrorReply), { len: 16, error: -5, unique: 99n });

const bmapInput = {
  block: 0x0102030405060708n,
  blocksize: 0x10203040,
};
const bmapReplyValue = { block: 0x1112131415161718n };
const bmapContexts = [
  { minor: 41, setxattrExt: false },
  { minor: 39, setxattrExt: false },
  { minor: 8, setxattrExt: false },
  { minor: 3, setxattrExt: false },
];
for (const ctx of bmapContexts) {
  const bmapBody = fuse.encodeBmapIn(bmapInput, ctx);
  assert.equal(bmapBody.length, 16, `BMAP request length for minor ${ctx.minor}`);
  assert.deepEqual(
    fuse.decodeBmapIn(bmapBody, ctx),
    bmapInput,
    `BMAP request round trip for minor ${ctx.minor}`,
  );
  const bmapReplyBody = fuse.encodeBmapOut(bmapReplyValue, ctx);
  assert.equal(bmapReplyBody.length, 8, `BMAP reply length for minor ${ctx.minor}`);
  assert.deepEqual(
    fuse.decodeBmapOut(bmapReplyBody, ctx),
    bmapReplyValue,
    `BMAP reply round trip for minor ${ctx.minor}`,
  );
}

const xattrContexts = [
  { minor: 41, setxattrExt: false },
  { minor: 41, setxattrExt: true },
  { minor: 39, setxattrExt: false },
  { minor: 8, setxattrExt: false },
];
const setxattrInput = {
  flags: fuse.XATTR_CREATE,
  setxattrFlags: fuse.FUSE_SETXATTR_ACL_KILL_SGID,
  name: "user.mountx",
  value: Uint8Array.from([0, 1, 2, 3, 255]),
};
const getxattrInput = { size: 0x80, name: "user.mountx" };
const listxattrInput = { size: 0x100 };
const removexattrInput = { name: "user.mountx" };
for (const ctx of xattrContexts) {
  const setxattrBody = fuse.encodeSetxattrIn(setxattrInput, ctx);
  const decodedSetxattr = fuse.decodeSetxattrIn(setxattrBody, ctx);
  assert.equal(setxattrBody.length, (ctx.setxattrExt ? 16 : 8) + Buffer.byteLength(setxattrInput.name) + 1 + setxattrInput.value.length);
  assert.deepEqual(
    { ...decodedSetxattr, value: Buffer.from(decodedSetxattr.value) },
    { ...setxattrInput, setxattrFlags: ctx.setxattrExt ? setxattrInput.setxattrFlags : 0, value: Buffer.from(setxattrInput.value) },
  );
  const getxattrBody = fuse.encodeGetxattrIn(getxattrInput, ctx);
  assert.equal(getxattrBody.length, 8 + Buffer.byteLength(getxattrInput.name) + 1);
  assert.deepEqual(fuse.decodeGetxattrIn(getxattrBody, ctx), getxattrInput);
  const listxattrBody = fuse.encodeListxattrIn(listxattrInput, ctx);
  assert.equal(listxattrBody.length, 8);
  assert.deepEqual(fuse.decodeListxattrIn(listxattrBody, ctx), listxattrInput);
  const removexattrBody = fuse.encodeRemovexattrIn(removexattrInput, ctx);
  assert.equal(removexattrBody.length, Buffer.byteLength(removexattrInput.name) + 1);
  assert.deepEqual(fuse.decodeRemovexattrIn(removexattrBody, ctx), removexattrInput);
}

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
  syncfs: {
    padding: 0x0102030405060708n,
  },
};
const lifecycleCases = [
  ["RELEASE", fuse.FUSE_RELEASE, lifecycleInput.release, fuse.decodeReleaseIn, fuse.encodeReleaseIn, 24],
  ["RELEASEDIR", fuse.FUSE_RELEASEDIR, lifecycleInput.release, fuse.decodeReleaseIn, fuse.encodeReleaseIn, 24],
  ["FLUSH", fuse.FUSE_FLUSH, lifecycleInput.flush, fuse.decodeFlushIn, fuse.encodeFlushIn, 24],
  ["FSYNC", fuse.FUSE_FSYNC, lifecycleInput.fsync, fuse.decodeFsyncIn, fuse.encodeFsyncIn, 16],
  ["FSYNCDIR", fuse.FUSE_FSYNCDIR, lifecycleInput.fsync, fuse.decodeFsyncIn, fuse.encodeFsyncIn, 16],
  ["SYNCFS", fuse.FUSE_SYNCFS, lifecycleInput.syncfs, fuse.decodeSyncfsIn, fuse.encodeSyncfsIn, 8],
];
for (const [name, , input, decodeIn, encodeIn, expectedLength] of lifecycleCases) {
  const body = encodeIn(input);
  assert.equal(body.length, expectedLength, `${name} request length`);
  assert.deepEqual(decodeIn(body), input, `${name} request round trip`);
}
const syncfsBody = fuse.encodeSyncfsIn(lifecycleInput.syncfs);
assert.deepEqual([...syncfsBody], [8, 7, 6, 5, 4, 3, 2, 1]);
assert.throws(() => fuse.decodeSyncfsIn(syncfsBody.subarray(0, 7)), fuse.ProtocolError);
assert.throws(
  () => fuse.decodeSyncfsIn(Buffer.concat([syncfsBody, Buffer.from([0])])),
  fuse.ProtocolError,
);
assert.equal(fuse.encodeReply(99n).length, fuse.FUSE_OUT_HEADER_SIZE);

const source = process.env.MOUNTX_SOURCE;
if (source) {
  const oracle = await import(pathToFileURL(`${source}/src/fuse/protocol.ts`).href);
  const oracleIoctlIsTyped =
    typeof oracle.encodeRequestBody === "function" &&
    !Array.from(oracle.UNIMPLEMENTED_OPCODES ?? []).includes(fuse.FUSE_IOCTL);
  const oracleSyncfsIsUnsupported = Array.from(oracle.UNIMPLEMENTED_OPCODES ?? []).includes(
    fuse.FUSE_SYNCFS,
  );

  const typedRequestCases = [
    ["SYMLINK", fuse.FUSE_SYMLINK, symlinkInput, undefined, fuse.encodeSymlinkIn, fuse.decodeSymlinkIn],
    ["MKNOD", fuse.FUSE_MKNOD, mknodInput, lookupContext, fuse.encodeMknodIn, fuse.decodeMknodIn],
    ["MKDIR", fuse.FUSE_MKDIR, mkdirInput, undefined, fuse.encodeMkdirIn, fuse.decodeMkdirIn],
    ["UNLINK", fuse.FUSE_UNLINK, { name: "unlink-entry" }, undefined, fuse.encodeUnlinkIn, fuse.decodeUnlinkIn],
    ["RMDIR", fuse.FUSE_RMDIR, { name: "rmdir-entry" }, undefined, fuse.encodeRmdirIn, fuse.decodeRmdirIn],
    ["RENAME", fuse.FUSE_RENAME, renameInput, undefined, fuse.encodeRenameIn, fuse.decodeRenameIn],
    ["RENAME2", fuse.FUSE_RENAME2, rename2Input, undefined, fuse.encodeRename2In, fuse.decodeRename2In],
    ["LINK", fuse.FUSE_LINK, linkInput, undefined, fuse.encodeLinkIn, fuse.decodeLinkIn],
    ["ACCESS", fuse.FUSE_ACCESS, accessInput, undefined, fuse.encodeAccessIn, fuse.decodeAccessIn],
    ["FALLOCATE", fuse.FUSE_FALLOCATE, fallocateInput, undefined, fuse.encodeFallocateIn, fuse.decodeFallocateIn],
    ["LSEEK", fuse.FUSE_LSEEK, lseekInput, undefined, fuse.encodeLseekIn, fuse.decodeLseekIn],
    ["GETLK", fuse.FUSE_GETLK, lkInput, undefined, fuse.encodeLkIn, fuse.decodeLkIn],
    ["SETLK", fuse.FUSE_SETLK, lkInput, undefined, fuse.encodeLkIn, fuse.decodeLkIn],
    ["SETLKW", fuse.FUSE_SETLKW, lkInput, undefined, fuse.encodeLkIn, fuse.decodeLkIn],
  ];
  for (const [name, opcode, value, ctx, encode, decode] of typedRequestCases) {
    const actual = ctx === undefined ? encode(value) : encode(value, ctx);
    const expected = oracle.encodeRequestBody(opcode, value, ctx);
    assert.deepEqual([...actual], [...expected], `${name} request bytes match oracle`);
    const decoded = ctx === undefined ? decode(actual) : decode(actual, ctx);
    assert.deepEqual(decoded, oracle.decodeRequestBody(opcode, expected, ctx), `${name} request decode matches oracle`);
  }

  for (const [name, opcode, value, ctx, encode, decode] of [
    ["SYMLINK", fuse.FUSE_SYMLINK, plusEntry, lookupContext, fuse.encodeSymlinkOut, fuse.decodeSymlinkOut],
    ["MKNOD", fuse.FUSE_MKNOD, plusEntry, lookupContext, fuse.encodeMknodOut, fuse.decodeMknodOut],
    ["MKDIR", fuse.FUSE_MKDIR, plusEntry, lookupContext, fuse.encodeMkdirOut, fuse.decodeMkdirOut],
    ["LINK", fuse.FUSE_LINK, plusEntry, lookupContext, fuse.encodeLinkOut, fuse.decodeLinkOut],
    ["LSEEK", fuse.FUSE_LSEEK, lseekReply, undefined, fuse.encodeLseekOut, fuse.decodeLseekOut],
    ["GETLK", fuse.FUSE_GETLK, lkReply, undefined, fuse.encodeLkOut, fuse.decodeLkOut],
  ]) {
    const actual = ctx === undefined ? encode(value) : encode(value, ctx);
    const expected = oracle.encodeReplyBody(opcode, value, ctx);
    assert.deepEqual([...actual], [...expected], `${name} reply bytes match oracle`);
    const decoded = ctx === undefined ? decode(actual) : decode(actual, ctx);
    assert.deepEqual(decoded, oracle.decodeReplyBody(opcode, expected, ctx), `${name} reply decode matches oracle`);
  }

  for (const ctx of ioctlContexts) {
    const expectedRequest = encodeIoctlInOracle(ioctlInput);
    const expectedReply = encodeIoctlOutOracle(ioctlReplyValue);
    assert.deepEqual(
      [...ioctlBody],
      [...expectedRequest],
      `IOCTL request bytes match the pinned FUSE oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      [...ioctlReplyBody],
      [...expectedReply],
      `IOCTL reply bytes match the pinned FUSE oracle for minor ${ctx.minor}`,
    );
    const ioctlRequest = Buffer.concat([
      fuse.encodeInHeader({
        len: fuse.FUSE_IN_HEADER_SIZE + ioctlBody.length,
        opcode: fuse.FUSE_IOCTL,
        unique: 99n,
        nodeid: fuse.FUSE_ROOT_ID,
        uid: 501,
        gid: 20,
        pid: 7,
        totalExtlen: 0,
      }),
      ioctlBody,
    ]);
    const oracleRequest = oracle.encodeRequest({
      opcode: fuse.FUSE_IOCTL,
      unique: 99n,
      nodeid: fuse.FUSE_ROOT_ID,
      uid: 501,
      gid: 20,
      pid: 7,
      payload: ioctlBody,
    }, ctx);
    assert.deepEqual(
      [...ioctlRequest],
      [...oracleRequest],
      `IOCTL request framing matches oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      [...oracle.decodeRequest(oracleRequest, ctx).payload],
      [...ioctlBody],
      `IOCTL request payload matches oracle for minor ${ctx.minor}`,
    );
    const ioctlReply = fuse.encodeReply(99n, ioctlReplyBody);
    const oracleReplyFrame = oracle.encodeReply(99n, ioctlReplyBody);
    assert.deepEqual(
      [...ioctlReply],
      [...oracleReplyFrame],
      `IOCTL reply framing matches oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      [...oracle.decodeReply(oracleReplyFrame, fuse.FUSE_IOCTL, ctx).payload],
      [...ioctlReplyBody],
      `IOCTL reply payload matches oracle for minor ${ctx.minor}`,
    );
    if (oracleIoctlIsTyped) {
      const oracleTypedRequest = oracle.encodeRequestBody(fuse.FUSE_IOCTL, ioctlInput, ctx);
      const oracleTypedReply = oracle.encodeReplyBody(fuse.FUSE_IOCTL, ioctlReplyValue, ctx);
      assert.deepEqual(
        [...ioctlBody],
        [...oracleTypedRequest],
        `IOCTL request bytes match oracle for minor ${ctx.minor}`,
      );
      assert.deepEqual(
        [...ioctlReplyBody],
        [...oracleTypedReply],
        `IOCTL reply bytes match oracle for minor ${ctx.minor}`,
      );
      assert.deepEqual(
        fuse.decodeIoctlIn(ioctlBody),
        oracle.decodeRequestBody(fuse.FUSE_IOCTL, oracleTypedRequest, ctx),
        `IOCTL request decode matches oracle for minor ${ctx.minor}`,
      );
      assert.deepEqual(
        fuse.decodeIoctlOut(ioctlReplyBody),
        oracle.decodeReplyBody(fuse.FUSE_IOCTL, oracleTypedReply, ctx),
        `IOCTL reply decode matches oracle for minor ${ctx.minor}`,
      );
      for (let length = 0; length < ioctlBody.length; length++) {
        const body = ioctlBody.subarray(0, length);
        assert.deepEqual(
          classifyProtocolError(() => fuse.decodeIoctlIn(body)),
          classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_IOCTL, body, ctx)),
          `IOCTL request truncation classification at ${length} bytes for minor ${ctx.minor}`,
        );
      }
      for (let length = 0; length < ioctlReplyBody.length; length++) {
        const body = ioctlReplyBody.subarray(0, length);
        assert.deepEqual(
          classifyProtocolError(() => fuse.decodeIoctlOut(body)),
          classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_IOCTL, body, ctx)),
          `IOCTL reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
        );
      }
      const requestTrailing = Buffer.concat([ioctlBody, Buffer.from([0])]);
      const replyTrailing = Buffer.concat([ioctlReplyBody, Buffer.from([0])]);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeIoctlIn(requestTrailing)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_IOCTL, requestTrailing, ctx)),
        `IOCTL request trailing-byte classification for minor ${ctx.minor}`,
      );
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeIoctlOut(replyTrailing)),
        classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_IOCTL, replyTrailing, ctx)),
        `IOCTL reply trailing-byte classification for minor ${ctx.minor}`,
      );
    }
  }
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

  for (const ctx of [
    { minor: 41, setxattrExt: false },
    { minor: 39, setxattrExt: false },
    { minor: 8, setxattrExt: false },
    { minor: 3, setxattrExt: false },
  ]) {
    const oracleBatchForget = oracle.encodeRequestBody(fuse.FUSE_BATCH_FORGET, batchForgetValue, ctx);
    assert.deepEqual(
      [...batchForgetBody],
      [...oracleBatchForget],
      `BATCH_FORGET request bytes match oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      fuse.decodeBatchForgetIn(batchForgetBody),
      oracle.decodeRequestBody(fuse.FUSE_BATCH_FORGET, oracleBatchForget, ctx),
      `BATCH_FORGET request decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < batchForgetBody.length; length++) {
      const body = batchForgetBody.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeBatchForgetIn(body)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_BATCH_FORGET, body, ctx)),
        `BATCH_FORGET request truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const batchTrailing = Buffer.concat([batchForgetBody, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeBatchForgetIn(batchTrailing)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_BATCH_FORGET, batchTrailing, ctx)),
      `BATCH_FORGET request trailing-byte classification for minor ${ctx.minor}`,
    );
    const malformedCount = Buffer.from(batchForgetBody);
    malformedCount.writeUInt32LE(batchForgetValue.forgets.length + 1, 0);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeBatchForgetIn(malformedCount)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_BATCH_FORGET, malformedCount, ctx)),
      `BATCH_FORGET count mismatch classification for minor ${ctx.minor}`,
    );

    const oracleInterrupt = oracle.encodeRequestBody(fuse.FUSE_INTERRUPT, interruptValue, ctx);
    assert.deepEqual(
      [...interruptBody],
      [...oracleInterrupt],
      `INTERRUPT request bytes match oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      fuse.decodeInterruptIn(interruptBody),
      oracle.decodeRequestBody(fuse.FUSE_INTERRUPT, oracleInterrupt, ctx),
      `INTERRUPT request decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < interruptBody.length; length++) {
      const body = interruptBody.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeInterruptIn(body)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_INTERRUPT, body, ctx)),
        `INTERRUPT request truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const interruptTrailing = Buffer.concat([interruptBody, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeInterruptIn(interruptTrailing)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_INTERRUPT, interruptTrailing, ctx)),
      `INTERRUPT request trailing-byte classification for minor ${ctx.minor}`,
    );

    for (const [name, opcode] of [
      ["BATCH_FORGET", fuse.FUSE_BATCH_FORGET],
      ["INTERRUPT", fuse.FUSE_INTERRUPT],
    ]) {
      const actualReply = fuse.encodeReply(99n);
      const oracleReply = oracle.encodeReplyFor(99n, opcode, {}, ctx);
      assert.deepEqual([...actualReply], [...oracleReply], `${name} empty reply framing matches oracle`);
      assert.deepEqual(
        oracle.decodeReplyBody(opcode, actualReply.subarray(fuse.FUSE_OUT_HEADER_SIZE), ctx),
        {},
        `${name} oracle accepts empty reply body`,
      );
    }
  }

  for (const ctx of pollContexts) {
    const oracleRequest = oracle.encodeRequestBody(fuse.FUSE_POLL, pollInput, ctx);
    const actualRequest = fuse.encodePollIn(pollInput, ctx);
    assert.deepEqual(
      [...actualRequest],
      [...oracleRequest],
      `POLL request bytes match oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      fuse.decodePollIn(actualRequest, ctx),
      oracle.decodeRequestBody(fuse.FUSE_POLL, oracleRequest, ctx),
      `POLL request decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualRequest.length; length++) {
      const body = actualRequest.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodePollIn(body, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_POLL, body, ctx)),
        `POLL request truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodePollIn(requestTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_POLL, requestTrailing, ctx)),
      `POLL request trailing-byte classification for minor ${ctx.minor}`,
    );

    const oracleReply = oracle.encodeReplyBody(fuse.FUSE_POLL, pollReplyValue, ctx);
    const actualReply = fuse.encodePollOut(pollReplyValue, ctx);
    assert.deepEqual(
      [...actualReply],
      [...oracleReply],
      `POLL reply bytes match oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      fuse.decodePollOut(actualReply, ctx),
      oracle.decodeReplyBody(fuse.FUSE_POLL, oracleReply, ctx),
      `POLL typed reply decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualReply.length; length++) {
      const body = actualReply.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodePollOut(body, ctx)),
        classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_POLL, body, ctx)),
        `POLL reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const replyTrailing = Buffer.concat([actualReply, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodePollOut(replyTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_POLL, replyTrailing, ctx)),
      `POLL reply trailing-byte classification for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodePollOut(Buffer.alloc(0), ctx)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_POLL, Buffer.alloc(0), ctx)),
      `POLL empty success-body classification for minor ${ctx.minor}`,
    );

    const actualError = fuse.encodeErrorReply(99n, "EIO");
    const oracleError = oracle.encodeErrorReply(99n, "EIO");
    assert.deepEqual(
      [...actualError],
      [...oracleError],
      `POLL error reply framing matches oracle for minor ${ctx.minor}`,
    );
    const decodedError = oracle.decodeReply(actualError, fuse.FUSE_POLL, ctx);
    assert.equal(decodedError.header.error, -5, `POLL error errno for minor ${ctx.minor}`);
    assert.equal(decodedError.body, undefined, `POLL error body is empty for minor ${ctx.minor}`);
  }

  for (const ctx of bmapContexts) {
    const oracleRequest = oracle.encodeRequestBody(fuse.FUSE_BMAP, bmapInput, ctx);
    const actualRequest = fuse.encodeBmapIn(bmapInput, ctx);
    assert.deepEqual(
      [...actualRequest],
      [...oracleRequest],
      `BMAP request bytes match oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      fuse.decodeBmapIn(actualRequest, ctx),
      oracle.decodeRequestBody(fuse.FUSE_BMAP, oracleRequest, ctx),
      `BMAP request decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualRequest.length; length++) {
      const body = actualRequest.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeBmapIn(body, ctx)),
        classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_BMAP, body, ctx)),
        `BMAP request truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeBmapIn(requestTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_BMAP, requestTrailing, ctx)),
      `BMAP request trailing-byte classification for minor ${ctx.minor}`,
    );

    const oracleReply = oracle.encodeReplyBody(fuse.FUSE_BMAP, bmapReplyValue, ctx);
    const actualReply = fuse.encodeBmapOut(bmapReplyValue, ctx);
    assert.deepEqual(
      [...actualReply],
      [...oracleReply],
      `BMAP reply bytes match oracle for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      fuse.decodeBmapOut(actualReply, ctx),
      oracle.decodeReplyBody(fuse.FUSE_BMAP, oracleReply, ctx),
      `BMAP reply decode matches oracle for minor ${ctx.minor}`,
    );
    for (let length = 0; length < actualReply.length; length++) {
      const body = actualReply.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeBmapOut(body, ctx)),
        classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_BMAP, body, ctx)),
        `BMAP reply truncation classification at ${length} bytes for minor ${ctx.minor}`,
      );
    }
    const replyTrailing = Buffer.concat([actualReply, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeBmapOut(replyTrailing, ctx)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_BMAP, replyTrailing, ctx)),
      `BMAP reply trailing-byte classification for minor ${ctx.minor}`,
    );

    // A request body and a reply body have different fixed wire shapes. Keep
    // both wrong-shape directions pinned to the oracle's protocol error.
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeBmapIn(actualReply, ctx)),
      classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_BMAP, actualReply, ctx)),
      `BMAP request wrong-shape classification for minor ${ctx.minor}`,
    );
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeBmapOut(actualRequest, ctx)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_BMAP, actualRequest, ctx)),
      `BMAP reply wrong-shape classification for minor ${ctx.minor}`,
    );
  }

  const lkContext = { minor: 41, setxattrExt: false };
  for (const [name, opcode] of [
    ["GETLK", fuse.FUSE_GETLK],
    ["SETLK", fuse.FUSE_SETLK],
    ["SETLKW", fuse.FUSE_SETLKW],
  ]) {
    const oracleRequest = oracle.encodeRequestBody(opcode, lkInput, lkContext);
    const actualRequest = fuse.encodeLkIn(lkInput);
    assert.deepEqual([...actualRequest], [...oracleRequest], `${name} request bytes match oracle`);
    assert.deepEqual(
      fuse.decodeLkIn(actualRequest),
      oracle.decodeRequestBody(opcode, oracleRequest, lkContext),
      `${name} request decode matches oracle`,
    );
    for (let length = 0; length < actualRequest.length; length++) {
      const body = actualRequest.subarray(0, length);
      assert.deepEqual(
        classifyProtocolError(() => fuse.decodeLkIn(body)),
        classifyProtocolError(() => oracle.decodeRequestBody(opcode, body, lkContext)),
        `${name} request truncation classification at ${length} bytes`,
      );
    }
    const requestTrailing = Buffer.concat([actualRequest, Buffer.from([0])]);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeLkIn(requestTrailing)),
      classifyProtocolError(() => oracle.decodeRequestBody(opcode, requestTrailing, lkContext)),
      `${name} request trailing-byte classification`,
    );

    if (opcode !== fuse.FUSE_GETLK) {
      const actualReply = fuse.encodeReply(99n);
      const oracleReply = oracle.encodeReplyFor(99n, opcode, {}, lkContext);
      assert.deepEqual([...actualReply], [...oracleReply], `${name} empty reply framing matches oracle`);
      assert.deepEqual(
        oracle.decodeReplyBody(opcode, actualReply.subarray(fuse.FUSE_OUT_HEADER_SIZE), lkContext),
        {},
        `${name} oracle accepts empty reply body`,
      );
    }
  }

  const oracleLkReply = oracle.encodeReplyBody(fuse.FUSE_GETLK, lkReply, lkContext);
  const actualLkReply = fuse.encodeLkOut(lkReply);
  assert.deepEqual([...actualLkReply], [...oracleLkReply], "GETLK reply bytes match oracle");
  assert.deepEqual(
    fuse.decodeLkOut(actualLkReply),
    oracle.decodeReplyBody(fuse.FUSE_GETLK, oracleLkReply, lkContext),
    "GETLK reply decode matches oracle",
  );
  for (let length = 0; length < actualLkReply.length; length++) {
    const body = actualLkReply.subarray(0, length);
    assert.deepEqual(
      classifyProtocolError(() => fuse.decodeLkOut(body)),
      classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_GETLK, body, lkContext)),
      `GETLK reply truncation classification at ${length} bytes`,
    );
  }
  const lkReplyTrailing = Buffer.concat([actualLkReply, Buffer.from([0])]);
  assert.deepEqual(
    classifyProtocolError(() => fuse.decodeLkOut(lkReplyTrailing)),
    classifyProtocolError(() => oracle.decodeReplyBody(fuse.FUSE_GETLK, lkReplyTrailing, lkContext)),
    "GETLK reply trailing-byte classification",
  );

  const xattrRequestCases = [
    ["SETXATTR", fuse.FUSE_SETXATTR, setxattrInput, fuse.decodeSetxattrIn, fuse.encodeSetxattrIn, (value) => ({ ...value, value: [...value.value] })],
    ["GETXATTR", fuse.FUSE_GETXATTR, getxattrInput, fuse.decodeGetxattrIn, fuse.encodeGetxattrIn, (value) => value],
    ["LISTXATTR", fuse.FUSE_LISTXATTR, listxattrInput, fuse.decodeListxattrIn, fuse.encodeListxattrIn, (value) => value],
    ["REMOVEXATTR", fuse.FUSE_REMOVEXATTR, removexattrInput, fuse.decodeRemovexattrIn, fuse.encodeRemovexattrIn, (value) => value],
  ];
  for (const ctx of xattrContexts) {
    for (const [name, opcode, input, decodeIn, encodeIn, normalize] of xattrRequestCases) {
      const oracleRequest = oracle.encodeRequestBody(opcode, input, ctx);
      const actualRequest = encodeIn(input, ctx);
      assert.deepEqual([...actualRequest], [...oracleRequest], `${name} request bytes match oracle for minor ${ctx.minor}, extension ${ctx.setxattrExt}`);
      assert.deepEqual(normalize(decodeIn(actualRequest, ctx)), normalize(oracle.decodeRequestBody(opcode, oracleRequest, ctx)), `${name} request decode matches oracle for minor ${ctx.minor}, extension ${ctx.setxattrExt}`);
      for (let length = 0; length < actualRequest.length; length++) {
        const body = actualRequest.subarray(0, length);
        assert.deepEqual(classifyProtocolError(() => decodeIn(body, ctx)), classifyProtocolError(() => oracle.decodeRequestBody(opcode, body, ctx)), `${name} truncation classification at ${length} bytes for minor ${ctx.minor}, extension ${ctx.setxattrExt}`);
      }
      const trailing = Buffer.concat([actualRequest, Buffer.from([0])]);
      assert.deepEqual(classifyProtocolError(() => decodeIn(trailing, ctx)), classifyProtocolError(() => oracle.decodeRequestBody(opcode, trailing, ctx)), `${name} trailing-byte classification for minor ${ctx.minor}, extension ${ctx.setxattrExt}`);
      if (name !== "LISTXATTR") {
        const malformed = { ...input, name: "bad\0name" };
        assert.deepEqual(classifyProtocolError(() => encodeIn(malformed, ctx)), classifyProtocolError(() => oracle.encodeRequestBody(opcode, malformed, ctx)), `${name} embedded-NUL classification for minor ${ctx.minor}, extension ${ctx.setxattrExt}`);
      }
    }

    const malformedSize = Buffer.from(fuse.encodeSetxattrIn(setxattrInput, ctx));
    malformedSize.writeUInt32LE(setxattrInput.value.length + 1, 0);
    assert.deepEqual(classifyProtocolError(() => fuse.decodeSetxattrIn(malformedSize, ctx)), classifyProtocolError(() => oracle.decodeRequestBody(fuse.FUSE_SETXATTR, malformedSize, ctx)), `SETXATTR declared-size classification for minor ${ctx.minor}, extension ${ctx.setxattrExt}`);

    const probe = { size: 4096 };
    const oracleProbe = oracle.encodeGetxattrOut(probe);
    const actualProbe = fuse.encodeGetxattrOut(probe);
    assert.deepEqual([...actualProbe], [...oracleProbe], `GETXATTR size-probe bytes match oracle for minor ${ctx.minor}`);
    assert.deepEqual(fuse.decodeGetxattrOut(actualProbe), oracle.decodeGetxattrOut(oracleProbe), `GETXATTR size-probe decode matches oracle for minor ${ctx.minor}`);
    for (let length = 0; length < actualProbe.length; length++) {
      const body = actualProbe.subarray(0, length);
      assert.deepEqual(classifyProtocolError(() => fuse.decodeGetxattrOut(body)), classifyProtocolError(() => oracle.decodeGetxattrOut(body)), `GETXATTR size-probe truncation at ${length} bytes for minor ${ctx.minor}`);
    }
    const probeTrailing = Buffer.concat([actualProbe, Buffer.from([0])]);
    assert.deepEqual(classifyProtocolError(() => fuse.decodeGetxattrOut(probeTrailing)), classifyProtocolError(() => oracle.decodeGetxattrOut(probeTrailing)), `GETXATTR size-probe trailing-byte classification for minor ${ctx.minor}`);

    const names = ["user.mountx", "user.other"];
    const oracleNames = oracle.encodeXattrNames(names);
    const actualNames = fuse.encodeXattrNames(names);
    assert.deepEqual([...actualNames], [...oracleNames], `LISTXATTR names bytes match oracle for minor ${ctx.minor}`);
    assert.deepEqual(fuse.decodeXattrNames(actualNames), oracle.decodeXattrNames(oracleNames), `LISTXATTR names decode matches oracle for minor ${ctx.minor}`);
    for (let length = 0; length < actualNames.length; length++) {
      const body = actualNames.subarray(0, length);
      compareProtocolOutcome(() => fuse.decodeXattrNames(body), () => oracle.decodeXattrNames(body), `LISTXATTR names truncation at ${length} bytes for minor ${ctx.minor}`);
    }
    const namesTrailing = Buffer.concat([actualNames, Buffer.from("tail")]);
    compareProtocolOutcome(() => fuse.decodeXattrNames(namesTrailing), () => oracle.decodeXattrNames(namesTrailing), `LISTXATTR names trailing-byte classification for minor ${ctx.minor}`);

    const rawValue = Uint8Array.from([0, 1, 2, 253, 254, 255]);
    for (const [name, opcode] of [["GETXATTR", fuse.FUSE_GETXATTR], ["LISTXATTR", fuse.FUSE_LISTXATTR]]) {
      const oracleRaw = oracle.encodeReplyBody(opcode, { data: rawValue }, ctx);
      const actualRaw = fuse.encodeReadOut({ data: rawValue });
      assert.deepEqual([...actualRaw], [...oracleRaw], `${name} raw reply bytes match oracle for minor ${ctx.minor}`);
      assert.deepEqual([...fuse.decodeReadOut(actualRaw).data], [...oracle.decodeReplyBody(opcode, oracleRaw, ctx).data], `${name} raw reply decode matches oracle for minor ${ctx.minor}`);
    }
    for (const [name, opcode] of [["SETXATTR", fuse.FUSE_SETXATTR], ["REMOVEXATTR", fuse.FUSE_REMOVEXATTR]]) {
      const actualReply = fuse.encodeReply(99n);
      const oracleReply = oracle.encodeReplyFor(99n, opcode, {}, ctx);
      assert.deepEqual([...actualReply], [...oracleReply], `${name} empty reply framing matches oracle for minor ${ctx.minor}`);
      assert.deepEqual(oracle.decodeReplyBody(opcode, actualReply.subarray(fuse.FUSE_OUT_HEADER_SIZE), ctx), {}, `${name} empty reply decode matches oracle for minor ${ctx.minor}`);
    }
  }

  const lifecycleContext = { minor: 41, setxattrExt: false };
  for (const [name, opcode, input, decodeIn, encodeIn] of lifecycleCases.filter(
    ([, opcode]) => !Array.from(oracle.UNIMPLEMENTED_OPCODES ?? []).includes(opcode),
  )) {
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
  console.log("mount-rs N-API FUSE BATCH_FORGET request/empty-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE INTERRUPT request/empty-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE POLL request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE BMAP request/typed-reply differential: PASS (pinned oracle)");
  console.log(
    `mount-rs N-API FUSE IOCTL request/reply differential: PASS (pinned oracle${oracleIoctlIsTyped ? " typed" : " raw-layout"})`,
  );
  console.log("mount-rs N-API FUSE BMAP request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE GETLK/SETLK/SETLKW request/typed-reply differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE SETXATTR/GETXATTR/LISTXATTR/REMOVEXATTR differential: PASS (pinned oracle)");
  console.log("mount-rs N-API FUSE RELEASE/RELEASEDIR, FLUSH, FSYNC/FSYNCDIR request/status differential: PASS (pinned oracle)");
  if (oracleSyncfsIsUnsupported) {
    console.log("mount-rs N-API FUSE SYNCFS request differential: SKIP (pinned oracle unimplemented)");
  }
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
  console.log("mount-rs N-API FUSE BATCH_FORGET request/empty-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE INTERRUPT request/empty-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE POLL request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE BMAP request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE IOCTL request/reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE BMAP request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE GETLK/SETLK/SETLKW request/typed-reply differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE SETXATTR/GETXATTR/LISTXATTR/REMOVEXATTR differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE RELEASE/RELEASEDIR, FLUSH, FSYNC/FSYNCDIR request/status differential: SKIP (MOUNTX_SOURCE unset)");
}

console.log("mount-rs N-API FUSE codec subpath: PASS");
