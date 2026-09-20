import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import * as fuse from "../fuse.cjs";

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

const source = process.env.MOUNTX_SOURCE;
if (source) {
  const oracle = await import(pathToFileURL(`${source}/src/fuse/protocol.ts`).href);
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
} else {
  console.log("mount-rs N-API FUSE READDIR body differential: SKIP (MOUNTX_SOURCE unset)");
  console.log("mount-rs N-API FUSE READDIRPLUS body differential: SKIP (MOUNTX_SOURCE unset)");
}

console.log("mount-rs N-API FUSE codec subpath: PASS");
