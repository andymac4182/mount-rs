import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { Filesystem, createNodeFsDriver, createWebdavServer } from "../index.js"

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-webdav-provider-concurrency-"))
const nodeDirectory = await mkdtemp(join(root, "node-fs-"))
const sqliteDirectory = await mkdtemp(join(root, "sqlite-"))
const concurrency = Number(process.env.MOUNT_RS_WEBDAV_PROVIDER_CONCURRENCY ?? 64)
assert.ok(Number.isInteger(concurrency) && concurrency >= 1 && concurrency <= 128)
const objects = Array.from({ length: concurrency }, (_, index) =>
  Buffer.alloc(16 * 1024 + index, index % 256),
)

async function runCase({ label, open }) {
  const filesystem = await open()
  let server
  try {
    server = createWebdavServer(filesystem, {
      host: "127.0.0.1",
      port: 0,
      readChunkBytes: 4 * 1024,
    })
    const collection = await server.session.handleRequest(
      { method: "MKCOL", target: "/provider-concurrent", headers: [] },
    )
    assert.ok([201, 204].includes(collection.status), `${label} MKCOL status`)

    const putReplies = await Promise.all(
      objects.map((body, index) =>
        server.session.handleRequest(
          { method: "PUT", target: `/provider-concurrent/${index}.bin`, headers: [] },
          body,
        ),
      ),
    )
    for (const reply of putReplies) {
      assert.ok([200, 201, 204].includes(reply.status), `${label} PUT status ${reply.status}`)
    }

    const getReplies = await Promise.all(
      objects.map(async (expected, index) => ({
        expected,
        index,
        reply: await server.session.handleRequest(
          { method: "GET", target: `/provider-concurrent/${index}.bin`, headers: [] },
        ),
      })),
    )
    for (const { expected, index, reply } of getReplies) {
      assert.equal(reply.status, 200, `${label} GET ${index} status`)
      assert.deepEqual(reply.body, expected, `${label} GET ${index} body`)
    }

    assert.equal(server.session.stats.methods.get("PUT"), concurrency, `${label} PUT count`)
    assert.equal(server.session.stats.methods.get("GET"), concurrency, `${label} GET count`)
  } finally {
    await server?.close().catch(() => {})
    await filesystem.shutdown()
  }
}

try {
  await runCase({
    label: "WebDAV NodeFs provider concurrency",
    open: async () => createNodeFsDriver(nodeDirectory),
  })
  await runCase({
    label: "WebDAV SQLite provider concurrency",
    open: async () => Filesystem.sqlite(join(sqliteDirectory, "webdav.sqlite")),
  })
} finally {
  await rm(root, { recursive: true, force: true })
}

console.log(`mount-rs N-API WebDAV provider concurrency: PASS (NodeFs + SQLite, ${concurrency} PUT/GET pairs each)`)
