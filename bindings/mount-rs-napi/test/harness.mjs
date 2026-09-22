import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { pathToFileURL } from "node:url"
import { createLoopback, resolveCapabilities, Filesystem } from "../index.js"

const source = process.env.MOUNTX_SOURCE
assert.ok(source, "MOUNTX_SOURCE is required; harness oracle tests never skip")
assert.equal(execFileSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8")
const oracle = await import(pathToFileURL(`${source}/src/harness.ts`).href)
const methods = ["stat", "lstat", "statfs", "readdir", "open", "mkdir", "rmdir", "unlink",
  "rename", "link", "symlink", "readlink", "chmod", "chown", "lchown", "truncate", "utimes", "lutimes"]
const errorShape = ({ name, message, code, errno, syscall, path, dest }) =>
  ({ name, message, code, errno, syscall, path, dest })

// Every inferred capability independently, and every declaration overriding
// both present and absent methods. Explicit extension arrays keep identity.
const full = Object.fromEntries(methods.map((name) => [name, () => {}]))
const keys = Object.keys(oracle.resolveCapabilities({})).filter((key) => key !== "extensions")
const drivers = [{}, full, { ...full, symlink: 1 }, { ...full, readlink: undefined },
  { ...full, lstat: null }, { mountx: { utimens() {}, mknod: undefined } }]
for (const key of keys) {
  for (const value of [true, false, null, undefined]) {
    drivers.push({ capabilities: { [key]: value } }, { ...full, capabilities: { [key]: value } })
  }
}
for (const driver of drivers) assert.deepEqual(resolveCapabilities(driver), oracle.resolveCapabilities(driver))
for (const resolve of [resolveCapabilities, oracle.resolveCapabilities]) {
  const extensions = []
  assert.equal(resolve({ capabilities: { extensions }, mountx: { mknod() {} } }).extensions, extensions)
}

async function exercise(factory) {
  const calls = []
  const driver = { mountx: { marker: true }, capabilities: { readOnly: false } }
  const result = {}
  for (const name of methods) {
    Object.defineProperty(driver, name, { configurable: true, get() {
      calls.push(["get", name])
      return function (...args) {
        assert.equal(this, driver)
        calls.push([name, ...args])
        return result
      }
    } })
  }
  const loop = factory(driver)
  assert.equal(loop.driver, driver)
  assert.equal(loop.mountx, driver.mountx)
  driver.mountx = {}
  driver.capabilities.readOnly = true
  assert.equal(loop.capabilities.readOnly, false)
  const options = { withFileTypes: true }
  const date = new Date(1234)
  for (const name of methods) {
    // All bound methods survive later replacement, including required ones.
    Object.defineProperty(driver, name, { value: () => assert.fail("late replacement"), configurable: true })
    const args = name === "rename" || name === "link" ? ["a//../b", "../../c/"]
      : name === "symlink" ? ["../opaque//target", "x/../y", "dir"]
      : name === "utimes" || name === "lutimes" ? ["x/../y", date, 4]
      : name === "readdir" || name === "mkdir" ? ["x/../y", options]
      : ["x/../y", 7, 8]
    assert.equal(await loop[name](...args), result)
  }
  return calls
}
assert.deepEqual(await exercise(createLoopback), await exercise(oracle.createLoopback))

for (const factory of [createLoopback, oracle.createLoopback]) {
  for (const asynchronous of [false, true]) {
    const sentinel = { callerOwned: true }
    const loop = factory({ stat() {
      if (asynchronous) return Promise.reject(sentinel)
      throw sentinel
    } })
    let pending
    assert.doesNotThrow(() => { pending = loop.stat("/file") })
    assert.equal(await pending.catch((error) => error), sentinel)
  }
}

for (const name of methods) {
  const failures = []
  for (const factory of [createLoopback, oracle.createLoopback]) {
    const driver = {}
    const loop = factory(driver)
    driver[name] = () => assert.fail("late-added method must stay missing")
    let pending
    assert.doesNotThrow(() => { pending = loop[name]("x/../y", "/dest") })
    assert.ok(pending instanceof Promise)
    const error = await pending.then(() => assert.fail("missing method accepted"), (error) => error)
    const another = await loop[name]("/a", "/b").catch((error) => error)
    assert.notEqual(error, another, "missing calls construct fresh errors")
    assert.equal(error.syscall, name)
    failures.push(errorShape(error))
  }
  assert.deepEqual(failures[0], failures[1], `missing ${name}`)
}

async function ioScenario(factory, operation, scenario) {
  const calls = []
  const ioError = new Error("IO sentinel")
  const closeError = new Error("close sentinel")
  const input = new Uint8Array([11, 22, 33, 44, 55])
  let count = 0
  const handle = {
    async read(buffer, offset, length, position) {
      assert.equal(this, handle)
      calls.push(["read", offset, length, position])
      if (scenario.includes("io-error")) throw ioError
      // Cross the 64 KiB boundary, including short reads and explicit EOF.
      const n = [65536, 3, 0][count++]
      buffer.fill(37, 0, n)
      return { bytesRead: n, buffer: new Uint8Array(0) }
    },
    async write(buffer, offset, length, position) {
      assert.equal(this, handle)
      if (scenario !== "string") assert.equal(buffer, input)
      calls.push(["write", [...buffer], offset, length, position])
      if (scenario.includes("io-error")) throw ioError
      const invalid = { zero: 0, negative: -1, fractional: 0.5, nan: NaN,
        infinity: Infinity, oversized: length + 1, missing: undefined }
      return { bytesWritten: Object.hasOwn(invalid, scenario) ? invalid[scenario] : Math.min(length, 2) }
    },
    async close() {
      assert.equal(this, handle)
      calls.push(["close"])
      if (scenario.includes("close-error")) throw closeError
    },
  }
  const driver = { open(...args) {
    assert.equal(this, driver)
    calls.push(["open", ...args])
    if (scenario === "open-error") throw ioError
    return handle
  } }
  const loop = factory(driver)
  let outcome
  try {
    const value = operation === "read" ? await loop.readFile("a/../file")
      : await loop.writeFile("a/../file", scenario === "string" ? "π🦀" : scenario === "empty" ? input.subarray(0, 0) : input)
    outcome = value && [value.constructor.name, value.length, value[0], value.at(-1)]
  } catch (error) {
    if (scenario.includes("close-error")) assert.equal(error, closeError)
    else if (scenario.includes("io-error") || scenario === "open-error") assert.equal(error, ioError)
    outcome = errorShape(error)
  }
  assert.equal(calls.filter(([name]) => name === "close").length, scenario === "open-error" ? 0 : 1)
  return { calls, outcome }
}
for (const operation of ["read", "write"]) {
  for (const scenario of ["partial", "io-error", "close-error", "io-error-close-error", "open-error",
    ...(operation === "write" ? ["zero", "negative", "fractional", "nan", "infinity", "oversized", "missing", "string", "empty"] : [])]) {
    assert.deepEqual(await ioScenario(createLoopback, operation, scenario),
      await ioScenario(oracle.createLoopback, operation, scenario), `${operation}/${scenario}`)
  }
}

// Native drivers still execute native filesystem operations, with no adapter
// ownership or shutdown side effects introduced by the loopback wrapper.
const native = Filesystem.memory()
try {
  const loop = createLoopback(native)
  assert.equal(loop.driver, native)
  await loop.writeFile("a/../native", "native bytes")
  assert.equal(new TextDecoder().decode(await loop.readFile("/native")), "native bytes")
  assert.deepEqual(loop.capabilities, oracle.resolveCapabilities(native))
} finally { await native.shutdown() }
console.log("Public harness pinned-oracle parity: PASS (capabilities, binding, optional methods, paths, partial IO, close errors)")
