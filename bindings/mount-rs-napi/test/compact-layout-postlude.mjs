import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { test } from "node:test"

const install = createRequire(import.meta.url)("../postlude.cjs")
const encoded = () => new Error([
  "__mount_rs_error_v1__", "ENOTSUP", "-95",
  Buffer.from("inspect compact layout").toString("hex"), "-", "-",
  Buffer.from("compact metadata inspection failed").toString("hex"),
].join("|"))

for (const synchronous of [true, false]) {
  test(`compact layout ${synchronous ? "thrown" : "rejected"} error uses the public error contract`, async () => {
    class NativeFilesystem {
      static memory() { return new NativeFilesystem() }
      inspectCompactLayout() {
        if (synchronous) throw encoded()
        return Promise.reject(encoded())
      }
    }
    const { Filesystem } = install({ Filesystem: NativeFilesystem })
    const filesystem = Filesystem.memory()
    await assert.rejects(async () => filesystem.inspectCompactLayout(), (error) => {
      assert.equal(error.code, "ENOTSUP")
      assert.equal(error.errno, -95)
      assert.equal(error.syscall, "inspect compact layout")
      assert.equal(error.message, "compact metadata inspection failed")
      assert.equal(error.path, undefined)
      assert.equal(error.dest, undefined)
      return true
    })
  })
}

test("postlude preserves receipt identity, null and the native receiver", async () => {
  const receipt = { marker: "MRC5" }
  class NativeFilesystem {
    static memory() { return new NativeFilesystem() }
    async inspectCompactLayout() { assert.equal(this, filesystem); return this.value }
  }
  const { Filesystem } = install({ Filesystem: NativeFilesystem })
  const filesystem = Filesystem.memory()
  filesystem.value = receipt
  assert.equal(await filesystem.inspectCompactLayout(), receipt)
  filesystem.value = null
  assert.equal(await filesystem.inspectCompactLayout(), null)
})
