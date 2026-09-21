import assert from "node:assert/strict"

import { Filesystem, createWebdavServer } from "../index.js"

const filesystem = Filesystem.memory()
const server = createWebdavServer(filesystem, {
  host: "127.0.0.1",
  port: 0,
  readChunkBytes: 4 * 1024,
})
const objects = Array.from({ length: 16 }, (_, index) =>
  Buffer.alloc(64 * 1024 + index, index),
)

try {
  await server.listen()
  const collection = await fetch(`${server.url}/concurrent-http`, {
    method: "MKCOL",
    signal: AbortSignal.timeout(10_000),
  })
  assert.ok([201, 204].includes(collection.status))
  await collection.arrayBuffer()

  const putReplies = await Promise.all(
    objects.map((body, index) =>
      fetch(`${server.url}/concurrent-http/${index}.bin`, {
        method: "PUT",
        body,
        signal: AbortSignal.timeout(10_000),
      }),
    ),
  )
  for (const reply of putReplies) {
    assert.ok([200, 201, 204].includes(reply.status), `concurrent PUT status ${reply.status}`)
    await reply.arrayBuffer()
  }

  const getReplies = await Promise.all(
    objects.map(async (expected, index) => {
      const reply = await fetch(`${server.url}/concurrent-http/${index}.bin`, {
        signal: AbortSignal.timeout(10_000),
      })
      return { body: Buffer.from(await reply.arrayBuffer()), expected, status: reply.status }
    }),
  )
  for (const { body, expected, status } of getReplies) {
    assert.equal(status, 200)
    assert.deepEqual(body, expected)
  }
  assert.equal(server.session.stats.methods.get("PUT"), 16)
  assert.equal(server.session.stats.methods.get("GET"), 16)
} finally {
  await server.close().catch(() => {})
  await filesystem.shutdown()
}

console.log("mount-rs N-API WebDAV network concurrency: PASS (16 concurrent HTTP PUT/GET pairs)")
