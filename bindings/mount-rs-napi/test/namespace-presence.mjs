import assert from "node:assert/strict"
import { inspectSplitNamespacePresence } from "../index.js"

// Every case is rejected before checkout or blob client construction. These
// exercise the actual addon export and public error adapter without a backend.
assert.equal(typeof inspectSplitNamespacePresence, "function", "native namespace presence API exists")
const metadata = {
  kind: "tidb", uri: "mysql://private-user:private-password@127.0.0.1:14000/storage",
  key: "storage-benchmark/native-control/metadata", durable: true,
}
const blocks = {
  kind: "rustfs", key: "storage-benchmark/native-control/blocks", durable: true,
  endpoint: "http://127.0.0.1:19000", bucket: "private-bucket", region: "us-east-1",
  accessKeyId: "private-access", secretAccessKey: "private-secret",
}
const cases = [
  [{ kind: "memory" }, {}],
  [{}, { kind: "r2" }],
  [{ key: "a/../b" }, {}],
  [{}, { key: "a//b" }],
  [{ key: "a".repeat(256) }, {}],
  [{}, { key: "a".repeat(513) }],
  [{ endpoint: "private-endpoint" }, {}],
  [{}, { uri: "private-uri" }],
  [{ key: undefined }, {}],
  [{}, { secretAccessKey: undefined }],
  [{ uri: "private malformed URL" }, {}],
  [{}, { endpoint: "http://private-user:private-password@127.0.0.1:19000" }],
]
for (const [metadataOverride, blocksOverride] of cases) {
  await assert.rejects(() => inspectSplitNamespacePresence(
    { ...metadata, ...metadataOverride }, { ...blocks, ...blocksOverride },
  ), (error) => {
    assert.equal(error.code, "EINVAL")
    assert.equal(error.errno, -22)
    assert.equal(error.syscall, "inspectSplitNamespacePresence")
    assert.equal(error.message, "namespace presence configuration invalid")
    assert.equal(error.path, undefined)
    assert.equal(error.dest, undefined)
    assert.ok(!error.message.includes("private"))
    return true
  })
}
console.log(`native namespace presence invalid-configuration controls: ${cases.length} PASS; backend calls unexecuted`)
