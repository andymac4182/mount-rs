import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { test } from "node:test"

const install = createRequire(import.meta.url)("../postlude.cjs")
const encoded = () => new Error([
  "__mount_rs_error_v1__", "EIO", "-5",
  Buffer.from("inspectSplitNamespacePresence").toString("hex"), "-", "-",
  Buffer.from("namespace presence metadata failed;shutdown_confirmed").toString("hex"),
].join("|"))

for (const synchronous of [true, false]) {
  test(`namespace presence ${synchronous ? "thrown" : "rejected"} error follows public error contract`, async () => {
    class Filesystem {}
    const binding = install({ Filesystem, inspectSplitNamespacePresence() {
      if (synchronous) throw encoded()
      return Promise.reject(encoded())
    } })
    await assert.rejects(async () => binding.inspectSplitNamespacePresence({}, {}), (error) => {
      assert.equal(error.code, "EIO")
      assert.equal(error.errno, -5)
      assert.equal(error.syscall, "inspectSplitNamespacePresence")
      assert.equal(error.message, "namespace presence metadata failed;shutdown_confirmed")
      assert.equal(error.path, undefined)
      assert.equal(error.dest, undefined)
      return true
    })
  })
}

test("namespace presence wrapping preserves receiver, arguments, JSON and single dispatch", async () => {
  class Filesystem {}
  const metadata = {}, blocks = {}, receipt = '{"schema":"mount-rs.split-namespace-presence.v1"}'
  let calls = 0
  const original = { Filesystem, inspectSplitNamespacePresence(m, b) {
    calls += 1
    assert.equal(this, original)
    assert.equal(m, metadata)
    assert.equal(b, blocks)
    return receipt
  } }
  const binding = install(original)
  const once = binding.inspectSplitNamespacePresence
  install(binding)
  assert.equal(binding.inspectSplitNamespacePresence, once)
  assert.equal(await binding.inspectSplitNamespacePresence(metadata, blocks), receipt)
  assert.equal(calls, 1)
})
