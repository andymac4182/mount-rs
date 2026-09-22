import { Filesystem, createNodeFsDriver, createWebdavServer } from "../index.js"

const kind = process.argv[2]
const location = process.argv[3]
if (!kind || !location || !["node-fs", "sqlite"].includes(kind)) {
  throw new Error("WebDAV in-flight crash child requires node-fs/sqlite and a provider path")
}

const filesystem = kind === "node-fs"
  ? createNodeFsDriver(location)
  : await Filesystem.sqlite(location)
const server = createWebdavServer(filesystem)
const prefix = Buffer.from("WebDAV in-flight prefix")

async function waitForReadback() {
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    try {
      const observed = Buffer.from(await filesystem.readFile("/in-flight.txt"))
      if (observed.equals(prefix)) return
    } catch {
      // The provider may expose the write only after the current body chunk
      // has been committed; keep polling until that becomes observable.
    }
    await new Promise((resolve) => setTimeout(resolve, 10))
  }
  throw new Error("WebDAV in-flight crash child could not read back its written prefix")
}

async function* body() {
  yield prefix
  await waitForReadback()
  process.stdout.write("WRITTEN\n")
  await new Promise(() => {})
}

try {
  await server.session.handleRequestStream(
    { method: "PUT", target: "/in-flight.txt", headers: [] },
    body(),
  )
} finally {
  await server.close().catch(() => {})
  await filesystem.shutdown().catch(() => {})
}
