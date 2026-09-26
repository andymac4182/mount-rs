// Current-source native SQLite byte, EOF, reopen and error control in either profiling mode.
import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRequire } from "node:module"

import { deltaNativeSnapshots } from "./diagnostics.mjs"

const profiling = process.env.MOUNT_RS_PROFILE_IO === "1"
const require = createRequire(import.meta.url)
const { createChunkedDriver, storageDiagnostics } = require("../../bindings/mount-rs-napi")
const directory = await mkdtemp(join(tmpdir(), "mount-rs-storage-diagnostics-"))
const options = {
  metadata: { kind: "sqlite", uri: join(directory, "metadata.sqlite") },
  blocks: { kind: "sqlite", uri: join(directory, "blocks.sqlite") },
  chunkSize: 65_536,
  owner: "storage-diagnostics-control",
}
const capture = () => JSON.parse(storageDiagnostics())
const initial = capture()
assert.equal(initial.enabled, profiling)
const assertDisabled = (snapshot) => {
  assert.equal(snapshot.enabled, false)
  assert.ok(snapshot.storage.entries.every((entry) => entry.calls === "0" && entry.bytes === "0"))
  assert.equal(snapshot.storage.forwarding_boxes.calls, "0")
  assert.equal(snapshot.storage.forwarding_boxes.requested_object_bytes, "0")
}
if (!profiling) assertDisabled(initial)
let filesystem
try {
  filesystem = await createChunkedDriver(options)
  const payload = Buffer.from("native SQLite bytes and EOF")
  await filesystem.writeFile("/sample", payload)
  assert.deepEqual(Buffer.from(await filesystem.readFile("/sample")), payload)
  const handle = await filesystem.open("/sample", "r")
  const target = Buffer.alloc(8)
  const eof = await handle.read(target, 0, target.length, payload.length)
  assert.equal(eof.bytesRead, 0)
  await handle.close()
  // Capture while both first-lifecycle connections are alive. Their counters
  // disappear on shutdown and cannot be inferred from a later process snapshot.
  const firstLive = capture()
  if (profiling) assert.ok(firstLive.sqlite.connections.length >= 2)
  await filesystem.shutdown()
  filesystem = null
  const firstClosed = capture()
  const firstIds = new Set(firstLive.sqlite.connections.map((entry) => entry.connection_id))
  assert.ok(firstClosed.sqlite.connections.every((entry) => !firstIds.has(entry.connection_id)))

  filesystem = await createChunkedDriver(options)
  const reopenStart = capture()
  if (profiling) assert.ok(reopenStart.sqlite.connections.length >= 2)
  assert.deepEqual(Buffer.from(await filesystem.readFile("/sample")), payload)
  await filesystem.unlink("/sample")
  await assert.rejects(filesystem.readFile("/sample"), (error) => error.code === "ENOENT")
  const reopenLive = capture()
  if (profiling) assert.ok(reopenLive.sqlite.connections.length >= 2)

  if (profiling) {
    const first = deltaNativeSnapshots(initial, firstLive)
    const reopened = deltaNativeSnapshots(reopenStart, reopenLive)
    assert.equal(first.complete, true, JSON.stringify(first.issues))
    assert.equal(reopened.complete, true, JSON.stringify(reopened.issues))
    const operation = (delta, name) => delta.storage.entries.find((entry) => entry.name === name)
    assert.ok(BigInt(operation(first, "blocks.put").bytes) >= BigInt(payload.length))
    assert.ok(BigInt(operation(first, "blocks.get").bytes) >= BigInt(payload.length))
    assert.ok(BigInt(operation(reopened, "blocks.get").bytes) >= BigInt(payload.length))
    assert.ok(BigInt(operation(first, "metadata.publish").calls) >= 2n)
    assert.ok(first.sqlite.connections.some((entry) => BigInt(entry.sql_statements) > 0n))
    assert.ok(first.sqlite.connections.some((entry) => BigInt(entry.pager.page_writes) > 0n))
    assert.ok(first.profile.entries.some((entry) => entry.name === "provider.namespace_serialized_bytes" && BigInt(entry.units) > 0n))
    assert.ok(BigInt(first.storage.forwarding_boxes.calls) > 0n)
    assert.ok(BigInt(first.storage.forwarding_boxes.requested_object_bytes) > 0n)
    console.log(JSON.stringify({ status: "PASS", mode: "enabled", bytes: payload.length, eofBytes: eof.bytesRead,
      reopened: true, negativeNativeError: "ENOENT", firstLiveConnections: firstLive.sqlite.connections.length,
      reopenLiveConnections: reopenLive.sqlite.connections.length, firstClosedConnections: firstClosed.sqlite.connections.length,
      firstLiveAttributionComplete: first.complete, reopenedLiveAttributionComplete: reopened.complete,
      blockPutBytesFirstLive: operation(first, "blocks.put").bytes, blockGetBytesFirstLive: operation(first, "blocks.get").bytes,
      blockGetBytesReopenedLive: operation(reopened, "blocks.get").bytes,
      forwardingBoxCallsFirstLive: first.storage.forwarding_boxes.calls,
      forwardingRequestedObjectBytesFirstLive: first.storage.forwarding_boxes.requested_object_bytes }))
  } else {
    for (const snapshot of [firstLive, firstClosed, reopenStart, reopenLive]) assertDisabled(snapshot)
    console.log(JSON.stringify({ status: "PASS", mode: "disabled", bytes: payload.length, eofBytes: eof.bytesRead,
      reopened: true, negativeNativeError: "ENOENT", firstLiveConnections: firstLive.sqlite.connections.length,
      reopenLiveConnections: reopenLive.sqlite.connections.length, firstClosedConnections: firstClosed.sqlite.connections.length,
      forwardingBoxCalls: "0", forwardingRequestedObjectBytes: "0" }))
  }
} finally {
  if (filesystem) await filesystem.shutdown()
  await rm(directory, { recursive: true, force: true })
}
