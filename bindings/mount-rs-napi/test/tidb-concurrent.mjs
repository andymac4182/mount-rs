import assert from "node:assert/strict"
import { createChunkedDriver } from "../index.js"

assert.equal(process.env.MOUNT_RS_TIDB_NAPI, "1", "explicit TiDB N-API opt-in required")
const uri = process.env.MOUNT_RS_TIDB_URL
assert.ok(uri, "MOUNT_RS_TIDB_URL is required")
const key = `${process.env.MOUNT_RS_TIDB_TEST_VOLUME_KEY || `tidb-node-${process.pid}-${Date.now()}`}/concurrent-node`
const options = {
  metadata: { kind: "tidb", uri, key, durable: true },
  blocks: { kind: "tidb", uri, key: `${key}/blocks`, durable: true },
  concurrentWrites: true,
  chunkSize: 4096,
}
const persisted = process.env.MOUNT_RS_TIDB_EXPECT_PERSISTED === "1"
const first = await createChunkedDriver({ ...options, owner: "tidb-node-first" })
let second
try {
  if (persisted) {
    assert.deepEqual(Buffer.from(await first.readFile("/restart-sentinel")), Buffer.from("TiDB concurrent restart"))
    for (let writer = 0; writer < 2; writer++) {
      for (let index = writer === 0 ? 1 : 0; index < 20; index++) {
        assert.deepEqual(Buffer.from(await first.readFile(`/node-${writer}-${index}`)), Buffer.alloc(4096, writer * 20 + index + 1))
      }
    }
    await assert.rejects(() => first.stat("/node-0-0"), (error) => error.code === "ENOENT")
    await assert.rejects(() => first.stat("/renamed"), (error) => error.code === "ENOENT")
    console.log("TIDB_CONCURRENT_NAPI_COMPONENT_RESTART_PASS files=40 before_new_writes=true")
  }
  second = await createChunkedDriver({ ...options, owner: "tidb-node-second" })
  const started = performance.now()
  await Promise.all([first, second].map(async (fs, writer) => {
    for (let index = 0; index < 20; index++) {
      await fs.writeFile(`/node-${writer}-${index}`, Buffer.alloc(4096, writer * 20 + index + 1))
    }
  }))
  for (const fs of [first, second]) {
    for (let writer = 0; writer < 2; writer++) {
      for (let index = 0; index < 20; index++) {
        assert.deepEqual(Buffer.from(await fs.readFile(`/node-${writer}-${index}`)), Buffer.alloc(4096, writer * 20 + index + 1))
      }
    }
  }
  await first.writeFile("/restart-sentinel", Buffer.from("TiDB concurrent restart"))
  await second.rename("/node-0-0", "/renamed")
  assert.deepEqual(Buffer.from(await first.readFile("/renamed")), Buffer.alloc(4096, 1))
  await first.unlink("/renamed")
  await assert.rejects(() => second.stat("/renamed"), (error) => error.code === "ENOENT")
  await assert.rejects(() => createChunkedDriver({
    ...options, blocks: { ...options.blocks, key: `${key}/wrong-blocks` }, owner: "tidb-node-wrong-backing",
  }), (error) => error.code === "ESTALE")
  assert.deepEqual(Buffer.from(await first.readFile("/restart-sentinel")), Buffer.from("TiDB concurrent restart"))
  console.log(`TIDB_CONCURRENT_NAPI_PASS phase=${persisted ? "reopen" : "seed"} acknowledged_writes=41 cross_view_checks=80 elapsed_ms=${Math.round(performance.now() - started)}`)
} finally {
  try {
    if (second) await second.shutdown()
  } finally {
    await first.shutdown()
  }
}
const reopened = await createChunkedDriver({ ...options, owner: "tidb-node-reopen" })
try {
  assert.deepEqual(Buffer.from(await reopened.readFile("/restart-sentinel")), Buffer.from("TiDB concurrent restart"))
  for (let writer = 0; writer < 2; writer++) {
    for (let index = writer === 0 ? 1 : 0; index < 20; index++) {
      assert.deepEqual(Buffer.from(await reopened.readFile(`/node-${writer}-${index}`)), Buffer.alloc(4096, writer * 20 + index + 1))
    }
  }
  console.log("TIDB_CONCURRENT_NAPI_FRESH_REOPEN_PASS files=40")
} finally {
  await reopened.shutdown()
}
