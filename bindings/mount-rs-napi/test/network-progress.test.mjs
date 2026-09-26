import assert from "node:assert/strict"
import { test } from "node:test"
import { createNetworkProgress } from "./network-progress.mjs"

function control(overrides = {}) {
  const state = { lines: [], cleared: [], samples: 0, clocks: 0, now: 10, cpu: 100, callback: null }
  const observer = createNetworkProgress({
    provider: "sqlite", concurrency: 32,
    sample: () => { state.samples++; return { cpu_user_us: state.cpu, cpu_system_us: 10, rss_current_bytes: 1024, rss_lifetime_peak_bytes: 2048 } },
    clock: () => { state.clocks++; return { unix_ms: 1000 + state.now, monotonic_ms: state.now } },
    write: (line) => { state.lines.push(line); return true },
    schedule: (callback, interval) => { assert.equal(interval, 5000); state.callback = callback; return 1 },
    cancel: (handle) => state.cleared.push(handle),
    ...overrides,
  })
  return { state, observer }
}

test("first rejected request freezes pending counts and clears the timer", () => {
  const { state, observer } = control()
  observer.begin("concurrent PUT fetch")
  observer.begin("concurrent PUT fetch")
  state.cpu = 170
  observer.settle("concurrent PUT fetch", "rejected", 1, new DOMException("private error", "TimeoutError"))
  const final = observer.receipt()
  const counts = [state.samples, state.clocks, state.lines.length]
  observer.settle("concurrent PUT fetch", "fulfilled", 0)
  observer.finish("passed")
  state.callback()
  assert.equal(observer.receipt(), final)
  assert.equal(final.status, "failed")
  assert.equal(final.terminal, true)
  assert.equal(final.failure_category, "timeout")
  assert.equal(final.failure_stage, "concurrent PUT fetch")
  assert.equal(final.failure_index, 1)
  assert.equal(final.cpu_user_us, 70)
  assert.equal(final.rows[0].in_flight, 1)
  assert.equal(final.rows[0].rejected, 1)
  assert.deepEqual(state.cleared, [1])
  assert.deepEqual([state.samples, state.clocks, state.lines.length], counts)
  assert.equal(state.lines.join("").includes("private error"), false)
  assert.throws(() => { final.rows[0].in_flight = 0 }, TypeError)
})

test("event recording adds no clocks, samples or publication", () => {
  const { state, observer } = control()
  const before = [state.samples, state.clocks, state.lines.length]
  observer.begin("concurrent GET fetch")
  observer.settle("concurrent GET fetch", "fulfilled", 0)
  assert.deepEqual([state.samples, state.clocks, state.lines.length], before)
  state.cpu = 220
  state.now = 5010
  state.callback()
  observer.finish("passed")
  const receipt = observer.receipt()
  assert.equal(receipt.schema, "mount-rs.network-test-progress.v1")
  assert.equal(receipt.status, "passed")
  assert.equal(receipt.cpu_user_us, 120)
  assert.equal(receipt.rss_current_bytes, 1024)
  assert.equal(receipt.rss_lifetime_peak_bytes, 2048)
  assert.equal(receipt.rows.length, 8)
  assert.equal(receipt.rows[2].fulfilled, 1)
  assert.equal(receipt.rows[2].in_flight, 0)
  assert.equal(receipt.application_drain_proven, false)
  assert.equal(receipt.capture_atomic, false)
  assert.equal(receipt.failure_category, null)
  assert.deepEqual(state.cleared, [1])
  for (const line of state.lines) {
    assert.ok(line.startsWith("network_test_progress "))
    assert.ok(Buffer.byteLength(line) <= 4096)
    assert.doesNotThrow(() => JSON.parse(line.slice("network_test_progress ".length)))
  }
})

test("disabled observers return before reading any source or scheduling", () => {
  const forbidden = () => assert.fail("disabled observer touched a source")
  const observer = createNetworkProgress({ enabled: false, sample: forbidden, clock: forbidden, write: forbidden, schedule: forbidden, cancel: forbidden })
  observer.begin("private stage")
  observer.settle("private stage", "rejected", 0, new Error("secret"))
  observer.finish("failed")
  assert.equal(observer.receipt(), null)
})

for (const [name, category] of [["AbortError", "aborted"], ["private error name", "request_error"]]) {
  test(`failure projects fixed category ${category}`, () => {
    const { state, observer } = control()
    observer.begin("streamed PUT fetch")
    observer.settle("streamed PUT fetch", "rejected", "/private/path", { name, message: "private payload" })
    assert.equal(observer.receipt().failure_index, null)
    assert.equal(observer.receipt().failure_category, category)
    assert.equal(state.lines.join("").includes("private"), false)
  })
}

