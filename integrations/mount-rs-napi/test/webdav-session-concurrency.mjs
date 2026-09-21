import assert from "node:assert/strict"

import { Filesystem, createWebdavServer } from "../index.js"

const filesystem = Filesystem.memory()
const server = createWebdavServer(filesystem, { readChunkBytes: 4 * 1024 })
const concurrency = Number(process.env.MOUNT_RS_WEBDAV_CONCURRENCY ?? 64)
assert.ok(Number.isInteger(concurrency) && concurrency >= 1 && concurrency <= 256)
const objects = Array.from({ length: concurrency }, (_, index) =>
  Buffer.alloc(64 * 1024 + index, index),
)

try {
  const collection = await server.session.handleRequest(
    { method: "MKCOL", target: "/direct-concurrent", headers: [] },
  )
  assert.ok([201, 204].includes(collection.status))

  const putReplies = await Promise.all(
    objects.map((body, index) =>
      server.session.handleRequest(
        { method: "PUT", target: `/direct-concurrent/${index}.bin`, headers: [] },
        body,
      ),
    ),
  )
  for (const reply of putReplies) {
    assert.ok([200, 201, 204].includes(reply.status))
  }

  const getReplies = await Promise.all(
    objects.map(async (expected, index) => {
      const reply = await server.session.handleRequest(
        { method: "GET", target: `/direct-concurrent/${index}.bin`, headers: [] },
      )
      return { body: reply.body, expected, status: reply.status }
    }),
  )
  for (const { body, expected, status } of getReplies) {
    assert.equal(status, 200)
    assert.deepEqual(body, expected)
  }

  assert.equal(server.session.stats.methods.get("PUT"), concurrency)
  assert.equal(server.session.stats.methods.get("GET"), concurrency)
} finally {
  await server.close().catch(() => {})
  await filesystem.shutdown()
}

console.log(`mount-rs N-API WebDAV direct-session concurrency: PASS (${concurrency} concurrent PUT/GET pairs)`)
