import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { Filesystem, createNodeFsDriver, createWebdavServer } from "../index.js"

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-webdav-provider-network-"))
const nodeDirectory = await mkdtemp(join(root, "node-fs-"))
const sqliteDirectory = await mkdtemp(join(root, "sqlite-"))
const concurrency = Number(process.env.MOUNT_RS_WEBDAV_PROVIDER_NETWORK_CONCURRENCY ?? 32)
// Keep the default tight; a temporary hosted-runner override helps separate
// platform slowness from a provider phase failure without weakening normal CI.
const requestTimeoutMs = Number(process.env.MOUNT_RS_WEBDAV_PROVIDER_NETWORK_TIMEOUT_MS ?? 10_000)
assert.ok(Number.isInteger(concurrency) && concurrency >= 1 && concurrency <= 64)
assert.ok(Number.isInteger(requestTimeoutMs) && requestTimeoutMs >= 1_000 && requestTimeoutMs <= 60_000)
const objects = Array.from({ length: concurrency }, (_, index) =>
  Buffer.alloc(8 * 1024 + index, index % 256),
)

async function runPhase(label, operation) {
  const startedAt = Date.now()
  console.log(`${label}: start`)
  try {
    const result = await operation()
    console.log(`${label}: PASS (${Date.now() - startedAt}ms)`)
    return result
  } catch (error) {
    throw new Error(`${label}: failed after ${Date.now() - startedAt}ms`, { cause: error })
  }
}

async function request(label, url, options) {
  const startedAt = Date.now()
  try {
    return await fetch(url, { ...options, signal: AbortSignal.timeout(requestTimeoutMs) })
  } catch (error) {
    throw new Error(`${label}: request failed after ${Date.now() - startedAt}ms`, { cause: error })
  }
}

async function runCase({ label, open }) {
  const filesystem = await open()
  let server
  try {
    server = createWebdavServer(filesystem, {
      host: "127.0.0.1",
      port: 0,
      readChunkBytes: 4 * 1024,
    })
    await runPhase(`${label} listen`, () => server.listen())

    const collection = await runPhase(`${label} MKCOL`, () => request(
      `${label} MKCOL`,
      `${server.url}/provider-network`,
      { method: "MKCOL" },
    ))
    assert.ok([201, 204].includes(collection.status), `${label} MKCOL status`)
    await collection.arrayBuffer()

    const putReplies = await runPhase(`${label} concurrent PUT fan-out`, () => Promise.all(
      objects.map((body, index) => request(
        `${label} PUT ${index}`,
        `${server.url}/provider-network/${index}.bin`,
        { method: "PUT", body },
      )),
    ))
    for (const reply of putReplies) {
      assert.ok([200, 201, 204].includes(reply.status), `${label} PUT status ${reply.status}`)
      await reply.arrayBuffer()
    }

    const getReplies = await runPhase(`${label} concurrent GET fan-out`, () => Promise.all(
      objects.map(async (expected, index) => {
        const reply = await request(
          `${label} GET ${index}`,
          `${server.url}/provider-network/${index}.bin`,
          {},
        )
        return { body: Buffer.from(await reply.arrayBuffer()), expected, index, status: reply.status }
      }),
    ))
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
    const streamedPut = await runPhase(`${label} streamed PUT`, () => request(
      `${label} streamed PUT`,
      `${server.url}/provider-network/streamed.bin`,
      {
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
      },
    ))
    assert.ok([200, 201, 204].includes(streamedPut.status), `${label} streamed PUT status`)
    await streamedPut.arrayBuffer()
    const streamedGet = await runPhase(`${label} streamed GET`, () => request(
      `${label} streamed GET`,
      `${server.url}/provider-network/streamed.bin`,
      {},
    ))
    assert.equal(streamedGet.status, 200, `${label} streamed GET status`)
    assert.deepEqual(Buffer.from(await streamedGet.arrayBuffer()), streamedObject, `${label} streamed body`)
    assert.equal(server.session.stats.methods.get("PUT"), concurrency + 1, `${label} PUT count`)
    assert.equal(server.session.stats.methods.get("GET"), concurrency + 1, `${label} GET count`)
  } finally {
    await server?.close().catch(() => {})
    await filesystem.shutdown()
  }
}

try {
  await runCase({
    label: "WebDAV NodeFs provider network",
    open: async () => createNodeFsDriver(nodeDirectory),
  })
  await runCase({
    label: "WebDAV SQLite provider network",
    open: async () => Filesystem.sqlite(join(sqliteDirectory, "webdav.sqlite")),
  })
} finally {
  // Native SQLite can finish unlinking its journal/WAL sidecars in the final
  // close turn after the last HTTP response has been drained. Retry only the
  // temporary-tree cleanup so this regression does not turn a harmless late
  // directory transition into a hosted-platform failure.
  await rm(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 50 })
}

console.log(`mount-rs N-API WebDAV provider network concurrency: PASS (NodeFs + SQLite, ${concurrency} HTTP PUT/GET pairs each)`)
