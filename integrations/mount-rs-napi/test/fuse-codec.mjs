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
  console.log("mount-rs N-API FUSE READDIR body differential: PASS (pinned oracle)");
} else {
  console.log("mount-rs N-API FUSE READDIR body differential: SKIP (MOUNTX_SOURCE unset)");
}

console.log("mount-rs N-API FUSE codec subpath: PASS");
