import assert from "node:assert/strict"
import { once } from "node:events"
import { spawn } from "node:child_process"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { fileURLToPath } from "node:url"
import { join } from "node:path"

import { Filesystem, createNodeFsDriver, createWebdavServer } from "../index.js"

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-webdav-inflight-crash-"))
const nodeDirectory = await mkdtemp(join(root, "node-fs-"))
const sqliteDirectory = await mkdtemp(join(root, "sqlite-"))
const childPath = fileURLToPath(new URL("./webdav-inflight-crash-child.mjs", import.meta.url))
const prefix = Buffer.from("WebDAV in-flight prefix")

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

async function runCase({ label, kind, location }) {
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

    replacementFilesystem = kind === "node-fs"
      ? createNodeFsDriver(location)
      : await Filesystem.sqlite(location)
    replacementServer = createWebdavServer(replacementFilesystem, {
      host: "127.0.0.1",
      port: 0,
    })
    const get = await replacementServer.session.handleRequest(
      { method: "GET", target: "/in-flight.txt", headers: [] },
    )
    assert.equal(get.status, 200, `${label} replacement must recover the in-flight resource`)
    assert.deepEqual(get.body, prefix, `${label} replacement must recover the written prefix`)
    assert.equal(replacementServer.session.lockCount, 0)
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) child.kill("SIGKILL")
    await replacementServer?.close().catch(() => {})
    await replacementFilesystem?.shutdown().catch(() => {})
  }
}

try {
  await runCase({ label: "WebDAV NodeFs in-flight crash", kind: "node-fs", location: nodeDirectory })
  await runCase({
    label: "WebDAV SQLite in-flight crash",
    kind: "sqlite",
    location: join(sqliteDirectory, "webdav.sqlite"),
  })
} finally {
  await rm(root, { recursive: true, force: true })
}

console.log("mount-rs N-API WebDAV in-flight PUT crash/restart: PASS (NodeFs + SQLite prefix recovery)")
