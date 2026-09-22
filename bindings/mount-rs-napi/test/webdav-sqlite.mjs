import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { Filesystem, createWebdavServer } from "../index.js"

const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-webdav-sqlite-"))
const database = join(directory, "webdav.sqlite")
let filesystem
let replacementFilesystem
let server
let replacementServer

const lockBody = Buffer.from(
  '<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope>' +
    '<D:locktype><D:write/></D:locktype></D:lockinfo>',
)
const payload = Buffer.from("durable WebDAV SQLite bytes")

try {
  filesystem = await Filesystem.sqlite(database)
  server = createWebdavServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
    locks: { defaultTimeoutSeconds: 30, maxTimeoutSeconds: 60, maxLocks: 4 },
  })

  const put = await server.session.handleRequest(
    { method: "PUT", target: "/durable-webdav.txt", headers: [] },
    payload,
  )
  assert.ok([200, 201, 204].includes(put.status))

  const lock = await server.session.handleRequest(
    { method: "LOCK", target: "/durable-webdav.txt", headers: [] },
    lockBody,
  )
  assert.equal(lock.status, 200)
  assert.equal(server.session.lockCount, 1)

  await server.close()
  server = undefined
  await filesystem.shutdown()
  filesystem = undefined

  replacementFilesystem = await Filesystem.sqlite(database)
  replacementServer = createWebdavServer(replacementFilesystem, {
    host: "127.0.0.1",
    port: 0,
  })
  const get = await replacementServer.session.handleRequest(
    { method: "GET", target: "/durable-webdav.txt", headers: [] },
  )
  assert.equal(get.status, 200)
  assert.deepEqual(get.body, payload)
  assert.equal(replacementServer.session.lockCount, 0)

  await replacementServer.close()
  replacementServer = undefined
  await replacementFilesystem.shutdown()
  replacementFilesystem = undefined
} finally {
  await replacementServer?.close().catch(() => {})
  await server?.close().catch(() => {})
  await replacementFilesystem?.shutdown().catch(() => {})
  await filesystem?.shutdown().catch(() => {})
  await rm(directory, { recursive: true, force: true })
}

console.log("mount-rs N-API WebDAV SQLite provider/reopen: PASS")
