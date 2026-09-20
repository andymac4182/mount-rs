import assert from "node:assert/strict";
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

console.log("mount-rs N-API FUSE codec subpath: PASS");
