import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { pathToFileURL } from "node:url"

import { createDriver } from "../index.js"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API JS FsDriver: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const [{ createMemoryDriver }, { rootedNodeFs }, { createLoopback, resolveCapabilities }] =
  await Promise.all([
    import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
    import(pathToFileURL(`${source}/test/rooted-node-fs.ts`).href),
    import(pathToFileURL(`${source}/src/harness.ts`).href),
  ])

function capabilityView(capabilities) {
  return {
    handles: capabilities.handles,
    hardlinks: capabilities.hardlinks,
    symlinks: capabilities.symlinks,
    permissions: capabilities.permissions,
    times: capabilities.times,
    truncate: capabilities.truncate,
    atomicRename: capabilities.atomicRename,
    caseSensitive: capabilities.caseSensitive,
    statfs: capabilities.statfs,
    readOnly: capabilities.readOnly,
    durableWrites: capabilities.durableWrites,
    extensions: [...capabilities.extensions],
  }
}

function statView(stats) {
  return {
    mode: stats.mode & 0o777777,
    size: stats.size,
    nlink: stats.nlink,
    file: stats.isFile(),
    directory: stats.isDirectory(),
    symlink: stats.isSymbolicLink(),
  }
}

function entryView(entry) {
  return {
    name: entry.name,
    parentPath: entry.parentPath,
    file: entry.isFile(),
    directory: entry.isDirectory(),
    symlink: entry.isSymbolicLink(),
  }
}

async function exercise(fs, label) {
  const root = `/js-driver-${label}`
  const directory = `${root}/tree/sub`
  const path = `${directory}/data`

  const firstCreated = await fs.mkdir(directory, { recursive: true, mode: 0o755 })
  await fs.writeFile(path, Buffer.from("0123456789"))

  const handle = await fs.open(path, "r+")
  try {
    const readBuffer = Buffer.alloc(4)
    const readResult = await handle.read(readBuffer, 0, 4, 3)
    assert.equal(readResult.bytesRead, 4, `${label}: positioned read count`)
    assert.equal(Buffer.from(readResult.buffer).toString(), "3456", `${label}: positioned read`)

    const writeResult = await handle.write(Buffer.from("ab"), 0, 2, 1)
    assert.equal(writeResult.bytesWritten, 2, `${label}: positioned write count`)
    await handle.truncate(8)
    if (typeof handle.sync === "function") await handle.sync()
    if (typeof handle.datasync === "function") await handle.datasync()
    assert.equal((await handle.stat()).size, 8, `${label}: handle stat`)
  } finally {
    await handle.close()
  }

  await fs.utimes(path, 1_700_000_000, 1_700_000_001)
  const stats = await fs.stat(path)
  const entries = (await fs.readdir(directory, { withFileTypes: true }))
    .map((entry) => {
      const view = entryView(entry)
      assert.equal(typeof view.parentPath, "string", `${label}: dirent parentPath type`)
      const parentPath = view.parentPath.replaceAll("\\", "/")
      assert.ok(
        parentPath === directory || parentPath.endsWith(directory),
        `${label}: dirent parentPath ${view.parentPath}`,
      )
      return { ...view, parentPath: directory.slice(root.length) }
    })
    .sort((left, right) => left.name.localeCompare(right.name))

  return {
    firstCreatedType: typeof firstCreated,
    data: Buffer.from(await fs.readFile(path)).toString(),
    stat: statView(stats),
    mtimeMs: stats.mtimeMs,
    entries,
  }
}

async function runPair(label, makeDrivers, expectedCapabilities) {
  const { driver, oracleDriver, cleanup } = await makeDrivers()
  const native = createDriver(driver)
  const oracle = createLoopback(oracleDriver)
  try {
    assert.deepEqual(
      capabilityView(native.capabilities),
      capabilityView(expectedCapabilities),
      `${label}: resolved capabilities`,
    )

    const [actual, expected] = await Promise.all([
      exercise(native, `${label}-native`),
      exercise(oracle, `${label}-oracle`),
    ])
    assert.deepEqual(actual, expected, `${label}: native adapter agrees with upstream loopback`)

    await assert.rejects(
      () => native.stat(`/js-driver-${label}-missing`),
      (error) => {
        assert.equal(error.code, "ENOENT", `${label}: errno code`)
        assert.equal(error.syscall, "stat", `${label}: syscall`)
        return true
      },
    )
  } finally {
    await native.shutdown()
    await cleanup?.()
  }
}

await runPair(
  "memory",
  async () => ({
    driver: createMemoryDriver(),
    oracleDriver: createMemoryDriver(),
    expectedCapabilities: resolveCapabilities(createMemoryDriver()),
  }),
  resolveCapabilities(createMemoryDriver()),
)

const nativeRoot = await mkdtemp(join(tmpdir(), "mount-rs-js-driver-native-"))
const oracleRoot = await mkdtemp(join(tmpdir(), "mount-rs-js-driver-oracle-"))
try {
  await runPair(
    "node-fs",
    async () => ({
      driver: rootedNodeFs(nativeRoot),
      oracleDriver: rootedNodeFs(oracleRoot),
      expectedCapabilities: resolveCapabilities(rootedNodeFs(nativeRoot)),
      cleanup: async () => {},
    }),
    resolveCapabilities(rootedNodeFs(nativeRoot)),
  )
} finally {
  await rm(nativeRoot, { recursive: true, force: true })
  await rm(oracleRoot, { recursive: true, force: true })
}

// Check that a normal Node-style rejection keeps its POSIX metadata when it
// travels through the TSFN/Promise bridge. The backing driver remains the
// upstream memory driver; this wrapper only injects one callback failure.
const base = createMemoryDriver()
const failing = {
  ...base,
  async stat(path) {
    if (path === "/bridge-error") {
      const error = new Error("permission denied by callback")
      Object.assign(error, { code: "EACCES", errno: -13, syscall: "stat", path })
      throw error
    }
    return base.stat(path)
  },
}
const failingFs = createDriver(failing)
try {
  await assert.rejects(
    () => failingFs.stat("/bridge-error"),
    (error) => {
      assert.equal(error.code, "EACCES")
      assert.equal(error.errno, -13)
      assert.equal(error.syscall, "stat")
      assert.equal(error.path, "/bridge-error")
      assert.match(error.message, /permission denied by callback/)
      return true
    },
  )
} finally {
  await failingFs.shutdown()
}

// An unresolved callback must be cancellable by explicit shutdown. This also
// guards the strong TSFN reference: after shutdown there is no native handle
// left that can keep Node alive while the JavaScript promise remains pending.
let releasePending
const pending = new Promise((resolve) => {
  releasePending = resolve
})
let callbackStarted
const callbackStartedPromise = new Promise((resolve) => {
  callbackStarted = resolve
})
const pendingDriver = {
  ...createMemoryDriver(),
  stat: () => {
    callbackStarted()
    return pending
  },
}
const pendingFs = createDriver(pendingDriver)
const pendingStat = pendingFs.stat("/pending")
const pendingAssertion = assert.rejects(
  pendingStat,
  (error) => error.code === "EBADF" || error.code === "EIO",
)
let callbackTimer
try {
  await Promise.race([
    callbackStartedPromise,
    new Promise((_, reject) => {
      callbackTimer = setTimeout(() => reject(new Error("driver callback did not start")), 5000)
    }),
  ])
} finally {
  clearTimeout(callbackTimer)
}
await pendingFs.shutdown()
await pendingAssertion
releasePending()

console.log("mount-rs N-API JS FsDriver: PASS")
await import("./structural-factories.mjs")
