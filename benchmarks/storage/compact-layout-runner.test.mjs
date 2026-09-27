import assert from "node:assert/strict"
import { test } from "node:test"

import { parseArgs, runBenchmark } from "./runner.mjs"

const receipt = () => ({
  schema: "mount-rs.compact-layout-receipt.v1", marker: "MRC5",
  backingId: "1234567890abcdef1234567890abcdef", structuralGeneration: "7",
  blockAuthorityVerified: true,
})

function model(inspect) {
  const events = []
  let stored
  const filesystem = {
    async writeFile(_path, value) { events.push("write"); stored = Buffer.from(value) },
    async readFile() { events.push("read"); return stored },
    async unlink() { events.push("unlink"); stored = undefined },
    ...(inspect === undefined ? {} : { async inspectCompactLayout() { events.push("inspect"); return inspect.call(filesystem) } }),
  }
  const definition = {
    id: "mount-rs-split-model", implementation: "model", binding: "model", backend: "model",
    chunking: { algorithm: "fixed-size" },
    availability() { return { configured: true } },
    async create() { events.push("create"); return { filesystem, async cleanup() { events.push("cleanup") } } },
  }
  return { events, filesystem, definitions: new Map([[definition.id, definition]]) }
}

function run(state, overrides = {}) {
  return runBenchmark({
    ...parseArgs([]), layout: "compact", providers: ["mount-rs-split-model"],
    sizes: [1], payloadBytes: 3, iterations: 1, concurrency: 1,
    timeoutMs: 100, cleanupTimeoutMs: 50, ...overrides,
  }, {}, state.definitions)
}

for (const [name, inspect, code] of [
  ["missing API", undefined, "COMPACT_LAYOUT_UNAVAILABLE"],
  ["validated noncompact", () => null, "COMPACT_LAYOUT_UNAVAILABLE"],
  ["malformed receipt", () => ({ ...receipt(), marker: "MRC4" }), "COMPACT_LAYOUT_INVALID"],
  ["rejected query", () => { throw new Error("private SQL credential") }, "COMPACT_LAYOUT_QUERY_FAILED"],
]) {
  test(`${name} fails before I/O and closes the opened provider`, async () => {
    const state = model(inspect)
    const result = await run(state)
    const provider = result.providers[0]
    assert.equal(result.status, "failed")
    assert.equal(provider.layoutInspectionError.code, code)
    assert.equal(provider.layoutSelection.persistedMarkerEvidence, "not-observed-by-benchmark-runner")
    assert.equal(provider.layoutSelection.persistedReceipt, undefined)
    assert.equal(provider.sizes[0].failures[0].operation, "layout-proof")
    assert.equal(provider.cleanup.resource.status, "ok")
    assert.equal(state.events.filter((event) => event === "cleanup").length, 1)
    assert.equal(state.events.some((event) => ["write", "read", "unlink"].includes(event)), false)
    assert.equal(JSON.stringify(provider).includes("private SQL"), false)
  })
}

test("persisted proof completes before any timed I/O and preserves the three-operation numerator", async () => {
  let entered, release
  const inspecting = new Promise((resolve) => { entered = resolve })
  const pending = new Promise((resolve) => { release = resolve })
  const state = model(() => { entered(); return pending })
  const running = run(state)
  await Promise.race([inspecting, running.then(() => assert.fail("workload completed without persisted inspection"))])
  assert.deepEqual(state.events, ["create", "inspect"])
  release(receipt())
  const result = await running
  assert.equal(result.status, "ok")
  const provider = result.providers[0]
  assert.equal(provider.layoutSelection.persistedMarkerEvidence, "metadata-MRC5-and-matching-block-authority-observed")
  assert.deepEqual(provider.layoutSelection.persistedReceipt, receipt())
  assert.equal(provider.sizes[0].summary.successfulOperations, 3)
  assert.equal(provider.sizes[0].summary.successfulIterations, 1)
  assert.deepEqual(state.events, ["create", "inspect", "write", "read", "unlink", "cleanup"])
})

test("pending inspection timeout retains uncertainty and defers provider shutdown", async () => {
  let release
  const pending = new Promise((resolve) => { release = resolve })
  const state = model(() => pending)
  const result = await run(state, { timeoutMs: 5, cleanupTimeoutMs: 5 })
  const provider = result.providers[0]
  assert.equal(result.status, "failed")
  assert.equal(provider.layoutInspectionError.code, "BENCHMARK_TIMEOUT")
  assert.equal(provider.layoutInspectionLateOperation, "pending")
  assert.equal(provider.cleanup.resource.status, "deferred")
  assert.deepEqual(provider.cleanup.pendingOperations, [{ path: null, operation: "compact layout inspection" }])
  assert.deepEqual(state.events, ["create", "inspect"])
  release(receipt())
  await pending
})

test("late settled inspection keeps its deadline failure while allowing safe shutdown", async () => {
  const state = model(() => new Promise((resolve) => setTimeout(() => resolve(receipt()), 20)))
  const result = await run(state, { timeoutMs: 5, cleanupTimeoutMs: 100 })
  const provider = result.providers[0]
  assert.equal(result.status, "failed")
  assert.equal(provider.layoutInspectionError.code, "BENCHMARK_TIMEOUT")
  assert.equal(provider.layoutInspectionLateOperation, "fulfilled")
  assert.equal(provider.layoutSelection.persistedReceipt, undefined)
  assert.equal(provider.cleanup.resource.status, "ok")
  assert.deepEqual(state.events, ["create", "inspect", "cleanup"])
})

test("legacy runs preserve the existing workload and never query compact layout", async () => {
  const state = model(() => assert.fail("legacy inspection"))
  const result = await run(state, { layout: "legacy" })
  assert.equal(result.status, "ok")
  assert.equal(result.providers[0].layoutSelection, undefined)
  assert.deepEqual(state.events, ["create", "write", "read", "unlink", "cleanup"])
})
