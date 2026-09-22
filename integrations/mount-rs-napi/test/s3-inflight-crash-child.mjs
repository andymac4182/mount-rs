import { Filesystem, createNodeFsDriver, createS3Server } from "../index.js"

const kind = process.argv[2]
const location = process.argv[3]
if (!kind || !location || !["node-fs", "sqlite"].includes(kind)) {
  throw new Error("S3 in-flight crash child requires node-fs/sqlite and a provider path")
}

const filesystem = kind === "node-fs"
  ? createNodeFsDriver(location)
  : await Filesystem.sqlite(location)
const server = createS3Server({ buckets: { photos: filesystem } }, {
  debug: true,
})
const prefix = Buffer.from("S3 in-flight prefix")

async function* body() {
  yield prefix
  process.stdout.write("WRITTEN\n")
  await new Promise(() => {})
}

try {
  await server.session.handleRequestStream(
    {
      method: "PUT",
      target: "/photos/in-flight.txt",
      headers: [{ name: "content-length", value: String(prefix.length) }],
    },
    body(),
  )
} finally {
  await server.close().catch(() => {})
  await filesystem.shutdown().catch(() => {})
}
