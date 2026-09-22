import assert from "node:assert/strict"
import { once } from "node:events"
import { spawn } from "node:child_process"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { fileURLToPath } from "node:url"
import { join } from "node:path"

import { Filesystem, createWebdavServer } from "../index.js"

const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-webdav-sqlite-crash-"))
const database = join(directory, "webdav.sqlite")
const childPath = fileURLToPath(new URL("./webdav-sqlite-crash-child.mjs", import.meta.url))
let child
let replacementFilesystem
let replacementServer

function waitForReady(process) {
  return new Promise((resolve, reject) => {
    let stdout = ""
    let stderr = ""
    const timer = setTimeout(() => {
      reject(new Error(`WebDAV SQLite crash child did not become ready: ${stderr}`))
    }, 10_000)
    process.stdout.setEncoding("utf8")
    process.stderr.setEncoding("utf8")
    const onStdout = (chunk) => {
      stdout += chunk
      if (!stdout.split("\n").includes("READY")) return
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
      reject(new Error(`WebDAV SQLite crash child exited before READY (${code ?? signal}): ${stderr}`))
    })
  })
}

try {
  child = spawn(process.execPath, [childPath, database], {
    cwd: fileURLToPath(new URL("..", import.meta.url)),
    env: process.env,
    stdio: ["ignore", "pipe", "pipe"],
  })
  const exited = once(child, "exit")
  await waitForReady(child)
  assert.equal(child.kill("SIGKILL"), true, "the WebDAV SQLite child must be killable")
  const [code, signal] = await exited
  assert.ok(
    signal === "SIGKILL" || (process.platform === "win32" && code !== 0),
    `the WebDAV SQLite child must exit from forced termination (code=${code}, signal=${signal})`,
  )
  child = undefined

  replacementFilesystem = await Filesystem.sqlite(database)
  replacementServer = createWebdavServer(replacementFilesystem, {
    host: "127.0.0.1",
    port: 0,
  })
  await replacementServer.listen()
  const get = await replacementServer.session.handleRequest(
    { method: "GET", target: "/crash-recovered.txt", headers: [] },
  )
  assert.equal(get.status, 200)
  assert.deepEqual(get.body, Buffer.from("WebDAV SQLite crash recovery bytes"))
  assert.equal(replacementServer.session.lockCount, 0)
} finally {
  if (child && child.exitCode === null && child.signalCode === null) child.kill("SIGKILL")
  await replacementServer?.close().catch(() => {})
  await replacementFilesystem?.shutdown().catch(() => {})
  await rm(directory, { recursive: true, force: true })
}

console.log("mount-rs N-API WebDAV SQLite crash/restart: PASS")
