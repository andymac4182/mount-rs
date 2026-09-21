import assert from "node:assert/strict";
import { Duplex } from "node:stream";

import root from "../index.js";
import p9 from "../p9.cjs";

const { Filesystem, createP9Server } = root;
const {
  encodeMessage,
  P9Reader,
  P9_RLERROR,
  P9_RVERSION,
  P9_TVERSION,
  readHeader,
} = p9;

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

const reports = [];
const assertions = [];
const stream = new Duplex({
  read() {},
  write(_chunk, _encoding, callback) {
    callback();
  },
});
const server = createP9Server(Filesystem.memory(), {
  debug: true,
  onError(error, header) {
    reports.push({ error, header });
  },
  onAssertion(message) {
    assertions.push(message);
  },
});
const connection = server.attach(stream, { own: false, peer: "observability-test" });

try {
  assert.equal(server.options.debug, true);
  assert.equal(connection.session.options.debug, true);
  assert.deepEqual(connection.session.assertions, []);
  assert.equal(connection.session.stats.assertions, 0);
  assert.ok(connection.session.driver instanceof Filesystem);
  assert.equal((await connection.session.driver.stat("/")).isDirectory(), true);

  assert.equal(await connection.session.handleCall(Buffer.from([1, 2, 3])), null);
  await waitUntil(() => reports.length >= 1, "malformed 9P error callback");
  assert.equal(reports[0].error.code, "EPROTO");
  assert.equal(reports[0].header, undefined);

  const version = await connection.session.handleCall(
    encodeMessage(P9_TVERSION, 0, (writer) => {
      writer.u32(32 * 1024);
      writer.string("9P2000.L");
    }),
  );
  assert.equal(readHeader(new P9Reader(version)).type, P9_RVERSION);

  const response = await connection.session.handleCall(
    encodeMessage(250, 17, () => {}),
  );
  assert.equal(readHeader(new P9Reader(response)).type, P9_RLERROR);
  await waitUntil(() => reports.length >= 2, "protocol 9P error callback");
  assert.equal(reports[1].error.code, "ENOTSUP");
  assert.deepEqual(reports[1].header, { size: 7, type: 250, tag: 17 });
  assert.equal(connection.session.stats.dropped, 1);
  assert.equal(connection.session.stats.errors, 1);
  assert.equal(connection.session.stats.assertions, 0);
  assert.deepEqual(assertions, []);
} finally {
  await connection.close();
  await server.close();
  stream.destroy();
}

console.log("mount-rs N-API P9 observability and driver surface: PASS");
