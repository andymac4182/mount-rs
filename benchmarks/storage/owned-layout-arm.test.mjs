import assert from "node:assert/strict"
import { test } from "node:test"

let api = {}
try { api = await import("./owned-layout-arm.mjs") } catch (error) {
  if (error.code !== "ERR_MODULE_NOT_FOUND" || !error.message.includes("owned-layout-arm.mjs")) throw error
}

function implementation() {
  assert.equal(typeof api.createOwnedLayoutArm, "function", "owned layout arm capability is missing")
  return api.createOwnedLayoutArm
}

const canaryPath = "/.mount-rs-owned-layout-canary.bin"
const backingId = "1234567890abcdef1234567890abcdef"
const privateError = () => Object.assign(new Error("private URI, bucket, credential and namespace"), { code: "private code", path: "private path" })

const presence = () => ({
  schema: "mount-rs.split-namespace-presence.v1", namespace_absent: true,
  metadata: { provider: "tidb", key_scope: "exact_input_utf8_bytes", schema_setup: "shared_ddl_and_session_configuration",
    row_presence: { metadata: false, inodes: false, compact_guards: false, compact_members: false, compact_dentries: false, block_authority: false, blocks: false }, observed_at_ns: "1" },
  blobs: { provider: "rustfs", scope: "canonical_ascii_prefix_descendants", observation: "signed_list_page_api", requested_max_keys: 1, prefix_absent: true, observed_at_ns: "2" },
  pool_shutdown: { confirmed: true, elapsed_ns: "1" }, clock: "elapsed_monotonic_since_native_preflight_start", consistency: "separate_observations_no_reservation",
  limits: { probe_deadline_ms: 30000, pool_shutdown_deadline_ms: 15000, list_request_keys: 1,
    response_byte_cap: "unavailable", server_truncation_flag: "unavailable", deadline_semantics: "cooperative_await_with_elapsed_recheck" },
})

