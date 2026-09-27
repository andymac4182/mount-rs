import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { Filesystem, createChunkedDriver } from "../index.js"
import { validateCompactLayoutReceipt } from "../../../benchmarks/storage/compact-layout.mjs"

async function unsupported(filesystem, syscall = undefined) {
  await assert.rejects(() => filesystem.inspectCompactLayout(), (error) => {
    assert.equal(error.code, "ENOTSUP")
    assert.equal(error.syscall, syscall)
    assert.equal(error.path, undefined)
    assert.equal(error.dest, undefined)
    return true
  })
}

// Check the actual loaded native API before creating any persistent stores.
const memory = Filesystem.memory()
try {
  assert.equal(typeof memory.inspectCompactLayout, "function", "native compact layout API exists")
  await unsupported(memory)
} finally {
  await memory.shutdown()
}
await unsupported(memory)

const directory = await mkdtemp(join(tmpdir(), "mount-rs-compact-layout-"))
const owned = new Set()
const open = async (name, mode) => {
  const filesystem = await createChunkedDriver({
    metadata: { kind: "sqlite", uri: join(directory, `${name}-metadata.sqlite`) },
    blocks: { kind: "sqlite", uri: join(directory, `${name}-blocks.sqlite`) },
    owner: "compact-layout-native-oracle",
    chunkSize: 65536,
    ...(mode === "legacy" ? {} : { concurrentWrites: true, inodeUpdates: true }),
    ...(mode === "compact" ? { compactInodeUpdates: true } : {}),
  })
  owned.add(filesystem)
  return filesystem
}
const close = async (filesystem) => {
  await filesystem.shutdown()
  owned.delete(filesystem)
}

let persistedReceipt = null
try {
  const legacy = await open("legacy", "legacy")
  if (process.platform === "win32") await unsupported(legacy, "inspect compact layout")
  else assert.equal(await legacy.inspectCompactLayout(), null, "null proves only validated non-MRC5")
  await close(legacy)
  await unsupported(legacy)

  if (process.platform === "win32") {
    // Concurrent SQLite still requires its qualified Unix physical-file guard.
    await assert.rejects(() => open("compact", "compact"), { code: "ENOTSUP" })
  } else {
    const inode = await open("inode", "inode")
    assert.equal(await inode.inspectCompactLayout(), null, "MRC4 must not become MRC5 evidence")
    await close(inode)

    const payload = Buffer.alloc(65543)
    for (let index = 0; index < payload.length; index++) payload[index] = index % 251
    const first = await open("compact", "compact")
    await first.writeFile("/oracle", payload)
    assert.deepEqual(Buffer.from(await first.readFile("/oracle")), payload)
    const original = validateCompactLayoutReceipt(await first.inspectCompactLayout())
    await close(first)
    await unsupported(first)

    const reopened = await open("compact", "compact")
    const receipt = validateCompactLayoutReceipt(await reopened.inspectCompactLayout())
    assert.deepEqual(receipt, original, "fresh handles observe the same persisted authority and generation")
    assert.deepEqual(Buffer.from(await reopened.readFile("/oracle")), payload)
    const handle = await reopened.open("/oracle", "r")
    try {
      const target = Buffer.alloc(payload.length + 1)
      const read = await handle.read(target, 0, target.length, 0)
      assert.equal(read.bytesRead, payload.length)
      assert.deepEqual(target.subarray(0, read.bytesRead), payload)
      assert.equal((await handle.read(target, 0, 1, payload.length)).bytesRead, 0)
    } finally {
      await handle.close()
    }
    await reopened.unlink("/oracle")
    const afterUnlink = validateCompactLayoutReceipt(await reopened.inspectCompactLayout())
    assert.equal(afterUnlink.backingId, original.backingId)
    await close(reopened)

    const empty = await open("compact", "compact")
    assert.deepEqual(await empty.readdir("/"), [])
    await assert.rejects(() => empty.readFile("/oracle"), { code: "ENOENT" })
    persistedReceipt = validateCompactLayoutReceipt(await empty.inspectCompactLayout())
    assert.deepEqual(persistedReceipt, afterUnlink)
    await close(empty)
  }
} finally {
  // Only this test's successfully opened handles and freshly created directory.
  try {
    for (const filesystem of owned) await filesystem.shutdown()
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}

console.log(JSON.stringify({
  schema: "mount-rs.compact-layout-native-test.v1",
  status: "passed",
  platform: process.platform,
  persistedReceipt,
  scope: process.platform === "win32" ? "unsupported-controls" : "sqlite-explicit-shutdown-and-fresh-reopen",
  atomicNamespaceProof: false,
  crashDurabilityProof: false,
}))
