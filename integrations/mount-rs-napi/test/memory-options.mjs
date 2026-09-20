import assert from "node:assert/strict"
import { pathToFileURL } from "node:url"

const { createMemoryDriver: createNativeMemoryDriver } = await import(
  "@andymac4182/mount-rs/drivers/memory",
)

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API memory options: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const [{ createMemoryDriver: createUpstreamMemoryDriver }, { createLoopback }] =
  await Promise.all([
    import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
    import(pathToFileURL(`${source}/src/harness.ts`).href),
  ])

const S_IFIFO = 0o010000

function view(stats) {
  return {
    uid: stats.uid,
    gid: stats.gid,
    mode: stats.mode & 0o177777,
    size: stats.size,
    rdev: stats.rdev,
    file: stats.isFile(),
    directory: stats.isDirectory(),
    fifo: stats.isFIFO(),
  }
}

async function observe(fs) {
  await fs.writeFile("/write-file", Buffer.from("x"))
  const opened = await fs.open("/open-file", "w", 0o777)
  await opened.close()
  await fs.mkdir("/directory", { mode: 0o777 })

  const mountx = fs.mountx
  assert.equal(typeof mountx?.mknod, "function")
  await mountx.mknod("/fifo", S_IFIFO | 0o666, 0)

  const [root, writeFile, openFile, directory, fifo] = await Promise.all([
    fs.stat("/"),
    fs.stat("/write-file"),
    fs.stat("/open-file"),
    fs.stat("/directory"),
    fs.lstat("/fifo"),
  ])
  return {
    root: view(root),
    writeFile: view(writeFile),
    openFile: view(openFile),
    directory: view(directory),
    fifo: view(fifo),
  }
}

async function nativeSnapshot(options) {
  return observe(createNativeMemoryDriver(options))
}

async function upstreamSnapshot(options) {
  return observe(createLoopback(createUpstreamMemoryDriver(options)))
}

const cases = [
  ["defaults", undefined],
  ["custom identity, umask, and root mode", { uid: 501, gid: 20, umask: 0o027, rootMode: 0o751 }],
  ["uid defaulted independently", { uid: 501 }],
  ["gid defaulted independently", { gid: 20 }],
  ["umask defaulted independently", { umask: 0o077 }],
  ["root mode defaulted independently", { rootMode: 0o711 }],
]

for (const [name, options] of cases) {
  const [native, upstream] = await Promise.all([
    nativeSnapshot(options),
    upstreamSnapshot(options),
  ])
  assert.deepEqual(native, upstream, name)
}

const defaults = await nativeSnapshot(undefined)
assert.equal(defaults.root.uid, process.getuid?.() ?? 0)
assert.equal(defaults.root.gid, process.getgid?.() ?? 0)
assert.equal(defaults.root.mode & 0o7777, 0o755)
assert.equal(defaults.writeFile.mode & 0o7777, 0o666)
assert.equal(defaults.openFile.mode & 0o7777, 0o777)
assert.equal(defaults.directory.mode & 0o7777, 0o777)
assert.equal(defaults.fifo.mode & 0o7777, 0o666)
assert.equal(defaults.fifo.fifo, true)

console.log("mount-rs N-API memory options: PASS (native/upstream parity)")
