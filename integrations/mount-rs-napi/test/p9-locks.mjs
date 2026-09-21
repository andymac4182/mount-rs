import assert from "node:assert/strict";

import p9 from "../p9.cjs";

const {
  P9LockTable,
  P9_LOCK_BLOCKED,
  P9_LOCK_SUCCESS,
} = p9;

const table = new P9LockTable({ maxLocksPerFile: 2 });
const first = table.client();
const second = table.client();
const request = (overrides = {}) => ({
  path: "/file",
  fid: 7,
  type: 1,
  start: 4n,
  length: 8n,
  procId: 11,
  clientId: "client-a",
  ...overrides,
});

assert.equal(table.files, 0);
assert.equal(table.size, 0);
assert.equal(first.lock(request()), P9_LOCK_SUCCESS);
assert.equal(first.held, 1);
assert.equal(table.files, 1);
assert.equal(table.size, 1);
assert.deepEqual(table.at("/file"), [{
  type: 1,
  start: 4n,
  length: 8n,
  procId: 11,
  clientId: "client-a",
  holder: first.id,
  fid: 7,
}]);

const conflict = table.getlock(request({ clientId: "client-b" }));
assert.deepEqual(conflict, {
  type: 1,
  start: 4n,
  length: 8n,
  procId: 11,
  clientId: "client-a",
});
assert.equal(second.lock(request({ clientId: "client-b" })), P9_LOCK_BLOCKED);
table.remap("/file", "/renamed");
assert.equal(table.at("/file").length, 0);
assert.equal(table.at("/renamed").length, 1);
table.release("/renamed");
assert.equal(table.size, 0);
assert.equal(first.held, 0);

console.log("mount-rs N-API P9 lock surface: PASS");
