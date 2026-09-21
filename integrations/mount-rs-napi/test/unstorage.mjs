import assert from "node:assert/strict"
import { createUnstorageDriver } from "../index.js"

function makeStore() {
  return {
    values: new Map([
      ["raw:string", "hello from a raw string"],
      ["raw:object", { answer: 42 }],
    ]),
    metadata: new Map(),
    hasItem(key) {
      return this.values.has(key)
    },
    async getItemRaw(key) {
      const value = this.values.get(key)
      if (value === undefined) return null
      return value instanceof Uint8Array ? new Uint8Array(value) : value
    },
    async setItemRaw(key, value) {
      assert.equal(value instanceof Uint8Array, true, "setItemRaw receives a byte array")
      this.values.set(key, new Uint8Array(value))
      this.metadata.set(key, {
        size: value.byteLength,
        atime: new Date(1_700_000_000_000),
        mtime: new Date(1_700_000_001_000),
      })
    },
    async removeItem(key) {
      this.values.delete(key)
      this.metadata.delete(key)
    },
    async getKeys(prefix) {
      return [...this.values.keys()].filter((key) => key.startsWith(prefix) && !key.endsWith("$"))
    },
    async getKeysBounded(prefix, maxKeys) {
      const keys = await this.getKeys(prefix)
      return keys.slice(0, maxKeys + 1)
    },
    getMeta(key) {
      return this.metadata.get(key) ?? {}
    },
  }
}

const store = makeStore()
store.metadata.set("raw:string", {
  size: 1234,
  atime: new Date(1_700_000_000_000),
  mtime: new Date(1_700_000_001_000),
  ctime: new Date(1_700_000_002_000),
  birthtime: new Date(1_699_999_999_000),
})
const fs = createUnstorageDriver(store)
assert.equal(fs.capabilities.handles, true)
assert.equal(fs.capabilities.readOnly, false)

const bytes = new Uint8Array([0, 1, 254, 255])
await fs.writeFile("/bytes.bin", bytes)
assert.deepEqual([...await fs.readFile("/bytes.bin")], [...bytes])
assert.deepEqual([...store.values.get("bytes.bin")], [...bytes])

const pending = await fs.open("/pending.bin", "w+")
const pendingBytes = new Uint8Array([7, 8, 9])
assert.equal((await pending.write(pendingBytes)).bytesWritten, pendingBytes.byteLength)
assert.deepEqual([...store.values.get("pending.bin")], [])
await pending.sync()
assert.deepEqual([...store.values.get("pending.bin")], [...pendingBytes])
await pending.close()

assert.equal((await fs.stat("/bytes.bin")).size, bytes.byteLength)
const metadataStats = await fs.stat("/raw/string")
assert.equal(metadataStats.size, 1234)
assert.equal(metadataStats.atimeMs, 1_700_000_000_000)
assert.equal(metadataStats.mtimeMs, 1_700_000_001_000)
assert.equal(metadataStats.ctimeMs, 1_700_000_002_000)
assert.equal(metadataStats.birthtimeMs, 1_699_999_999_000)
assert.equal(new TextDecoder().decode(await fs.readFile("/raw/string")), "hello from a raw string")
assert.equal(new TextDecoder().decode(await fs.readFile("/raw/object")), '{"answer":42}')
assert.deepEqual(
  (await fs.readdir("/"))
    .map((entry) => entry.name)
    .sort(),
  ["bytes.bin", "pending.bin", "raw"].sort(),
)

const boundedStore = makeStore()
boundedStore.values.clear()
boundedStore.values.set("one", new Uint8Array([1]))
boundedStore.values.set("two", new Uint8Array([2]))
boundedStore.values.set("three", new Uint8Array([3]))
const bounded = createUnstorageDriver(boundedStore)
await assert.rejects(
  () => bounded.readdirBounded("/", 2),
  (error) => error.code === "EOVERFLOW",
)
assert.equal((await bounded.readdirBounded("/", 3)).length, 3)
await bounded.shutdown()

const legacyStore = makeStore()
delete legacyStore.getKeysBounded
const legacy = createUnstorageDriver(legacyStore)
await assert.rejects(
  () => legacy.readdirBounded("/", 2),
  (error) => error.code === "ENOTSUP",
)
await legacy.shutdown()

const readOnly = createUnstorageDriver(store, { readOnly: true })
assert.equal(readOnly.capabilities.readOnly, true)
await assert.rejects(
  () => readOnly.writeFile("/read-only", new Uint8Array([1])),
  (error) => error.code === "EROFS",
)

const failingStore = makeStore()
failingStore.setItemRaw = async () => {
  const error = new Error("backend write failed")
  error.code = "EIO"
  throw error
}
const failing = createUnstorageDriver(failingStore)
await assert.rejects(
  () => failing.writeFile("/failure", new Uint8Array([3])),
  (error) => error.code === "EIO" && error.message.includes("backend write failed"),
)

await fs.shutdown()
await fs.shutdown()
await readOnly.shutdown()
await failing.shutdown()

if (process.env.MOUNTX_SOURCE) {
  // Keep the capability-limited, oracle-backed matrix in the root test tree
  // while making it part of the published N-API package gate.
  await import("../../../tests/unstorage/capability-parity.mjs")
  // The remaining-skip packet executes every capability boundary and keeps
  // exact ENOSYS/ENOTSUP results separate from upstream capability skips.
  await import("../../../tests/unstorage/remaining-skip-parity.mjs")
} else {
  console.log("mount-rs N-API unstorage capability parity: SKIP (MOUNTX_SOURCE unset)")
  console.log("mount-rs N-API unstorage remaining-skip parity: SKIP (MOUNTX_SOURCE unset)")
}

console.log("mount-rs N-API unstorage bridge: PASS")
