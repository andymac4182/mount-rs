import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { Filesystem, createNodeFsDriver, createS3Server } from "../index.js"

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-s3-provider-network-"))
const nodeDirectory = await mkdtemp(join(root, "node-fs-"))
const sqliteDirectory = await mkdtemp(join(root, "sqlite-"))
const concurrency = Number(process.env.MOUNT_RS_S3_PROVIDER_NETWORK_CONCURRENCY ?? 32)
assert.ok(Number.isInteger(concurrency) && concurrency >= 1 && concurrency <= 64)
const objects = Array.from({ length: concurrency }, (_, index) =>
  Buffer.alloc(8 * 1024 + index, index % 256),
)

async function runCase({ label, open }) {
  const filesystem = await open()
  let server
  try {
    server = createS3Server({ buckets: { photos: filesystem } }, {
      host: "127.0.0.1",
      port: 0,
      readChunkBytes: 4 * 1024,
      debug: true,
    })
    await server.listen()

    const putReplies = await Promise.all(
      objects.map((body, index) =>
        fetch(`${server.url}/photos/provider-network/${index}.bin`, {
          method: "PUT",
          body,
          signal: AbortSignal.timeout(15_000),
        }),
      ),
    )
    for (const [index, reply] of putReplies.entries()) {
      assert.equal(reply.status, 200, `${label} PUT ${index} status ${reply.status}`)
      await reply.arrayBuffer()
    }

    const getReplies = await Promise.all(
      objects.map(async (expected, index) => {
        const reply = await fetch(`${server.url}/photos/provider-network/${index}.bin`, {
          signal: AbortSignal.timeout(15_000),
        })
        return {
          body: Buffer.from(await reply.arrayBuffer()),
          expected,
          index,
          status: reply.status,
        }
      }),
    )
    for (const { body, expected, index, status } of getReplies) {
      assert.equal(status, 200, `${label} GET ${index} status`)
      assert.deepEqual(body, expected, `${label} GET ${index} body`)
    }

    const streamedObject = Buffer.concat([
      Buffer.alloc(3 * 1024, 0x41),
      Buffer.alloc(5 * 1024, 0x42),
      Buffer.alloc(7 * 1024, 0x43),
    ])
    const streamedChunks = [
      streamedObject.subarray(0, 3 * 1024),
      streamedObject.subarray(3 * 1024, 8 * 1024),
      streamedObject.subarray(8 * 1024),
    ]
    const streamedPut = await fetch(`${server.url}/photos/provider-network/streamed.bin`, {
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
      headers: { "content-length": String(streamedObject.length) },
      signal: AbortSignal.timeout(15_000),
    })
    assert.equal(streamedPut.status, 200, `${label} streamed PUT status`)
    await streamedPut.arrayBuffer()

    const streamedGet = await fetch(`${server.url}/photos/provider-network/streamed.bin`, {
      signal: AbortSignal.timeout(15_000),
    })
    assert.equal(streamedGet.status, 200, `${label} streamed GET status`)
    assert.deepEqual(
      Buffer.from(await streamedGet.arrayBuffer()),
      streamedObject,
      `${label} streamed body`,
    )

    const stats = await server.session.stats()
    assert.equal(stats.errors, 0, `${label} request errors`)
    assert.equal(stats.operations.PutObject, concurrency + 1, `${label} PUT count`)
    assert.equal(stats.operations.GetObject, concurrency + 1, `${label} GET count`)
  } finally {
    await server?.close().catch(() => {})
    await filesystem.shutdown()
  }
}

try {
  await runCase({
    label: "S3 NodeFs provider network",
    open: async () => createNodeFsDriver(nodeDirectory),
  })
  await runCase({
    label: "S3 SQLite provider network",
    open: async () => Filesystem.sqlite(join(sqliteDirectory, "s3.sqlite")),
  })
} finally {
  await rm(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 50 })
}

console.log(`mount-rs N-API S3 provider network concurrency: PASS (NodeFs + SQLite, ${concurrency} HTTP PUT/GET pairs each)`)
