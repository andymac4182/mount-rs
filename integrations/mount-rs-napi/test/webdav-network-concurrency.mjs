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
let authServer

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

  const streamedObject = Buffer.concat([
    Buffer.alloc(7 * 1024, 0x31),
    Buffer.alloc(13 * 1024, 0x32),
    Buffer.alloc(23 * 1024, 0x33),
  ])
  const streamedChunks = [
    streamedObject.subarray(0, 7 * 1024),
    streamedObject.subarray(7 * 1024, 20 * 1024),
    streamedObject.subarray(20 * 1024),
  ]
  const streamedPut = await fetch(`${server.url}/concurrent-http/streamed.bin`, {
    method: "PUT",
    body: new ReadableStream({
      async pull(controller) {
        const chunk = streamedChunks.shift()
        if (chunk === undefined) {
          controller.close()
          return
        }
        await new Promise((resolve) => setImmediate(resolve))
        controller.enqueue(chunk)
      },
    }),
    duplex: "half",
    signal: AbortSignal.timeout(10_000),
  })
  assert.ok([200, 201, 204].includes(streamedPut.status))
  await streamedPut.arrayBuffer()
  const streamedGet = await fetch(`${server.url}/concurrent-http/streamed.bin`, {
    signal: AbortSignal.timeout(10_000),
  })
  assert.equal(streamedGet.status, 200)
  assert.deepEqual(Buffer.from(await streamedGet.arrayBuffer()), streamedObject)
  assert.equal(server.session.stats.methods.get("PUT"), 17)
  assert.equal(server.session.stats.methods.get("GET"), 17)

  authServer = createWebdavServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
    credentials: { username: "network-user", password: "network-secret" },
    realm: "network-realm",
  })
  await authServer.listen()
  const unauthorized = await fetch(`${authServer.url}/`, {
    method: "OPTIONS",
    signal: AbortSignal.timeout(10_000),
  })
  assert.equal(unauthorized.status, 401)
  assert.match(unauthorized.headers.get("www-authenticate") ?? "", /^Basic realm="network-realm"/)
  await unauthorized.arrayBuffer()

  const authorized = await fetch(`${authServer.url}/`, {
    method: "OPTIONS",
    headers: {
      authorization: `Basic ${Buffer.from("network-user:network-secret").toString("base64")}`,
    },
    signal: AbortSignal.timeout(10_000),
  })
  assert.equal(authorized.status, 200)
  await authorized.arrayBuffer()
} finally {
  await authServer?.close().catch(() => {})
  await server.close().catch(() => {})
  await filesystem.shutdown()
}

console.log("mount-rs N-API WebDAV network concurrency/auth/streaming: PASS (16 concurrent HTTP PUT/GET pairs)")
