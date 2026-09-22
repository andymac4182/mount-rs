import assert from "node:assert/strict"
import { pathToFileURL } from "node:url"
import nativeBinding from "../index.js"
import installUtilities from "../postlude-utilities.cjs"

const native = installUtilities(nativeBinding)

const source = process.env.MOUNTX_SOURCE
let oracle
if (source) {
  const [path, errors, types, lock] = await Promise.all([
    import(pathToFileURL(`${source}/src/path.ts`).href),
    import(pathToFileURL(`${source}/src/errors.ts`).href),
    import(pathToFileURL(`${source}/src/types.ts`).href),
    import(pathToFileURL(`${source}/src/lock.ts`).href),
  ])
  oracle = { path, errors, types, lock }
}

const paths = [
  "/",
  "/a/b",
  "/a//b/../c/",
  "a/./b/../../c",
  "../../",
  "",
  "/é/δ",
]
for (const value of paths) {
  assert.equal(native.isNormalizedPath(value), oracle ? oracle.path.isNormalizedPath(value) : native.isNormalizedPath(value))
  assert.deepEqual(native.splitPath(value), oracle ? oracle.path.splitPath(value) : native.splitPath(value))
  assert.equal(native.normalizePath(value), oracle ? oracle.path.normalizePath(value) : native.normalizePath(value))
  assert.deepEqual(native.resolvePath(value), oracle ? oracle.path.resolvePath(value) : native.resolvePath(value))
  assert.equal(native.dirname(value), oracle ? oracle.path.dirname(value) : native.dirname(value))
  assert.equal(native.basename(value), oracle ? oracle.path.basename(value) : native.basename(value))
}

const joinCases = [
  [],
  ["/a", "b", "..", "c"],
  ["a", "//b//", "./c"],
  ["../../", "x"],
]
for (const parts of joinCases) {
  const expected = oracle ? oracle.path.joinPath(...parts) : native.joinPath(...parts)
  assert.equal(native.joinPath(...parts), expected)
  assert.equal(native.joinPathParts(parts), expected)
}
for (const [path, parent] of [
  ["/", "/"],
  ["/a", "/"],
  ["/a/b", "/a"],
  ["/ab", "/a"],
]) {
  const expected = oracle
    ? oracle.path.isPathInside(path, parent)
    : native.isPathInside(path, parent)
  assert.equal(native.isPathInside(path, parent), expected)
}

const modeNames = [
  "S_IFMT",
  "S_IFREG",
  "S_IFDIR",
  "S_IFLNK",
  "S_IFBLK",
  "S_IFCHR",
  "S_IFIFO",
  "S_IFSOCK",
  "S_ISGID",
  "S_IXGRP",
]
for (const name of modeNames) {
  assert.equal(native[name], oracle ? oracle.types[name] : native[name])
}
for (const mode of [native.S_IFREG, native.S_IFDIR, native.S_IFLNK, 0]) {
  assert.equal(native.fileTypeMode(mode), mode & native.S_IFMT || native.S_IFREG)
  assert.equal(native.isSpecialMode(mode), false)
}
for (const mode of [native.S_IFBLK, native.S_IFCHR, native.S_IFIFO, native.S_IFSOCK]) {
  assert.equal(native.isSpecialMode(mode), true)
}

assert.deepEqual(native.errnoCodes(), {
  EPERM: 1,
  ENOENT: 2,
  EINTR: 4,
  EIO: 5,
  ENXIO: 6,
  EBADF: 9,
  EAGAIN: 11,
  ENOMEM: 12,
  EACCES: 13,
  EBUSY: 16,
  EEXIST: 17,
  EXDEV: 18,
  ENODEV: 19,
  ENOTDIR: 20,
  EISDIR: 21,
  EINVAL: 22,
  ENFILE: 23,
  EMFILE: 24,
  EFBIG: 27,
  ENOSPC: 28,
  ESPIPE: 29,
  EROFS: 30,
  EMLINK: 31,
  ERANGE: 34,
  ENAMETOOLONG: 36,
  ENOSYS: 38,
  ENOTEMPTY: 39,
  ELOOP: 40,
  ENODATA: 61,
  EPROTO: 71,
  EOVERFLOW: 75,
  ENOTSUP: 95,
  ESTALE: 116,
  EDQUOT: 122,
})
const localError = native.fsError("ENOENT", { syscall: "stat", path: "/missing" })
assert.equal(localError instanceof Error, true)
assert.equal(localError.code, "ENOENT")
assert.equal(localError.errno, -2)
assert.equal(localError.message, "ENOENT: no such file or directory, stat '/missing'")
assert.equal(native.isFsError(localError), true)
assert.equal(native.isFsError(localError, "ENOENT"), true)
assert.equal(native.isFsError(localError, "EIO"), false)
assert.equal(native.errnoOf(localError), 2)
assert.equal(native.errnoOf({ errno: -22 }), 22)
assert.equal(native.errnoOf(new Error("unknown")), 5)
const cause = { source: "utilities-test" }
assert.equal(native.fsError("EIO", { cause }).cause, cause)
const localRange = native.rangeError("offset", "an integer", 1.5)
assert.equal(localRange instanceof RangeError, true)
assert.equal(localRange.code, "ERR_OUT_OF_RANGE")
assert.equal(
  localRange.message,
  'The value of "offset" is out of range. It must be an integer. Received 1.5',
)

