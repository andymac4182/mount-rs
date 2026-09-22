import assert from "node:assert/strict"
import { once } from "node:events"
import { spawn } from "node:child_process"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { fileURLToPath } from "node:url"
import { join } from "node:path"

import { Filesystem, createNodeFsDriver, createS3Server } from "../index.js"

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-s3-inflight-crash-"))
const nodeDirectory = await mkdtemp(join(root, "node-fs-"))
const sqliteDirectory = await mkdtemp(join(root, "sqlite-"))
const childPath = fileURLToPath(new URL("./s3-inflight-crash-child.mjs", import.meta.url))

async function waitForWritten(process, label) {
  return new Promise((resolve, reject) => {
    let stdout = ""
    let stderr = ""
    const timer = setTimeout(() => {
      reject(new Error(`${label} child did not report the written prefix: ${stderr}`))
    }, 10_000)
    process.stdout.setEncoding("utf8")
    process.stderr.setEncoding("utf8")
    const onStdout = (chunk) => {
      stdout += chunk
      if (!stdout.split("\n").includes("WRITTEN")) return
      clearTimeout(timer)
      process.stdout.off("data", onStdout)
      resolve()
    }
    process.stdout.on("data", onStdout)
    process.stderr.on("data", (chunk) => { stderr += chunk })
    process.once("error", (error) => {
      clearTimeout(timer)
      reject(error)
    })
    process.once("exit", (code, signal) => {
      clearTimeout(timer)
      reject(new Error(`${label} child exited before WRITTEN (${code ?? signal}): ${stderr}`))
    })
  })
}

function bufferedS3(session, method, target, body = Buffer.alloc(0)) {
  const headers = body.length === 0
    ? []
    : [{ name: "content-length", value: String(body.length) }]
  return session.handleRequest({ method, target, headers }, body)
}

async function drainResponse(response) {
  if (!response.body) return
  for await (const _chunk of response.body) {}
}

async function runCase({ label, kind, location, open }) {
  let child
  let replacementFilesystem
  let replacementServer
  try {
    child = spawn(process.execPath, [childPath, kind, location], {
      cwd: fileURLToPath(new URL("..", import.meta.url)),
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    })
    const exited = once(child, "exit")
    await waitForWritten(child, label)
    assert.equal(child.kill("SIGKILL"), true, `${label} child must be killable`)
    const [code, signal] = await exited
    assert.ok(
      signal === "SIGKILL" || (process.platform === "win32" && code !== 0),
      `${label} child must exit from forced termination (code=${code}, signal=${signal})`,
    )
    child = undefined

    replacementFilesystem = await open()
    let replacementNow = Date.now()
    replacementServer = createS3Server({ buckets: { photos: replacementFilesystem } }, {
      debug: true,
      now: () => replacementNow,
    })
    const stagingTtlMs = replacementServer.session.options.multipartStagingTtlMs
    assert.ok(stagingTtlMs > 0, `${label} staging TTL must be positive`)

    const missing = await bufferedS3(
      replacementServer.session,
      "GET",
      "/photos/in-flight.txt",
    )
    assert.equal(missing.status, 404, `${label} partial destination must stay unpublished`)

    replacementNow = Date.now() + stagingTtlMs + 1
    const recoveredBody = Buffer.from("S3 crash replacement bytes")
    const recovered = await replacementServer.session.handleRequestStream(
      {
        method: "PUT",
        target: "/photos/recovered.txt",
        headers: [{ name: "content-length", value: String(recoveredBody.length) }],
      },
      (async function* () {
        yield recoveredBody
      })(),
    )
    assert.equal(recovered.status, 200, `${label} replacement PUT status`)
    await drainResponse(recovered)

    const object = await bufferedS3(
      replacementServer.session,
      "GET",
      "/photos/recovered.txt",
    )
    assert.equal(object.status, 200, `${label} replacement GET status`)
    assert.deepEqual(object.body, recoveredBody, `${label} replacement bytes`)

    const entries = await replacementFilesystem.readdir("/", { withFileTypes: true })
    assert.equal(
      entries.some((entry) => entry.name.startsWith(".mountx-put-")),
      false,
      `${label} orphaned streaming staging must be reaped after the effective TTL`,
    )
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) child.kill("SIGKILL")
    await replacementServer?.close().catch(() => {})
    await replacementFilesystem?.shutdown().catch(() => {})
  }
}

try {
  await runCase({
    label: "S3 NodeFs in-flight crash",
    kind: "node-fs",
    location: nodeDirectory,
    open: async () => createNodeFsDriver(nodeDirectory),
  })
  await runCase({
    label: "S3 SQLite in-flight crash",
    kind: "sqlite",
    location: join(sqliteDirectory, "s3.sqlite"),
    open: async () => Filesystem.sqlite(join(sqliteDirectory, "s3.sqlite")),
  })
} finally {
  await rm(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 50 })
}

console.log("mount-rs N-API S3 in-flight PUT crash/restart: PASS (NodeFs + SQLite)")
