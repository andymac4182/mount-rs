import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { pathToFileURL, fileURLToPath } from "node:url"
import * as native from "../index.js"

const source = process.env.MOUNTX_SOURCE
assert.ok(source, "MOUNTX_SOURCE is required; this test never skips its oracle")
assert.equal(execFileSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8")
const load = (path) => import(pathToFileURL(`${source}/src/${path}.ts`).href)
const { createMemoryDriver } = await load("drivers/memory")
const { createLoopback, resolveCapabilities } = await load("harness")

const base = createMemoryDriver()
const minimal = { stat: base.stat.bind(base), readdir: base.readdir.bind(base), open: base.open.bind(base) }
for (const driver of [minimal, { ...minimal, capabilities: { readOnly: true, handles: true } }, base]) {
  const adapted = native.createDriver(driver)
  try {
    const expected = resolveCapabilities(driver)
    for (const [key, value] of Object.entries(expected)) assert.deepEqual(adapted.capabilities[key], value, key)
    if (!driver.mkdir) {
      const oracle = createLoopback(driver)
      for (const fs of [adapted, oracle]) await assert.rejects(() => fs.mkdir("/missing"), { code: "ENOSYS" })
    }
  } finally { await adapted.shutdown() }
}

// The complete existing TCP/HTTP exercises now receive the oracle's plain
// structural driver directly, including NFS RPC and P9 file reads.
await new Promise((resolve, reject) => {
  const child = spawn(process.execPath, [fileURLToPath(new URL("servers.mjs", import.meta.url))], {
    env: { ...process.env, MOUNT_RS_STRUCTURAL_SERVERS: "1" }, stdio: "inherit", timeout: 30_000,
  })
  child.on("error", reject)
  child.on("exit", (code, signal) => code === 0 ? resolve() : reject(new Error(`structural servers: ${code}/${signal}`)))
})

for (const [name, path, prefix] of [
  ["createWebdavServer", "webdav/server", ""],
  ["createS3Server", "s3/server", "/mountx"],
]) {
  const oracle = await load(path)
  const results = []
  for (const factory of [native[name], oracle[name]]) {
    const backing = createMemoryDriver()
    const view = createLoopback(backing)
    await view.writeFile("/data", "structural data")
    let shutdowns = 0
    const driver = { stat: backing.stat.bind(backing), readdir: backing.readdir.bind(backing), open: backing.open.bind(backing),
      shutdown() { shutdowns++ } }
    const server = factory(driver, { host: "127.0.0.1", port: 0 })
    try {
      await server.listen()
      const get = await fetch(`${server.url}${prefix}/data`, { signal: AbortSignal.timeout(5000) })
      const body = await get.text()
      const deletion = await fetch(`${server.url}${prefix}/data`, { method: "DELETE", signal: AbortSignal.timeout(5000) })
      await deletion.arrayBuffer()
      results.push([get.status, body, deletion.status])
    } finally { await Promise.all([server.close(), server.close()]) }
    assert.equal(shutdowns, 0, "server does not own caller driver shutdown")
    assert.equal((await driver.stat("/data")).size, 15)
  }
  assert.deepEqual(results[0], results[1], `${name} minimal driver wire behavior`)
}

const { createWebdavServer: oracleWebdav } = await load("webdav/server")
for (const [scenario, expectedStatus] of [
  ["missing", 501], ["ENOSYS", 501], ["EACCES", 403], ["EIO", 500],
  ["ENOENT", 404], ["success", 204], ["partial", 207], ["collection-missing", 207],
]) {
  const results = []
  const collection = scenario === "partial" || scenario === "collection-missing"
  for (const factory of [native.createWebdavServer, oracleWebdav]) {
    const structuralFactory = factory === native.createWebdavServer
    const driver = createMemoryDriver()
    const view = createLoopback(driver)
    if (collection) await view.mkdir("/tree")
    const target = collection ? "/tree/blocked" : "/data"
    await view.writeFile(target, "preserve")
    if (collection) await view.writeFile("/tree/removable", "remove")
    const unlink = driver.unlink.bind(driver)
    if (scenario === "missing" || scenario === "collection-missing") {
      delete driver.unlink
    } else if (scenario !== "success") {
      driver.unlink = async (path) => {
        if (scenario === "partial" && path !== target) return unlink(path)
        throw Object.assign(new Error("injected unlink failure"), {
          code: scenario === "partial" ? "EACCES" : scenario,
          syscall: "unlink", path,
        })
      }
    }
    const server = factory(driver, { host: "127.0.0.1", port: 0 })
    try {
      await server.listen()
      const response = await fetch(`${server.url}${collection ? "/tree" : target}`, {
        method: "DELETE", signal: AbortSignal.timeout(5000),
      })
      const body = await response.text()
      assert.equal(response.status, expectedStatus, scenario)
      const exists = async (path) => driver.stat(path).then(() => true, (error) => {
        if (error.code === "ENOENT") return false
        throw error
      })
      const survivors = [await exists(target)]
      if (collection) {
        survivors.push(await exists("/tree/removable"), await exists("/tree"))
        assert.match(body, /multistatus/)
        if (structuralFactory) {
          // The N-API structural driver exposes only unbounded `readdir`.
          // Recursive WebDAV mutations therefore fail closed at the
          // collection boundary instead of materializing an unbounded tree.
          assert.match(body, /\/tree\//)
          assert.match(body, /501 Not Implemented/)
          assert.deepEqual(survivors, [true, true, true])
        } else {
          assert.match(body, /\/tree\/blocked/)
          assert.match(body, scenario === "partial" ? /403 Forbidden/ : /501 Not Implemented/)
          if (scenario === "partial") assert.doesNotMatch(body, /\/tree\/removable/)
        }
      } else {
        assert.equal(body, "", "single-resource refusal/success has no multistatus body")
      }
      if (!structuralFactory || !collection) {
        assert.deepEqual(survivors, collection
          ? [true, scenario !== "partial", true] : [scenario !== "success"])
      }
      results.push({ status: response.status, survivors })
    } finally { await server.close() }
  }
  if (collection) {
    assert.equal(results[0].status, results[1].status, `WebDAV DELETE ${scenario} status parity`)
  } else {
    assert.deepEqual(results[0], results[1], `WebDAV DELETE ${scenario} oracle parity`)
  }
}
console.log("WebDAV DELETE oracle parity: PASS (8 single-resource/collection cases)")

const { createS3Server: oracleS3 } = await load("s3/server")
const expectedS3SourceError =
  "mountx: createS3Server() takes an FsDriver (with `stat`, `readdir` and `open`) " +
  "or `{ buckets: { name: driver } }`, and was given neither."
const invalidS3BucketError = (name) =>
  `mountx: ${JSON.stringify(name)} cannot be a bucket name — a path-style S3 URL ` +
  "carries it as one path segment, so it cannot be empty, `.`, `..`, longer than " +
  "255 characters, or contain a slash, a backslash or a control character."

for (const name of ["", ".", "..", "a/b", "a\\b", "a\u0000b", "x".repeat(256)]) {
  const source = { buckets: { [name]: createMemoryDriver() } }
  for (const [label, factory] of [["native", native.createS3Server], ["oracle", oracleS3]]) {
    assert.throws(() => factory(source), (error) => {
      assert.equal(error.constructor, TypeError, `${label} invalid bucket error class`)
      assert.equal(error.message, invalidS3BucketError(name), `${label} invalid bucket error message`)
      return true
    }, `${label} invalid bucket ${JSON.stringify(name)}`)
  }
  const driver = createMemoryDriver()
  for (const [label, factory] of [["native", native.createS3Server], ["oracle", oracleS3]]) {
    assert.throws(() => factory(driver, { bucket: name }), (error) => {
      assert.equal(error.constructor, TypeError, `${label} invalid single bucket error class`)
      assert.equal(error.message, invalidS3BucketError(name), `${label} invalid single bucket error message`)
      return true
    }, `${label} invalid single bucket ${JSON.stringify(name)}`)
  }
}

for (const [label, factory] of [["native", native.createS3Server], ["oracle", oracleS3]]) {
  assert.throws(() => factory({}), (error) => {
    assert.equal(error.constructor, TypeError, `${label} invalid S3 source error class`)
    assert.equal(error.message, expectedS3SourceError, `${label} invalid S3 source error message`)
    return true
  }, `${label} invalid S3 source`)
}

const structuralS3 = createMemoryDriver()
const unicodeBoundaryBucket = "😀".repeat(127)
const structuralS3Results = []
for (const [label, factory] of [["native", native.createS3Server], ["oracle", oracleS3]]) {
  const empty = factory({ buckets: {} })
  const mapped = factory({ buckets: {
    photos: structuralS3,
    notes: createMemoryDriver(),
    [unicodeBoundaryBucket]: createMemoryDriver(),
  } }, { bucket: "ignored" })
  const single = factory(structuralS3, { bucket: "photos" })
  try {
    structuralS3Results.push({
      empty: [empty.buckets, empty.session.bucketNames],
      mapped: [mapped.buckets, mapped.session.bucketNames],
      single: [single.buckets, single.session.bucketNames],
    })
  } finally {
    await Promise.all([empty.close(), mapped.close(), single.close()])
  }
}
assert.deepEqual(structuralS3Results[0], structuralS3Results[1], "S3 structural source oracle parity")
console.log("S3 structural source oracle parity: PASS (empty map, mixed map, single bucket, and 14 refusal cases)")

// Closing one server must not invalidate another adapter over the same driver.
const first = native.createWebdavServer(base, { host: "127.0.0.1", port: 0 })
const second = native.createWebdavServer(base, { host: "127.0.0.1", port: 0 })
await createLoopback(base).writeFile("/shared", "still alive")
await first.close()
try {
  await second.listen()
  const response = await fetch(`${second.url}/shared`, { signal: AbortSignal.timeout(5000) })
  await response.arrayBuffer()
  assert.equal(response.status, 200)
} finally { await second.close() }

for (const name of ["createNfsServer", "createP9Server", "createS3Server", "createWebdavServer"]) {
  assert.throws(() => native[name](minimal, { port: -1 }))
}
assert.throws(() => native.createS3Server({ buckets: { valid: base, invalid: {} } }))
await assert.rejects(() => native.mount(minimal, "/unused", { transport: "invalid" }))
console.log("Native mount acceptance: UNAVAILABLE (no host mount attempted; invalid-option rejection only)")
console.log("Structural factory oracle/server/lifecycle tests: PASS")