if (oracle) {
  assert.deepEqual(native.errnoCodes(), oracle.errors.ERRNO_CODES)
  for (const code of Object.keys(oracle.errors.ERRNO_CODES)) {
    const options = { syscall: "stat", path: "/missing", dest: "/other" }
    const actual = native.fsError(code, options)
    const expected = oracle.errors.fsError(code, options)
    assert.equal(actual instanceof Error, true)
    assert.equal(actual.message, expected.message)
    assert.deepEqual(
      {
        errno: actual.errno,
        code: actual.code,
        syscall: actual.syscall,
        path: actual.path,
        dest: actual.dest,
      },
      {
        errno: expected.errno,
        code: expected.code,
        syscall: expected.syscall,
        path: expected.path,
        dest: expected.dest,
      },
    )
    assert.equal(native.isFsError(actual), true)
    assert.equal(native.isFsError(actual, code), true)
    assert.equal(native.isFsError(actual, "EIO"), code === "EIO")
    assert.equal(native.errnoOf(actual), oracle.errors.errnoOf(expected))
  }
  const range = native.rangeError("offset", "an integer", 1.5)
  const expectedRange = oracle.errors.rangeError("offset", "an integer", 1.5)
  assert.equal(range instanceof RangeError, true)
  assert.equal(range.code, expectedRange.code)
  assert.equal(range.message, expectedRange.message)
}

const lock = new native.PathLock()
const order = []
let releaseReader
let markReaderStarted
const readerStarted = new Promise((resolve) => {
  markReaderStarted = resolve
})
const readerDone = lock.read(async () => {
  order.push("reader-start")
  markReaderStarted()
  await new Promise((resolve) => {
    releaseReader = resolve
  })
  order.push("reader-end")
})
await readerStarted
const writerDone = lock.write(async () => {
  order.push("writer")
})
assert.deepEqual(order, ["reader-start"])
releaseReader()
assert.equal(await readerDone, undefined)
assert.equal(await writerDone, undefined)
assert.deepEqual(order, ["reader-start", "reader-end", "writer"])

const valueLock = new native.PathLock()
const value = { kind: "object-result" }
assert.strictEqual(await valueLock.read(async () => value), value)
assert.equal(await valueLock.write(async () => 42), 42)

const identityLock = new native.PathLock()
const expectedError = new Error("callback identity")
await assert.rejects(
  identityLock.read(async () => {
    throw expectedError
  }),
  (error) => error === expectedError,
)

function callbackStartBarrier() {
  let markStarted
  const started = new Promise((resolve) => {
    markStarted = resolve
  })
  return { started, markStarted }
}

async function awaitCallbackStart(started, task, label) {
  await Promise.race([
    started,
    task.then(
      () => {
        throw new Error(`${label} completed without invoking its callback`)
      },
      (error) => {
        throw error
      },
    ),
  ])
}

async function writerStress(PathLock) {
  const lock = new PathLock()
  const events = []
  let release
  const firstStarted = callbackStartBarrier()
  const first = lock.write(async () => {
    events.push("w0:start")
    firstStarted.markStarted()
    await new Promise((resolve) => {
      release = resolve
    })
    events.push("w0:end")
    return { id: 0 }
  })
  await awaitCallbackStart(firstStarted.started, first, "first writer")

  const queued = [1, 2, 3, 4, 5].map((id) =>
    lock.write(async () => {
      events.push(`w${id}`)
      if (id === 2) throw new Error("writer-2")
      return { id }
    }),
  )
  release()
  const results = await Promise.all(
    queued.map((task) =>
      task.then(
        (value) => ({ status: "fulfilled", value }),
        (error) => ({ status: "rejected", reason: { name: error.name, message: error.message } }),
      ),
    ),
  )
  return { events, first: await first, results }
}

async function readerAfterQueuedWriter(PathLock) {
  const lock = new PathLock()
  const events = []
  let release
  const firstStarted = callbackStartBarrier()
  const first = lock.read(async () => {
    events.push("r0:start")
    firstStarted.markStarted()
    await new Promise((resolve) => {
      release = resolve
    })
    events.push("r0:end")
    return "r0"
  })
  await awaitCallbackStart(firstStarted.started, first, "first reader")

  const writer = lock.write(async () => {
    events.push("w")
    return "w"
  })
  const secondStarted = callbackStartBarrier()
  const second = lock.read(async () => {
    events.push("r1")
    secondStarted.markStarted()
    return "r1"
  })
  await awaitCallbackStart(secondStarted.started, second, "second reader")
  const beforeRelease = [...events]
  release()
  return { beforeRelease, events, values: await Promise.all([first, writer, second]) }
}

const nativeWriterStress = await writerStress(native.PathLock)
assert.deepEqual(nativeWriterStress.events, ["w0:start", "w0:end", "w1", "w2", "w3", "w4", "w5"])
assert.deepEqual(nativeWriterStress.first, { id: 0 })
assert.deepEqual(nativeWriterStress.results, [
  { status: "fulfilled", value: { id: 1 } },
  { status: "rejected", reason: { name: "Error", message: "writer-2" } },
  { status: "fulfilled", value: { id: 3 } },
  { status: "fulfilled", value: { id: 4 } },
  { status: "fulfilled", value: { id: 5 } },
])

const nativeReaderStress = await readerAfterQueuedWriter(native.PathLock)
assert.deepEqual(nativeReaderStress.beforeRelease, ["r0:start", "r1"])
assert.deepEqual(nativeReaderStress.events, ["r0:start", "r1", "r0:end", "w"])
assert.deepEqual(nativeReaderStress.values, ["r0", "w", "r1"])

if (oracle) {
  assert.deepEqual(nativeWriterStress, await writerStress(oracle.lock.PathLock))
  assert.deepEqual(nativeReaderStress, await readerAfterQueuedWriter(oracle.lock.PathLock))
}

console.log(`mount-rs N-API utilities: PASS${oracle ? " (differential)" : ""}`)
