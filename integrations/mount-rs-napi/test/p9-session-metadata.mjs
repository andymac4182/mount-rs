import assert from "node:assert/strict";
import { Duplex } from "node:stream";

import root from "../index.js";
import p9 from "../p9.cjs";

const { Filesystem, createP9Server } = root;
const { encodeMessage } = p9;

function waitUntil(predicate, label) {
  const deadline = Date.now() + 5_000;
  return new Promise((resolve, reject) => {
    const poll = () => {
      if (predicate()) {
        resolve();
        return;
      }
      if (Date.now() >= deadline) {
        reject(new Error(`${label} timed out`));
        return;
      }
      setImmediate(poll);
    };
    poll();
  });
}

const stream = new Duplex({
  read() {},
  write(_chunk, _encoding, callback) {
    callback();
  },
});
const server = createP9Server(Filesystem.memory(), {
  maxInFlight: 3,
  readOnly: true,
  msize: 32 * 1024,
});
const connection = server.attach(stream, { own: false, peer: "metadata-test" });

try {
  assert.equal(server.options.maxInFlight, 3);
  assert.equal(server.options.readOnly, true);
  assert.equal(server.options.msize, 32 * 1024);
  assert.equal(connection.session.options.msize, 32 * 1024);
  assert.equal(connection.session.options.readOnly, true);
  assert.equal(connection.session.userFor(1), null);

  stream.push(encodeMessage(100, 0xffff, (writer) => {
    writer.u32(32 * 1024);
    writer.string("9P2000.L");
  }));
  await waitUntil(
    () => connection.session.stats.replies >= 1,
    "P9 metadata version reply",
  );

  stream.push(encodeMessage(104, 1, (writer) => {
    writer.u32(1);
    writer.u32(0xffff_ffff);
    writer.string("node");
    writer.string("");
    writer.u32(0xffff_ffff);
  }));
  await waitUntil(
    () => connection.session.userFor(1) !== null,
    "P9 metadata attach identity",
  );
  assert.deepEqual(connection.session.userFor(1), {
    uname: "node",
    aname: "",
  });
} finally {
  await connection.close();
  await server.close();
  stream.destroy();
}

console.log("mount-rs N-API P9 session metadata: PASS");
