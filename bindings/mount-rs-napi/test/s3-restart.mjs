import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { createNodeFsDriver, createS3Server } from "../index.js"

const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-s3-restart-"))
const metadataPath = join(directory, "restart-metadata.json")
const child = String.raw`
  (async () => {
    const { writeFileSync } = require("node:fs")
    const { createNodeFsDriver, createS3Server } = require("./index.js")
    const root = process.env.MOUNT_RS_S3_RESTART_ROOT
    const metadataPath = process.env.MOUNT_RS_S3_RESTART_METADATA
    const xmlField = (body, name) => {
      const match = Buffer.from(body).toString().match(new RegExp("<" + name + ">([^<]*)</" + name + ">"))
      if (!match) throw new Error("missing S3 XML field " + name)
      return match[1]
    }
    const driver = createNodeFsDriver(root)
    const server = createS3Server(driver, { bucket: "photos", debug: true })
    const initiated = await server.session.handleRequest(
      { method: "POST", target: "/photos/restarted.bin?uploads", headers: [] },
      Buffer.alloc(0),
    )
    if (initiated.status !== 200) throw new Error("CreateMultipartUpload failed: " + initiated.status)
    const uploadId = xmlField(initiated.body, "UploadId")
    const part = await server.session.handleRequest(
      {
        method: "PUT",
        target: "/photos/restarted.bin?uploadId=" + uploadId + "&partNumber=1",
        headers: [{ name: "content-length", value: "31" }],
      },
      Buffer.from("process restart multipart bytes"),
    )
    if (part.status !== 200) throw new Error("UploadPart failed: " + part.status)
    const etag = part.headers.find(({ name }) => name === "etag")?.value
    if (!etag) throw new Error("UploadPart did not return an ETag")
    writeFileSync(metadataPath, JSON.stringify({ uploadId, etag }))
    // Simulate an abrupt process crash after the multipart state is written,
    // without giving the server a chance to run its close sweep.
    process.abort()
  })().catch((error) => {
    console.error(error)
    process.exit(1)
  })
`

let replacement
try {
  try {
    execFileSync(
      process.execPath,
      ["-e", child],
      {
        cwd: new URL("..", import.meta.url),
        env: {
          ...process.env,
          MOUNT_RS_S3_RESTART_ROOT: directory,
          MOUNT_RS_S3_RESTART_METADATA: metadataPath,
        },
        stdio: ["ignore", "ignore", "ignore"],
        timeout: 20_000,
      },
    )
    throw new Error("restart child exited cleanly instead of crashing")
  } catch (error) {
    const expectedWindowsAbort = process.platform === "win32" && error?.status !== 0
    assert.ok(
      error?.signal === "SIGABRT" || expectedWindowsAbort,
      `restart child did not abort abruptly: ${error}`,
    )
  }

  const { uploadId, etag } = JSON.parse(await readFile(metadataPath, "utf8"))
  assert.match(uploadId, /^[0-9a-f]{32}$/)
  assert.match(etag, /^"[0-9a-f-]+"$/)

  const driver = createNodeFsDriver(directory)
  replacement = createS3Server(driver, { bucket: "photos", debug: true })
  const listed = await replacement.session.handleRequest(
    { method: "GET", target: `/photos/restarted.bin?uploadId=${uploadId}`, headers: [] },
    Buffer.alloc(0),
  )
  assert.equal(listed.status, 200)
  assert.match(Buffer.from(listed.body).toString(), /<PartNumber>1<\/PartNumber>/)

  const completed = await replacement.session.handleRequest(
    {
      method: "POST",
      target: `/photos/restarted.bin?uploadId=${uploadId}`,
      headers: [{ name: "content-length", value: String(Buffer.byteLength(
        `<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>${etag}</ETag></Part></CompleteMultipartUpload>`,
      )) }],
    },
    Buffer.from(
      `<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>${etag}</ETag></Part></CompleteMultipartUpload>`,
    ),
  )
  assert.equal(completed.status, 200)

  const object = await replacement.session.handleRequest(
    { method: "GET", target: "/photos/restarted.bin", headers: [] },
    Buffer.alloc(0),
  )
  assert.equal(object.status, 200)
  assert.deepEqual(object.body, Buffer.from("process restart multipart bytes"))
} finally {
  await replacement?.close().catch(() => {})
  await rm(directory, { recursive: true, force: true })
}

console.log("mount-rs N-API S3 process-restart multipart recovery: PASS")