for (const value of [new Error("private resource error"), { cpu_user_us: NaN }, { cpu_user_us: 1, cpu_system_us: -1, rss_current_bytes: 1, rss_lifetime_peak_bytes: 2 }]) {
  test("failed or invalid resource readings are unavailable without changing completion", () => {
    const { state, observer } = control({ sample: () => { if (value instanceof Error) throw value; return value } })
    assert.doesNotThrow(() => observer.finish("passed"))
    assert.equal(observer.receipt().status, "passed")
    assert.equal(observer.receipt().available, false)
    assert.equal(observer.receipt().reason, "resource_unavailable")
    assert.equal(observer.receipt().cpu_user_us, null)
    assert.equal(observer.receipt().rss_current_bytes, null)
    assert.equal(state.lines.join("").includes("private resource error"), false)
  })
}

for (const sink of [() => { throw new Error("private sink error") }, () => false]) {
  test("sink failure or backpressure clears the timer and preserves workload outcome", () => {
    const { observer, state } = control({ write: sink })
    assert.equal(observer.receipt().available, false)
    assert.match(observer.receipt().reason, /^sink_(?:failed|backpressure)$/u)
    assert.doesNotThrow(() => { observer.begin("concurrent PUT fetch"); observer.settle("concurrent PUT fetch", "rejected", 0, new Error("original")) })
    assert.equal(observer.receipt().status, "failed")
    assert.equal(observer.receipt().available, false)
    assert.match(observer.receipt().reason, /^sink_(?:failed|backpressure)$/u)
    assert.equal(state.callback, null)
  })
}

test("invalid events remain redacted and do not throw through request code", () => {
  const { state, observer } = control()
  assert.doesNotThrow(() => { observer.begin("PRIVATE_STAGE"); observer.settle("concurrent PUT fetch", "fulfilled", 0); observer.finish("passed") })
  assert.equal(observer.receipt().status, "passed")
  assert.equal(observer.receipt().available, false)
  assert.equal(observer.receipt().reason, "invalid_event")
  assert.equal(state.lines.join("").includes("PRIVATE_STAGE"), false)
})

test("publication count is bounded even when cleanup never finishes", () => {
  const { state, observer } = control()
  for (let index = 0; index < 100; index++) { state.now += 5000; state.callback() }
  observer.finish("failed")
  assert.ok(state.lines.length <= 16)
  assert.equal(observer.receipt().status, "failed")
  assert.equal(observer.receipt().available, false)
  assert.equal(observer.receipt().reason, "publication_cap")
  assert.deepEqual(state.cleared, [1])
})

test("clock and timer failures cannot replace completion", () => {
  for (const overrides of [{ clock: () => { throw new Error("private clock") } }, { schedule: () => { throw new Error("private timer") } }]) {
    const { observer, state } = control(overrides)
    assert.equal(observer.receipt().available, false)
    assert.doesNotThrow(() => observer.finish("passed"))
    assert.equal(observer.receipt().status, "passed")
    assert.equal(observer.receipt().available, false)
    assert.equal(state.lines.join("").includes("private"), false)
  }
})

test("resource and clock baselines do not retain mutable hook objects", () => {
  const sample = { cpu_user_us: 100, cpu_system_us: 10, rss_current_bytes: 1024, rss_lifetime_peak_bytes: 2048 }
  const clock = { unix_ms: 1000, monotonic_ms: 10 }
  const { observer } = control({ sample: () => sample, clock: () => clock })
  sample.cpu_user_us += 70
  sample.cpu_system_us += 20
  clock.monotonic_ms += 5000
  clock.unix_ms += 5000
  observer.finish("passed")
  assert.equal(observer.receipt().cpu_user_us, 70)
  assert.equal(observer.receipt().cpu_system_us, 20)
  assert.equal(observer.receipt().elapsed_ms, 5000)
})

test("largest supported counters retain the closed bounded schema", () => {
  const { observer, state } = control({
    concurrency: 64,
    sample: () => ({ cpu_user_us: Number.MAX_SAFE_INTEGER, cpu_system_us: Number.MAX_SAFE_INTEGER,
      rss_current_bytes: Number.MAX_SAFE_INTEGER, rss_lifetime_peak_bytes: Number.MAX_SAFE_INTEGER,
      private_path: "/private/secret" }),
    clock: () => ({ unix_ms: Number.MAX_SAFE_INTEGER, monotonic_ms: Number.MAX_SAFE_INTEGER }),
  })
  for (const row of observer.receipt().rows) {
    for (let index = 0; index < 64; index++) observer.begin(row.stage)
  }
  observer.finish("passed")
  const final = observer.receipt()
  assert.deepEqual(Object.keys(final), [
    "schema", "pid", "platform", "provider", "concurrency", "sequence",
    "published_unix_ms", "elapsed_ms", "cadence_ms", "status", "terminal", "available", "reason",
    "cpu_user_us", "cpu_system_us", "rss_current_bytes", "rss_lifetime_peak_bytes",
    "cpu_scope", "rss_scope", "requests_scope", "application_drain_proven", "capture_atomic",
    "failure_stage", "failure_index", "failure_category", "rows",
  ])
  for (const row of final.rows) {
    assert.deepEqual(Object.keys(row), ["stage", "issued", "fulfilled", "rejected", "in_flight"])
    assert.equal(row.in_flight, 64)
  }
  for (const line of state.lines) assert.ok(Buffer.byteLength(line) <= 4096)
  assert.equal(state.lines.join("").includes("private"), false)
})
