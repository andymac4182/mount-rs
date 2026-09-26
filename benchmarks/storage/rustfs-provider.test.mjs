import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { test } from "node:test"
import { fileURLToPath } from "node:url"

import { providerById, providerSummary } from "./providers.mjs"

const id = "mount-rs-split-tidb-rustfs"
const environment = Object.freeze({
  MOUNT_RS_TIDB_URL: "mysql://private-tidb.example.test/storage",
  MOUNT_RS_RUSTFS_ENDPOINT: "http://127.0.0.1:19000",
  MOUNT_RS_RUSTFS_BUCKET: "private-bucket",
  MOUNT_RS_RUSTFS_REGION: "private-region",
  MOUNT_RS_RUSTFS_ACCESS_KEY_ID: "private-access-key",
  MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY: "private-secret-key",
})

test("TiDB/RustFS is a separate explicit provider with redacted summaries", () => {
  const definition = providerById(environment).get(id)
  assert.ok(definition, "explicit RustFS benchmark provider is missing")
  assert.equal(definition.metadataProvider, "tidb")
  assert.equal(definition.blockProvider, "rustfs")
  assert.equal(definition.topology, "split-stores")
  assert.equal(definition.availability().configured, true)
  assert.equal(definition.blockDurabilityClass, "configured-volatile-remote")
  assert.equal(definition.metadataDurabilityClass, "configured-durable-remote")
  const summary = JSON.stringify(providerSummary(definition, 65_536))
  for (const value of Object.values(environment)) assert.equal(summary.includes(value), false, "configuration must not enter summary")
  assert.equal(providerById(environment).get("mount-rs-split-tidb-r2").availability().configured, false)
})

for (const name of Object.keys(environment)) {
  test(`missing ${name} is unavailable even with complete R2 credentials`, () => {
    const incomplete = { ...environment,
      MOUNT_RS_R2_ENDPOINT: "https://r2.example.test", MOUNT_RS_R2_BUCKET: "r2-bucket",
      MOUNT_RS_R2_ACCESS_KEY_ID: "r2-key", MOUNT_RS_R2_SECRET_ACCESS_KEY: "r2-secret",
    }
    delete incomplete[name]
    const definition = providerById(incomplete).get(id)
    assert.ok(definition, "explicit RustFS benchmark provider is missing")
    assert.equal(definition.availability().configured, false)
    assert.equal(definition.availability().missing.some((value) => value.includes(name)), true)
  })
}

test("legacy, inode and compact select RustFS and distinct per-run keys through the public factory", async () => {
  const capturePath = fileURLToPath(new URL("./capture-native.cjs", import.meta.url))
  const previous = process.env.NAPI_RS_NATIVE_LIBRARY_PATH
  process.env.NAPI_RS_NATIVE_LIBRARY_PATH = capturePath
  try {
    const capture = createRequire(import.meta.url)(capturePath)
    for (const layout of ["legacy", "inode", "compact"]) {
      const definition = providerById(environment).get(id)
      assert.ok(definition, "explicit RustFS benchmark provider is missing")
      const before = capture.calls.length
      const opened = await definition.create({ environment, layout, runId: `owned-${layout}`, chunkSizeBytes: 65_536 })
      assert.equal(capture.calls.length, before + 1)
      assert.deepEqual(capture.calls.at(-1), {
        metadata: { kind: "tidb", uri: environment.MOUNT_RS_TIDB_URL, key: `storage-benchmark/owned-${layout}/tidb-rustfs/metadata`, durable: true },
        blocks: { kind: "rustfs", key: `storage-benchmark/owned-${layout}/tidb-rustfs/blocks`, endpoint: environment.MOUNT_RS_RUSTFS_ENDPOINT,
          bucket: environment.MOUNT_RS_RUSTFS_BUCKET, region: environment.MOUNT_RS_RUSTFS_REGION,
          accessKeyId: environment.MOUNT_RS_RUSTFS_ACCESS_KEY_ID, secretAccessKey: environment.MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY, durable: false },
        chunkSize: 65_536,
        ...(layout === "legacy" ? {} : { concurrentWrites: true, inodeUpdates: true }),
        ...(layout === "compact" ? { compactInodeUpdates: true } : {}),
        owner: `storage-benchmark-owned-${layout}`,
      })
      let shutdownCalls = 0, releaseShutdown, cleaned = false
      const shutdown = new Promise((resolve) => { releaseShutdown = resolve })
      opened.filesystem.shutdown = () => { shutdownCalls += 1; return shutdown }
      const cleaning = opened.cleanup().then(() => { cleaned = true })
      await Promise.resolve()
      assert.equal(shutdownCalls, 1, "cleanup must call the opened filesystem shutdown")
      assert.equal(cleaned, false, "cleanup must await shutdown")
      releaseShutdown()
      await cleaning
      assert.equal(cleaned, true)
    }
    const durable = { ...environment, MOUNT_RS_TIDB_DURABLE: "0", MOUNT_RS_RUSTFS_DURABLE: "1" }
    const definition = providerById(durable).get(id)
    assert.equal(definition.blockDurabilityClass, "configured-durable-remote")
    assert.equal(definition.metadataDurabilityClass, "configured-volatile-remote")
    const opened = await definition.create({ environment: durable, layout: "legacy", runId: "owned-durable", chunkSizeBytes: 65_536 })
    assert.equal(capture.calls.at(-1).blocks.durable, true)
    assert.equal(capture.calls.at(-1).metadata.durable, false)
    const shutdownError = new Error("private shutdown detail")
    opened.filesystem.shutdown = async () => { throw shutdownError }
    await assert.rejects(opened.cleanup(), (error) => error === shutdownError)
    capture.failNext(Object.assign(new Error("private factory detail"), { code: "CONFIG_ERROR" }))
    await assert.rejects(definition.create({ environment: durable, layout: "compact", runId: "owned-failed", chunkSizeBytes: 65_536 }), { code: "CONFIG_ERROR" })
  } finally {
    if (previous === undefined) delete process.env.NAPI_RS_NATIVE_LIBRARY_PATH
    else process.env.NAPI_RS_NATIVE_LIBRARY_PATH = previous
  }
})
