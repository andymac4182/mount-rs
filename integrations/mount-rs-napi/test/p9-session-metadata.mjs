import assert from "node:assert/strict";
import { Duplex } from "node:stream";

import root from "../index.js";
import p9 from "../p9.cjs";

const { Filesystem, createP9Server } = root;
const { P9LockTable, encodeMessage } = p9;
const onError = () => {};
const onAssertion = () => {};

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
const sharedLocks = new P9LockTable({ maxLocksPerFile: 4 });
const server = createP9Server(Filesystem.memory(), {
  maxInFlight: 3,
  readOnly: true,
  msize: 32 * 1024,
  locks: sharedLocks,
  onError,
  onAssertion,
});
const connection = server.attach(stream, { own: false, peer: "metadata-test" });

try {
  assert.equal(server.address(), null);
  assert.equal(server.path, null);
  assert.equal(server.options.onError, undefined);
  assert.equal(server.options.onAssertion, undefined);
  assert.equal(server.options.maxInFlight, 3);
  assert.equal(server.options.readOnly, true);
  assert.equal(server.options.msize, 32 * 1024);
  assert.equal(connection.stream, stream);
  assert.equal(connection.peer, "metadata-test");
  assert.equal(connection.isClosed, false);
  assert.equal(connection.session.options.msize, 32 * 1024);
  assert.equal(connection.session.msize, undefined);
  assert.equal(connection.session.version, undefined);
  assert.equal(connection.session.options.readOnly, true);
  assert.equal(connection.session.options.onError, undefined);
  assert.equal(connection.session.options.onAssertion, undefined);
  assert.ok(server.options.locks);
  assert.ok(connection.session.options.locks);
  const sharedClient = sharedLocks.client();
  assert.equal(sharedClient.lock({
    path: "/",
    fid: 77,
    type: 1,
    start: 10n,
    length: 1n,
    procId: 18,
    clientId: "injected-table",
  }), 0);
  assert.equal(server.options.locks.at("/").length, 1);
  assert.equal(connection.session.options.locks.at("/").length, 1);
  sharedClient.releaseAll();
  assert.equal(connection.session.userFor(1), undefined);

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
    () => connection.session.userFor(1) !== undefined,
    "P9 metadata attach identity",
  );
  const user = connection.session.userFor(1)
  assert.deepEqual(user, {
    uname: "node",
    uid: undefined,
    aname: "",
  });
  assert.equal(Object.hasOwn(user, "uid"), true);
  assert.equal(connection.session.msize, 32 * 1024);
  assert.equal(connection.session.version, "9P2000.L");
  assert.equal(connection.session.locks.table.files, 0);
  assert.equal(connection.session.locks.lock({
    path: "/",
    fid: 1,
    type: 1,
    start: 0n,
    length: 1n,
    procId: 17,
    clientId: "metadata-test",
  }), 0);
  assert.equal(connection.session.locks.held, 1);
  assert.equal(connection.session.locks.table.at("/").length, 1);
  connection.session.locks.releaseFid(1);
  assert.equal(connection.session.locks.held, 0);
} finally {
  await connection.close();
  await server.close();
  stream.destroy();
}

console.log("mount-rs N-API P9 session metadata: PASS");
