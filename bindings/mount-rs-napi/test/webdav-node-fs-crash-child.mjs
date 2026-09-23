import { createNodeFsDriver, createWebdavServer } from "../index.js"

const directory = process.argv[2]
if (!directory) throw new Error("WebDAV NodeFs crash child requires a directory")

const filesystem = createNodeFsDriver(directory)
const server = createWebdavServer(filesystem, {
  host: "127.0.0.1",
  port: 0,
  locks: { defaultTimeoutSeconds: 30, maxTimeoutSeconds: 60, maxLocks: 4 },
})

try {
  const payload = Buffer.from("WebDAV NodeFs crash recovery bytes")
  const put = await server.session.handleRequest(
    { method: "PUT", target: "/crash-recovered.txt", headers: [] },
    payload,
  )
  if (![200, 201, 204].includes(put.status)) {
    throw new Error(`WebDAV NodeFs crash child PUT failed: ${put.status}`)
  }

  const lock = await server.session.handleRequest(
    { method: "LOCK", target: "/crash-recovered.txt", headers: [] },
    Buffer.from(
      '<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope>' +
        "<D:locktype><D:write/></D:locktype></D:lockinfo>",
    ),
  )
  if (lock.status !== 200 || server.session.lockCount !== 1) {
    throw new Error(`WebDAV NodeFs crash child LOCK failed: ${lock.status}`)
  }

  process.stdout.write("READY\n")
  // The Rust server has no refed Node event-loop handle. Keep the child live
  // until the parent forces termination so the crash assertion cannot race
  // Node's unsettled-top-level-await exit (code 13).
  await new Promise(() => setInterval(() => {}, 60_000))
} finally {
  await server.close().catch(() => {})
  await filesystem.shutdown().catch(() => {})
}