function deferred() {
  let resolve, reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

function model(layout = "compact", behavior = {}) {
  const metadataKey = `comparison/${layout}-a/metadata`
  const blockPrefix = `comparison/${layout}-a/blocks`
  const input = {
    cohort: { id: `${layout}-a`, layout, metadataKey, blockPrefix, owner: "comparison-owner" },
    metadata: { kind: "tidb", uri: "mysql://private-user:private-secret@private-tidb/storage", key: metadataKey, durable: true },
    blocks: { kind: "rustfs", key: blockPrefix, endpoint: "http://127.0.0.1:19000", bucket: "private-bucket", region: "private-region",
      accessKeyId: "private-access", secretAccessKey: "private-secret", durable: false },
  }
  const store = { files: new Map(), generation: 1n, backingId }
  const events = [], creates = [], filesystems = [], writeHandles = [], readHandles = []
  const binding = {
    async inspectSplitNamespacePresence(metadata, blocks) {
      assert.equal(this, binding)
      events.push(["preflight"])
      assert.equal(Object.isFrozen(metadata), true)
      assert.equal(Object.isFrozen(blocks), true)
      if (behavior.preflightError) throw privateError()
      if (behavior.preflightGate) await behavior.preflightGate.promise
      return JSON.stringify(behavior.presence || presence())
    },
    async createChunkedDriver(options) {
      assert.equal(this, binding)
      const index = creates.length
      creates.push(options)
      events.push(["create", index])
      if (behavior.factoryErrorAt === index) throw privateError()
      if (behavior.reuseAt === index) return filesystems[0]
      const filesystem = {
        closed: false,
        async shutdown() {
          assert.equal(this, filesystem)
          events.push(["shutdown", index])
          if (behavior.shutdownGateAt === index) await behavior.shutdownGate.promise
          if (behavior.shutdownErrorAt === index) throw privateError()
          filesystem.closed = true
        },
        async inspectCompactLayout() {
          assert.equal(this, filesystem)
          assert.equal(filesystem.closed, false)
          events.push(["inspect", index])
          const receipt = options.compactInodeUpdates ? { schema: "mount-rs.compact-layout-receipt.v1", marker: "MRC5", backingId: store.backingId,
            structuralGeneration: String(store.generation), blockAuthorityVerified: true } : null
          return behavior.proof ? behavior.proof(index, receipt) : receipt
        },
        async readdirBounded(path, limit) {
          assert.equal(this, filesystem)
          assert.equal(filesystem.closed, false)
          assert.equal(path, "/")
          assert.equal(limit, 1)
          events.push(["empty", index])
          if (behavior.nonemptyAt === index) return [{ name: "private leftover" }]
          if (behavior.listShapeAt === index) return "private listing"
          return [...store.files.keys()].map((name) => ({ name })).slice(0, 1)
        },
        async open(path, flags) {
          assert.equal(this, filesystem)
          assert.equal(filesystem.closed, false)
          events.push(["open", index, path, flags])
          if (flags === "wx") {
            if (store.files.has(path)) throw privateError()
            store.files.set(path, new Uint8Array())
            store.generation++
          } else {
            assert.equal(flags, "r")
            if (!store.files.has(path)) throw privateError()
          }
          const handle = {
            async write(bytes, offset, length, position) {
              assert.equal(this, handle)
              assert.equal(position, 0)
              assert.equal(offset, 0)
              assert.equal(length, 4096)
              events.push(["write", index, path])
              if (behavior.writeError) throw privateError()
              const count = behavior.shortWrite ? 4095 : 4096
              store.files.set(path, Uint8Array.from(bytes.subarray(0, count)))
              if (behavior.writeResult) return behavior.writeResult(bytes)
              return { bytesWritten: count, buffer: bytes }
            },
            async sync() {
              assert.equal(this, handle)
              events.push(["sync", index, path])
              if (behavior.syncError) throw privateError()
            },
            async read(bytes, offset, length, position) {
              assert.equal(this, handle)
              assert.equal(offset, 0)
              assert.equal(length, 1)
              assert.equal(position, 4096)
              events.push(["eof", index, path])
              if (behavior.eofError) throw privateError()
              return { bytesRead: behavior.eofNonzero ? 1 : 0, buffer: bytes }
            },
            async close() {
              assert.equal(this, handle)
              events.push(["handle-close", index, path, flags])
              if (behavior.handleCloseError === flags) throw privateError()
            },
          }
          ;(flags === "wx" ? writeHandles : readHandles).push(handle)
          return handle
        },
        async readFile(path) {
          assert.equal(this, filesystem)
          assert.equal(filesystem.closed, false)
          events.push(["read", index, path])
          if (behavior.readError) throw privateError()
          const bytes = Uint8Array.from(store.files.get(path) || [])
          if (behavior.corrupt) bytes[0] ^= 1
          if (behavior.shortRead) return bytes.subarray(0, 4095)
          if (behavior.extraRead) return Uint8Array.from([...bytes, 1])
          return bytes
        },
        async unlink(path) {
          assert.equal(this, filesystem)
          assert.equal(filesystem.closed, false)
          events.push(["unlink", index, path])
          if (behavior.unlinkError) throw privateError()
          assert.equal(store.files.delete(path), true)
          store.generation++
        },
      }
      filesystems.push(filesystem)
      if (behavior.configure) behavior.configure(index, filesystem)
      return filesystem
    },
  }
  return { input, binding, behavior, store, events, creates, filesystems, writeHandles, readHandles }
}

function fixedFailure(code, cleanupCodes) {
  return (error) => {
    assert.equal(error.code, code)
    assert.match(error.message, /^owned layout arm /u)
    assert.equal(error.cause, undefined)
    assert.equal(error.path, undefined)
    assert.equal(error.message.includes("private"), false)
    assert.equal(JSON.stringify(error).includes("private"), false)
    if (cleanupCodes) assert.deepEqual(error.cleanupCodes, cleanupCodes)
    return true
  }
}

function deeplyFrozen(value) {
  assert.equal(Object.isFrozen(value), true)
  for (const child of Object.values(value)) if (child && typeof child === "object") deeplyFrozen(child)
}

async function ready(model) {
  const arm = implementation()(model.binding, model.input)
  await arm.prepare()
  return { arm, opened: arm.takeForRunner() }
}

test("construction is inert and pins an immutable exact private cohort and configuration", async () => {
  const makeArm = implementation(), fixture = model()
  const source = structuredClone(fixture.input)
  const arm = makeArm(fixture.binding, fixture.input)
  assert.equal(fixture.events.length, 0)
  assert.deepEqual(arm.cohort, source.cohort)
  assert.notEqual(arm.cohort, fixture.input.cohort)
  deeplyFrozen(arm.cohort)
  assert.equal(Object.isFrozen(arm), true)
  assert.equal(Object.keys(arm).includes("cohort"), false)
  fixture.input.cohort.metadataKey = "foreign/metadata"
  fixture.input.metadata.uri = "private replacement"
  fixture.input.blocks.secretAccessKey = "private replacement"
  await arm.prepare()
  assert.equal(fixture.creates[0].metadata.uri, source.metadata.uri)
  assert.equal(fixture.creates[0].metadata.key, source.cohort.metadataKey)
  assert.equal(fixture.creates[0].blocks.secretAccessKey, source.blocks.secretAccessKey)
  assert.equal(JSON.stringify(arm).includes("private"), false)
  await arm.cleanupOriginal()
})

for (const layout of ["legacy", "compact"]) {
  test(`${layout} prepares preflight and exclusive canary before a once-only runner handoff`, async () => {
    const fixture = model(layout), { arm, opened } = await ready(fixture)
    const options = fixture.creates[0]
    assert.equal(options.metadata.kind, "tidb")
    assert.equal(options.blocks.kind, "rustfs")
    assert.equal(options.chunkSize, 65536)
    assert.equal(options.owner, fixture.input.cohort.owner)
    for (const flag of ["concurrentWrites", "inodeUpdates", "compactInodeUpdates"]) assert.equal(options[flag], layout === "compact")
    deeplyFrozen(options)
    assert.equal(opened.filesystem, fixture.filesystems[0])
    assert.equal(fixture.creates.length, 1)
    assert.deepEqual(fixture.events.map(([name]) => name), ["preflight", "create", "empty", "open", "write", "sync", "handle-close", "inspect"])
    assert.deepEqual(fixture.events.find(([name]) => name === "open").slice(2), [canaryPath, "wx"])
    assert.equal(canaryPath.startsWith("/.mount-rs-storage-"), false)
    const snapshot = arm.snapshot()
    assert.equal(snapshot.canary.payload_bytes, 4096)
    assert.equal(snapshot.canary.created, true)
    assert.equal(snapshot.counts.canary_bytes_written, 4096)
    assert.equal(snapshot.counts.initial_create_calls, 1)
    assert.equal(snapshot.layout_observations.before_timed.kind, layout === "compact" ? "MRC5" : "recognized_non_mrc5")
    if (layout === "legacy") assert.equal(snapshot.layout_observations.before_timed.exact_legacy_marker, false)
    assert.throws(() => arm.takeForRunner(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
    await opened.cleanup()
  })
}

test("explicit configuration rejects R2, key drift, missing fields and accessors before any API call", () => {
  const makeArm = implementation()
  for (const mutate of [
    (input) => { input.blocks.kind = "r2" },
    (input) => { delete input.blocks.region },
    (input) => { input.metadata.key = "foreign/key" },
    (input) => { input.cohort.blockPrefix = "comparison//blocks" },
    (input) => { input.cohort.layout = "inode" },
    (input) => { input.metadata.extra = "private" },
    (input) => { input.blocks.durable = "false" },
    (input) => { Object.defineProperty(input.blocks, "secretAccessKey", { get() { assert.fail("config getter must not execute") } }) },
  ]) {
    const fixture = model()
    mutate(fixture.input)
    assert.throws(() => makeArm(fixture.binding, fixture.input), fixedFailure("OWNED_LAYOUT_ARM_CONFIG_INVALID"))
    assert.equal(fixture.events.length, 0)
  }
})

test("contamination and rejected preflight cannot create a filesystem or be retried", async () => {
  const contaminated = presence(); contaminated.metadata.row_presence.inodes = true
  for (const behavior of [{ presence: contaminated }, { preflightError: true }]) {
    const fixture = model("compact", behavior), arm = implementation()(fixture.binding, fixture.input)
    await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_PREFLIGHT_FAILED"))
    await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
    assert.equal(fixture.creates.length, 0)
    assert.equal(fixture.events.filter(([name]) => name === "preflight").length, 1)
    assert.throws(() => arm.takeForRunner(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  }
})

test("a pending preflight prevents creation, shutdown and concurrent prepare", async () => {
  const gate = deferred(), fixture = model("compact", { preflightGate: gate })
  const arm = implementation()(fixture.binding, fixture.input), preparing = arm.prepare()
  await Promise.resolve()
  assert.equal(fixture.creates.length, 0)
  await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  await assert.rejects(() => arm.cleanupOriginal(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  gate.resolve()
  await preparing
  assert.equal(fixture.creates.length, 1)
  await arm.cleanupOriginal()
})

test("initial factory failure remains redacted with no handoff or fresh reopen", async () => {
  const fixture = model("compact", { factoryErrorAt: 0 }), arm = implementation()(fixture.binding, fixture.input)
  await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_CREATE_FAILED"))
  assert.equal(fixture.creates.length, 1)
  assert.equal(fixture.filesystems.length, 0)
  assert.throws(() => arm.takeForRunner(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
})

test("setup failures await acquired handle shutdown and preserve independent cleanup failures", async () => {
  for (const behavior of [{ shortWrite: true }, { writeError: true }, { syncError: true }]) {
    const fixture = model("compact", behavior), arm = implementation()(fixture.binding, fixture.input)
    await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED"))
    assert.equal(fixture.events.filter(([name]) => name === "handle-close").length, 1)
    assert.equal(fixture.events.filter(([name]) => name === "shutdown").length, 1)
    assert.equal(fixture.filesystems[0].closed, true)
    assert.equal(arm.snapshot().original_shutdown_confirmed, true)
    assert.throws(() => arm.takeForRunner(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  }
  const fixture = model("compact", { writeError: true, handleCloseError: "wx", shutdownErrorAt: 0 })
  const arm = implementation()(fixture.binding, fixture.input)
  await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED", ["OWNED_LAYOUT_ARM_HANDLE_CLOSE_FAILED", "OWNED_LAYOUT_ARM_SHUTDOWN_FAILED"]))
  assert.equal(arm.snapshot().original_shutdown_confirmed, false)
})

test("malformed canary write results cannot reach handoff and setup rejection waits for acquired shutdown", async () => {
  for (const writeResult of [
    () => ({ bytesWritten: 4096, buffer: "private result" }),
    () => ({ bytesWritten: 4096, buffer: new Uint8Array(4095) }),
    () => ({ bytesWritten: 4096, buffer: new Uint8Array(4096) }),
  ]) {
    const gate = deferred(), fixture = model("compact", { writeResult, shutdownGateAt: 0, shutdownGate: gate })
    const arm = implementation()(fixture.binding, fixture.input)
    let settled = false
    const preparing = arm.prepare().finally(() => { settled = true })
    const rejected = assert.rejects(preparing, fixedFailure("OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED"))
    try {
      for (let index = 0; index < 20; index++) await Promise.resolve()
      assert.equal(fixture.events.filter(([name]) => name === "shutdown").length, 1)
      assert.equal(settled, false)
      assert.throws(() => arm.takeForRunner(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
    } finally {
      gate.resolve()
      await rejected
    }
    assert.equal(fixture.filesystems[0].closed, true)
    assert.equal(arm.snapshot().original_shutdown_confirmed, true)
  }
})

test("layout proof failures clean the acquired original and cannot certify legacy from a nonnull marker", async () => {
  for (const layout of ["legacy", "compact"]) {
    const fixture = model(layout, { proof: () => ({ schema: "private", marker: "MRC4" }) })
    const arm = implementation()(fixture.binding, fixture.input)
    await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_LAYOUT_PROOF_FAILED"))
    assert.equal(fixture.filesystems[0].closed, true)
    assert.equal(fixture.creates.length, 1)
  }
})

test("original shutdown is shared once and persistence cannot start before confirmed close", async () => {
  const gate = deferred(), fixture = model("compact", { shutdownGateAt: 0, shutdownGate: gate })
  const { arm, opened } = await ready(fixture)
  const closing = opened.cleanup()
  assert.equal(arm.cleanupOriginal(), closing)
  await Promise.resolve(); await Promise.resolve()
  await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  assert.equal(fixture.creates.length, 1)
  gate.resolve()
  await closing
  await arm.cleanupOriginal()
  assert.equal(fixture.events.filter(([name, index]) => name === "shutdown" && index === 0).length, 1)
  assert.equal(arm.snapshot().original_shutdown_confirmed, true)
})

test("failed original shutdown stays failed and forbids all fresh opens", async () => {
  const fixture = model("compact", { shutdownErrorAt: 0 }), { arm, opened } = await ready(fixture)
  await assert.rejects(() => opened.cleanup(), fixedFailure("OWNED_LAYOUT_ARM_SHUTDOWN_FAILED"))
  await assert.rejects(() => arm.cleanupOriginal(), fixedFailure("OWNED_LAYOUT_ARM_SHUTDOWN_FAILED"))
  await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  assert.equal(fixture.creates.length, 1)
  assert.equal(fixture.events.filter(([name]) => name === "shutdown").length, 1)
})

for (const layout of ["legacy", "compact"]) {
  test(`${layout} persists full canary bytes and EOF through config-only fresh reopens then proves logical root empty`, async () => {
    const fixture = model(layout), { arm, opened } = await ready(fixture)
    const before = arm.snapshot().layout_observations.before_timed
    for (let index = 0; index < 3; index++) {
      const path = `/.mount-rs-storage-modeled-${index}.bin`
      const handle = await opened.filesystem.open(path, "wx")
      await handle.write(new Uint8Array(4096), 0, 4096, 0)
      await handle.close()
      await opened.filesystem.unlink(path)
    }
    await opened.cleanup()
    fixture.binding.createChunkedDriver = () => assert.fail("selected public factory must remain pinned")
    const result = await arm.verifyPersistence()
    deeplyFrozen(result)
    assert.equal(result.complete, true)
    assert.equal(result.canary.full_bytes_verified, true)
    assert.equal(result.canary.eof_verified, true)
    assert.equal(result.canary.remove_confirmed, true)
    assert.equal(result.canary.logical_root_empty, true)
    assert.equal(result.counts.preflight_calls, 1)
    assert.equal(result.counts.initial_create_calls, 1)
    assert.equal(result.counts.reopen_create_calls, 2)
    assert.equal(result.counts.original_shutdown_calls, 1)
    assert.equal(result.counts.reopen_shutdown_calls, 2)
    assert.equal(result.counts.canary_bytes_written, 4096)
    assert.equal(result.counts.canary_bytes_read, 4096)
    assert.equal(result.counts.canary_eof_bytes_read, 0)
    assert.equal(result.counts.canary_remove_calls, 1)
    assert.equal(result.counts.layout_inspection_calls, 5)
    assert.equal(fixture.creates.length, 3)
    assert.equal(new Set(fixture.filesystems).size, 3)
    assert.equal(fixture.filesystems.every((filesystem) => filesystem.closed), true)
    for (const options of fixture.creates) assert.deepEqual(options, fixture.creates[0])
    assert.notEqual(fixture.creates[1], fixture.creates[0])
    assert.equal(JSON.stringify(result).includes("private"), false)
    assert.equal(JSON.stringify(result).includes(fixture.input.cohort.metadataKey), false)
    if (layout === "compact") {
      const observations = result.layout_observations
      assert.ok(BigInt(observations.drained_original.receipt.structuralGeneration) > BigInt(before.receipt.structuralGeneration))
      assert.deepEqual(observations.reopened.receipt, observations.drained_original.receipt)
      assert.ok(BigInt(observations.removed_drained.receipt.structuralGeneration) > BigInt(observations.reopened.receipt.structuralGeneration))
      assert.deepEqual(observations.empty_reopened.receipt, observations.removed_drained.receipt)
    } else {
      for (const observation of Object.values(result.layout_observations)) assert.equal(observation.exact_legacy_marker, false)
    }
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  })
}

test("full-byte corruption, short reads and extra data stop after closing the first fresh handle", async () => {
  for (const behavior of [{ corrupt: true }, { shortRead: true }, { extraRead: true }, { readError: true }]) {
    const fixture = model("compact", behavior), { arm, opened } = await ready(fixture)
    await opened.cleanup()
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_CANARY_READ_FAILED"))
    assert.equal(fixture.creates.length, 2)
    assert.equal(fixture.filesystems[1].closed, true)
    assert.equal(fixture.events.some(([name]) => name === "unlink"), false)
  }
})

test("EOF and read-handle-close failures preserve failure and close the first reopened filesystem", async () => {
  for (const behavior of [{ eofNonzero: true }, { eofError: true }]) {
    const fixture = model("compact", behavior), { arm, opened } = await ready(fixture)
    await opened.cleanup()
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_CANARY_EOF_FAILED"))
    assert.equal(fixture.readHandles.length, 1)
    assert.equal(fixture.events.filter(([name, index, , flags]) => name === "handle-close" && index === 1 && flags === "r").length, 1)
    assert.equal(fixture.filesystems[1].closed, true)
  }
  const fixture = model("compact", { eofError: true, handleCloseError: "r", shutdownErrorAt: 1 }), { arm, opened } = await ready(fixture)
  await opened.cleanup()
  await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_CANARY_EOF_FAILED", ["OWNED_LAYOUT_ARM_HANDLE_CLOSE_FAILED", "OWNED_LAYOUT_ARM_SHUTDOWN_FAILED"]))
})

test("failed canary removal stops before empty reopen and still closes the acquired fresh filesystem", async () => {
  const fixture = model("compact", { unlinkError: true }), { arm, opened } = await ready(fixture)
  await opened.cleanup()
  await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_CANARY_REMOVE_FAILED"))
  assert.equal(fixture.creates.length, 2)
  assert.equal(fixture.filesystems[1].closed, true)
  assert.equal(arm.snapshot().canary.remove_confirmed, false)
})

test("only a bounded empty array proves the final logical root empty", async () => {
  for (const behavior of [{ nonemptyAt: 2 }, { listShapeAt: 2 }]) {
    const fixture = model("compact", behavior), { arm, opened } = await ready(fixture)
    await opened.cleanup()
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_EMPTY_PROOF_FAILED"))
    assert.equal(fixture.filesystems[2].closed, true)
    assert.equal(arm.snapshot().canary.logical_root_empty, false)
  }
})

test("regressed or changed backing at the drained original boundary still shuts down and refuses reopen", async () => {
  for (const change of ["generation", "backing"]) {
    const fixture = model(), { arm, opened } = await ready(fixture)
    if (change === "generation") fixture.store.generation = 0n
    else fixture.store.backingId = "abcdef1234567890abcdef1234567890"
    await assert.rejects(() => opened.cleanup(), fixedFailure("OWNED_LAYOUT_ARM_GENERATION_CONTINUITY_FAILED"))
    assert.equal(fixture.filesystems[0].closed, true)
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
    assert.equal(fixture.creates.length, 1)
  }
})

test("immediate fresh reopen must match the drained generation and backing exactly", async () => {
  for (const change of ["generation", "backing"]) {
    const fixture = model("compact", { proof: (index, receipt) => index === 1 ? { ...receipt,
      ...(change === "generation" ? { structuralGeneration: String(BigInt(receipt.structuralGeneration) + 1n) } : { backingId: "abcdef1234567890abcdef1234567890" }) } : receipt })
    const { arm, opened } = await ready(fixture)
    await opened.cleanup()
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_GENERATION_CONTINUITY_FAILED"))
    assert.equal(fixture.creates.length, 2)
    assert.equal(fixture.filesystems[1].closed, true)
    assert.equal(fixture.events.some(([name]) => name === "read"), false)
  }
})

test("post-remove and final reopen continuity cannot regress or change", async () => {
  for (const boundary of ["removed", "empty"]) {
    let firstReopenInspections = 0
    const fixture = model("compact", { proof: (index, receipt) => {
      if (index === 1) firstReopenInspections++
      if (boundary === "removed" && index === 1 && firstReopenInspections === 2) return { ...receipt, structuralGeneration: "0" }
      if (boundary === "empty" && index === 2) return { ...receipt, structuralGeneration: String(BigInt(receipt.structuralGeneration) + 1n) }
      return receipt
    } })
    const { arm, opened } = await ready(fixture)
    await opened.cleanup()
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_GENERATION_CONTINUITY_FAILED"))
    assert.equal(fixture.filesystems.at(-1).closed, true)
    assert.equal(arm.snapshot().complete, false)
  }
})

test("fresh reopen cannot reuse the original handle or silently recover a factory failure", async () => {
  for (const behavior of [{ reuseAt: 1 }, { factoryErrorAt: 1 }]) {
    const fixture = model("compact", behavior), { arm, opened } = await ready(fixture)
    await opened.cleanup()
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure(behavior.reuseAt ? "OWNED_LAYOUT_ARM_REOPEN_NOT_FRESH" : "OWNED_LAYOUT_ARM_REOPEN_FAILED"))
    assert.equal(fixture.creates.length, 2)
    assert.equal(fixture.events.filter(([name, index]) => name === "shutdown" && index === 0).length, 1)
    await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_STATE_INVALID"))
  }
})

test("a failed fresh shutdown blocks the second reopen and remains incomplete", async () => {
  const fixture = model("compact", { shutdownErrorAt: 1 }), { arm, opened } = await ready(fixture)
  await opened.cleanup()
  await assert.rejects(() => arm.verifyPersistence(), fixedFailure("OWNED_LAYOUT_ARM_SHUTDOWN_FAILED"))
  assert.equal(fixture.creates.length, 2)
  assert.equal(arm.snapshot().complete, false)
})

test("missing APIs cannot drop an acquired original without a shutdown attempt", async () => {
  const fixture = model("compact", { configure: (index, filesystem) => { if (index === 0) filesystem.open = undefined } })
  const arm = implementation()(fixture.binding, fixture.input)
  await assert.rejects(() => arm.prepare(), fixedFailure("OWNED_LAYOUT_ARM_API_UNAVAILABLE"))
  assert.equal(fixture.filesystems[0].closed, true)
  const absent = model(), missing = implementation()({}, absent.input)
  await assert.rejects(() => missing.prepare(), fixedFailure("OWNED_LAYOUT_ARM_API_UNAVAILABLE"))
  assert.equal(absent.events.length, 0)
})
