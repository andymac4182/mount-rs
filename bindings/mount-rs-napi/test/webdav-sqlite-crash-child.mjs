import { Filesystem, createWebdavServer } from "../index.js"

const database = process.argv[2]
if (!database) throw new Error("WebDAV SQLite crash child requires a database path")

const filesystem = await Filesystem.sqlite(database)
const server = createWebdavServer(filesystem, {
  host: "127.0.0.1",
  port: 0,
  locks: { defaultTimeoutSeconds: 30, maxTimeoutSeconds: 60, maxLocks: 4 },
})

try {
  await server.listen()
  const payload = Buffer.from("WebDAV SQLite crash recovery bytes")
  const put = await server.session.handleRequest(
    { method: "PUT", target: "/crash-recovered.txt", headers: [] },
    payload,
  )
  if (![200, 201, 204].includes(put.status)) {
    throw new Error(`WebDAV SQLite crash child PUT failed: ${put.status}`)
  }

  const lock = await server.session.handleRequest(
    { method: "LOCK", target: "/crash-recovered.txt", headers: [] },
    Buffer.from(
      '<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope>' +
        "<D:locktype><D:write/></D:locktype></D:lockinfo>",
    ),
  )
  if (lock.status !== 200 || server.session.lockCount !== 1) {
    throw new Error(`WebDAV SQLite crash child LOCK failed: ${lock.status}`)
  }

  process.stdout.write("READY\n")
  await new Promise(() => {})
} finally {
  await server.close().catch(() => {})
  await filesystem.shutdown().catch(() => {})
}
