import assert from "node:assert/strict"
import test from "node:test"
import { createRequire, Module } from "node:module"
import { fileURLToPath } from "node:url"
import { parseArgs, runBenchmark } from "./runner.mjs"
import { providerById } from "./providers.mjs"
import { STORAGE_OPERATION_NAMES, STORAGE_OPERATION_FAMILIES, STORAGE_INSTRUMENTED_OPERATION_NAMES, STORAGE_CALL_SEMANTICS, STORAGE_BYTE_SEMANTICS, STORAGE_ROW_SEMANTICS, TIDB_DIAGNOSTIC_COVERAGE } from "./diagnostics.mjs"

const require = createRequire(import.meta.url)
const capturePath = fileURLToPath(new URL("./capture-native.cjs", import.meta.url))
assert.equal(process.env.NAPI_RS_NATIVE_LIBRARY_PATH, capturePath, "launch this suite with the explicit capture binding before benchmark metadata can load a binding")
for (const key of ["NAPI_RS_FORCE_WASI", "NAPI_RS_WASI_FLAVOR", "NODE_PATH"]) assert.equal(process.env[key], undefined, `${key} must be unset for this capture-only suite`)
const nativeExtension = Module._extensions[".node"]
Module._extensions[".node"] = () => { throw new Error("real native addon loading is forbidden in the backing observer suite") }
test.after(() => {
  assert.equal(require("../../bindings/mount-rs-napi/index.js").__napiBindingTarget, "storage-benchmark-capture")
  assert.equal(Object.keys(require.cache).some((path) => path.endsWith(".node")), false)
  Module._extensions[".node"] = nativeExtension
})

// Import absence is reported as an assertion, while runner controls exercise the
// existing runner before the hook implementation exists.
const observerModule = await import("./backing-observer.mjs").catch((error) => {
  if (error.code === "ERR_MODULE_NOT_FOUND") return null
  throw error
})
function api() {
  assert.ok(observerModule, "the backing observer protocol must exist")
  return observerModule
}

const cid = "a".repeat(64)
const secondCid = "b".repeat(64)
const image = `sha256:${"c".repeat(64)}`
const owned = { cid, role: "tidb", labels: { "mount-rs.tidb.run": "owned-run" } }
const secondOwned = { cid: secondCid, role: "tikv-0", labels: { "mount-rs.tidb.run": "owned-run" } }
const secret = "EXCLUDED_CREDENTIAL_SENTINEL"

function clock() {
  let now = 0
  let id = 0
  const timers = new Map()
  return {
    now: () => now,
    utc: () => new Date(1_800_000_000_000 + now).toISOString(),
    cpu: () => ({ user: now * 10, system: now * 2 }),
    setTimeout(callback, ms) { const key = ++id; timers.set(key, { callback, at: now + ms }); return key },
    clearTimeout(key) { timers.delete(key) },
    advance(ms, fire = true) {
      now += ms
      if (fire) for (const [key, timer] of [...timers]) {
        if (timer.at <= now) { timers.delete(key); timer.callback() }
      }
    },
    timers,
  }
}

function inspect(id = cid, edits = {}) {
  return JSON.stringify({
    Id: id, Image: image, RestartCount: 0,
    State: { Running: true, StartedAt: "2026-09-26T00:00:00Z" },
    Config: { Env: [`PASSWORD=${secret}`], Cmd: [secret], Labels: { ...owned.labels, secret } },
    HostConfig: { Memory: 0, MemorySwap: -1, NanoCpus: 0, CpuQuota: -1, CpuPeriod: 0, CpuShares: 0, CpusetCpus: "", PidsLimit: 0 },
    Mounts: [{ Source: secret }], ...edits,
  })
}

function stats({ id = cid, total = "9007199254740993", second = 0, bytes = "0", operations = "0", omitted = [], extraEntries = [] } = {}) {
  const data = {
    id, read: `2026-09-26T00:00:${String(second).padStart(2, "0")}.123456789Z`,
    cpu_stats: { cpu_usage: { total_usage: "TOKEN" }, system_cpu_usage: 0, online_cpus: 8 },
    memory_stats: { usage: 0, limit: 1024 },
    blkio_stats: {
      io_service_bytes_recursive: [{ major: 8, minor: 0, op: "Read", value: "BYTES" }, { major: 8, minor: 0, op: "Write", value: 0 }, { major: 8, minor: 0, op: "Total", value: 999 }, ...extraEntries],
      io_serviced_recursive: [{ major: 8, minor: 0, op: "Read", value: "OPS" }, { major: 8, minor: 0, op: "Write", value: 0 }],
    },
    additive_unknown: secret,
  }
  for (const field of omitted) delete data.blkio_stats[field]
  return JSON.stringify(data).replace('"TOKEN"', total).replace('"BYTES"', bytes).replace('"OPS"', operations)
}

function transportFor(fakeClock, handler = () => undefined) {
  const requests = []
  return {
    requests,
    async request(request) {
      requests.push(request)
      const override = await handler(request, requests.length)
      if (override) return override
      let body
      if (request.path === "/version") body = '{"ApiVersion":"1.51","MinAPIVersion":"1.24","Os":"linux"}'
      else if (request.path.endsWith("/json")) body = inspect(request.cid)
      else body = stats({ id: request.cid, second: Math.floor(fakeClock.now() / 1000) })
      return { status: 200, body: (async function* () { yield Buffer.from(body) })() }
    },
  }
}

function factory(fakeClock, transport, allowlist = [owned], caps = {}) {
  return api().createBackingObserver({ allowlist, transport, clock: fakeClock, caps })
}

function provider({ setupError, configured = true, revisionMismatch = false, hooks = {}, tick = () => {} } = {}) {
  const definition = { ...providerById({}).get("mount-rs-memory") }
  const files = new Map()
  definition.availability = () => ({ configured, revisionMismatch, reason: "controlled-unavailable" })
  definition.create = async () => {
    hooks.create?.()
    tick(5)
    if (setupError) throw setupError
    return {
      filesystem: {
        async writeFile(path, payload) { hooks.write?.(); tick(10); files.set(path, Buffer.from(payload)) },
        async readFile(path) { tick(10); return files.get(path) },
        async unlink(path) { tick(10); files.delete(path) },
      },
      async cleanup() { hooks.cleanup?.(); tick(5) },
    }
  }
  return new Map([[definition.id, definition]])
}
const options = () => parseArgs(["--provider", "mount-rs-memory", "--sizes", "1", "--payload-bytes", "1", "--iterations", "1", "--timeout-ms", "10", "--cleanup-timeout-ms", "10"])
function recorder(events, fakeClock = clock(), failAt) {
  return {
    async beginPhase(meta) { events.push(`begin:${meta.name}`); fakeClock.advance(100); if (failAt === "begin") throw new Error(secret) },
    async endPhase(meta) { events.push(`end:${meta.name}:${meta.quiescent}`); fakeClock.advance(100); if (failAt === "end") throw new Error(secret) },
    async finalize(meta) { events.push(`finalize:${meta.status}`); if (failAt === "finalize") throw new Error(secret) },
  }
}

test("runner finalizes an observer after setup rejection without native profiling", async () => {
  const events = []
  const result = await runBenchmark(options(), {}, provider({ setupError: new Error("setup sentinel") }), { backingObserver: recorder(events) })
  assert.deepEqual(events, ["begin:create", "end:create:false", "finalize:failed"])
  assert.equal(result.providers[0].setupError.message, "setup sentinel")
  assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
})

test("runner finalizes skips and revision failures without starting a phase", async () => {
  for (const state of [{ configured: false }, { revisionMismatch: true }]) {
    const events = []
    const result = await runBenchmark(options(), {}, provider(state), { backingObserver: recorder(events) })
    assert.deepEqual(events, [`finalize:${result.providers[0].status}`])
    assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
  }
})

test("runner hooks preserve lifecycle and setup timers and disabled result shape", async () => {
  const fakeClock = clock()
  const original = Object.getOwnPropertyDescriptor(globalThis, "performance")
  Object.defineProperty(globalThis, "performance", { configurable: true, value: { now: fakeClock.now } })
  try {
    const baseline = await runBenchmark(options(), {}, provider({ tick: (ms) => fakeClock.advance(ms) }))
    const events = []
    const observed = await runBenchmark(options(), {}, provider({ tick: (ms) => fakeClock.advance(ms), hooks: { create: () => events.push("create"), write: () => events.push("write"), cleanup: () => events.push("cleanup") } }), { backingObserver: recorder(events, fakeClock), observerClock: fakeClock })
    assert.equal(baseline.providers[0].backingObserver, undefined)
    assert.equal(observed.results[0].summary.elapsedMs, baseline.results[0].summary.elapsedMs)
    assert.equal(observed.results[0].summary.elapsedMs, 30)
    assert.equal(observed.providers[0].setupMs, 5)
    assert.equal(observed.providers[0].cleanup.resource.ms, 5)
    assert.deepEqual(events, ["begin:create", "create", "end:create:null", "begin:workload-1bytes", "write", "end:workload-1bytes:null", "begin:cleanup", "end:cleanup:null", "begin:shutdown", "cleanup", "end:shutdown:null", "finalize:ok"])
    assert.equal(observed.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
    assert.equal(observed.providers[0].backingObserver.terminal.owned_operations_settled, true)
    assert.equal(observed.providers[0].backingObserver.terminal.workload_native_evidence_complete, null)
    assert.match(observed.providers[0].backingObserver.terminal.quiescence_scope, /native_workload_proof_unobserved/)
  } finally { Object.defineProperty(globalThis, "performance", original) }
})

test("observer hook failures preserve provider cleanup and redact hook error bodies", async () => {
  for (const failAt of ["begin", "end", "finalize"]) {
    let cleaned = 0
    const result = await runBenchmark(options(), {}, provider({ hooks: { cleanup: () => ++cleaned } }), { backingObserver: recorder([], clock(), failAt) })
    assert.equal(cleaned, 1)
    assert.equal(result.status, "ok")
    assert.equal(result.providers[0].backingObserver.complete, false)
    assert.equal(JSON.stringify(result.providers[0].backingObserver).includes(secret), false)
  }
})

test("lossless parser and stats projection preserve an exact one-unit uint64 delta", () => {
  const { parseEngineJSON, projectStats, summarizeInterval } = api()
  const before = projectStats(parseEngineJSON(stats()), cid)
  const after = projectStats(parseEngineJSON(stats({ total: "9007199254740994", second: 1 })), cid)
  const interval = summarizeInterval([owned], { samples: [{ cid, stats: before }] }, { samples: [{ cid, stats: after }] })
  assert.equal(interval.metrics.cpu_usage_ns.total, "1")
  assert.equal(interval.containers[0].metrics.cpu_usage_ns.rate.denominator_ns, "1000000000")
  assert.equal(interval.metrics.block_bytes.total, "0")
  assert.equal(interval.metrics.block_operations.total, "0")
  assert.equal(JSON.stringify(before).includes(secret), false)
  for (const token of ["-1", "1.5", "1e3", "18446744073709551616"]) {
    assert.throws(() => projectStats(parseEngineJSON(stats({ total: token })), cid), /counter/)
  }
})

test("missing counters, changed devices, resets and partial aggregates never become zero", () => {
  const { parseEngineJSON, projectStats, summarizeInterval } = api()
  const sample = (raw, id = cid) => ({ cid: id, stats: projectStats(parseEngineJSON(raw), id) })
  const before = { samples: [sample(stats({ bytes: "7", operations: "2" })), sample(stats({ id: secondCid }), secondCid)] }
  const after = { samples: [sample(stats({ bytes: "9", operations: "3", second: 1 })), sample(stats({ id: secondCid, second: 1, omitted: ["io_serviced_recursive"] }), secondCid)] }
  const delta = summarizeInterval([owned, secondOwned], before, after)
  assert.equal(delta.metrics.block_bytes.total, "2")
  assert.equal(delta.metrics.block_operations.complete, false)
  assert.equal(delta.metrics.block_operations.total, null)
  assert.equal(delta.metrics.block_operations.partial_total, "1")
  assert.deepEqual(delta.metrics.block_operations.missing_members, [secondCid])
  assert.equal(summarizeInterval([owned], { samples: [before.samples[0]] }, { samples: [sample(stats({ bytes: "6", second: 1 }))] }).metrics.block_bytes.complete, false)
  const changed = stats({ bytes: "9", second: 1 }).replace('"minor":0', '"minor":1')
  assert.equal(summarizeInterval([owned], { samples: [before.samples[0]] }, { samples: [sample(changed)] }).metrics.block_bytes.complete, false)
  for (const value of [null, []]) {
    const raw = JSON.parse(stats({ second: 1 })); raw.blkio_stats.io_serviced_recursive = value
    const result = summarizeInterval([owned], { samples: [before.samples[0]] }, { samples: [sample(JSON.stringify(raw))] })
    assert.equal(result.metrics.block_operations.complete, false)
  }
  assert.throws(() => sample(stats({ extraEntries: [{ major: 8, minor: 0, op: "Read", value: 0 }] })), /duplicate/)
  for (const second of [0]) {
    assert.equal(summarizeInterval([owned], { samples: [before.samples[0]] }, { samples: [sample(stats({ second }))] }).metrics.cpu_usage_ns.complete, false)
  }
})

test("inspect identity is projected before retention and drift is rejected", async () => {
  const fakeClock = clock()
  const transport = transportFor(fakeClock)
  const observer = factory(fakeClock, transport)
  const first = await observer.captureBoundary({ id: "start", kind: "idle" })
  assert.equal(first.complete, true)
  assert.equal(typeof first.samples[0].stats_window.dispatch_utc, "string")
  assert.equal(first.samples[0].identity.limits.Memory, "0")
  assert.equal(first.samples[0].identity.limits.MemorySwap, "-1")
  assert.equal(JSON.stringify(observer.receipt()).includes(secret), false)
  fakeClock.advance(1000)
  const second = await observer.captureBoundary({ id: "end", kind: "idle" })
  const interval = api().summarizeInterval([owned], first, second, { kind: "idle", workload_elapsed_ms: 50 })
  assert.equal(interval.kind, "idle")
  assert.equal(interval.workload_elapsed_ms, 50)
  assert.match(interval.attribution, /descriptive/)
  assert.equal(transport.requests.every((request) => request.method === "GET"), true)
  assert.equal(transport.requests.some((request) => /containers\/json/.test(request.path)), false)
  assert.match(transport.requests.at(-1).path, /stats\?stream=false&one-shot=true$/)
  for (const edits of [{ Id: secondCid }, { Image: `sha256:${"d".repeat(64)}` }, { RestartCount: 1 }, { State: { Running: false, StartedAt: "2026-09-26T00:00:00Z" } }, { Config: { Labels: { "mount-rs.tidb.run": "foreign" } } }]) {
    fakeClock.advance(1000)
    transport.request = async () => ({ status: 200, body: (async function* () { yield Buffer.from(inspect(cid, edits)) })() })
    assert.equal((await observer.captureBoundary({ id: `drift-${Object.keys(edits)[0]}`, kind: "phase" })).complete, false)
  }
  assert.throws(() => factory(clock(), transport, [owned, owned]), /allowlist/)
  assert.throws(() => factory(clock(), transport, [owned, { ...secondOwned, role: owned.role }]), /allowlist/)
  assert.throws(() => factory(clock(), transport, [{ ...owned, labels: {} }]), /ownership/)
})

test("request deadline rejects completion before a delayed timer and aborts hanging work", async () => {
  const lateClock = clock()
  let aborted = false
  const late = factory(lateClock, { async request({ signal }) {
    signal.addEventListener("abort", () => { aborted = true })
    lateClock.advance(2001, false)
    return { status: 200, body: (async function* () { yield Buffer.from('{"ApiVersion":"1.51","MinAPIVersion":"1.24","Os":"linux"}') })() }
  } })
  assert.equal((await late.captureBoundary({ id: "late", kind: "phase" })).complete, false)
  assert.equal(aborted, true)
  assert.equal(late.receipt().issues.includes("request_deadline"), true)

  const hangClock = clock()
  let aborts = 0
  const hang = factory(hangClock, { request({ signal }) { signal.addEventListener("abort", () => ++aborts); return new Promise(() => {}) } })
  const capture = hang.captureBoundary({ id: "hung", kind: "phase" })
  await Promise.resolve(); await Promise.resolve()
  hangClock.advance(2001)
  assert.equal((await capture).complete, false)
  assert.equal(aborts, 1)
  const calls = hang.receipt().cost.requests
  await hang.captureBoundary({ id: "no-retry", kind: "phase" })
  assert.equal(hang.receipt().cost.requests, calls)
  await hang.finalize({ status: "failed", native_quiescent: false, cleanup_complete: false })
  assert.equal(hang.receipt().terminal.safe_to_continue_pair, false)
  assert.equal(hangClock.timers.size, 0)
})

test("response, journal, request and sample cadence caps retain incomplete partial receipts", async () => {
  const fakeClock = clock()
  const oversize = factory(fakeClock, transportFor(fakeClock, () => ({ status: 200, body: (async function* () { yield Buffer.alloc(2000) })() })), [owned], { maxResponseBytes: 1024 })
  assert.equal((await oversize.captureBoundary({ id: "oversize", kind: "phase" })).complete, false)
  assert.ok(oversize.receipt().issues.includes("response_bytes_cap"))
  const limited = factory(clock(), transportFor(clock()), [owned], { maxRequests: 1 })
  assert.equal((await limited.captureBoundary({ id: "limited", kind: "phase" })).complete, false)
  assert.equal(limited.receipt().cost.requests, 1)
  const observer = factory(fakeClock, transportFor(fakeClock), [owned], { maxJournalBytes: 8192 })
  await observer.captureBoundary({ id: "first", kind: "phase" })
  const cadence = await observer.captureBoundary({ id: "too-fast", kind: "phase" })
  assert.equal(cadence.complete, false)
  assert.ok(observer.receipt().issues.includes("sample_interval_cap"))
  for (let index = 0; index < 12; index += 1) { fakeClock.advance(1000); await observer.captureBoundary({ id: `sample-${index}`, kind: "idle" }) }
  await observer.finalize({ status: "ok", native_quiescent: true, cleanup_complete: true })
  const receipt = observer.receipt()
  assert.equal(receipt.complete, false)
  assert.ok(receipt.dropped_entries > 0)
  assert.ok(Buffer.byteLength(JSON.stringify(receipt)) <= 8192)
  assert.ok(receipt.cost.requests > 0)
  assert.ok(receipt.cost.response_bytes > 0)
  assert.equal(receipt.daemon_overhead, "unisolated")
})

test("runner terminal rejects pending native work and bounded hooks cannot block cleanup", async () => {
  const definitions = provider()
  let cleaned = 0
  definitions.get("mount-rs-memory").create = async () => ({ filesystem: { writeFile: () => new Promise(() => {}), unlink: async () => {} }, cleanup: async () => { ++cleaned } })
  const events = []
  const result = await runBenchmark(options(), {}, definitions, { backingObserver: recorder(events) })
  assert.equal(result.providers[0].cleanup.resource.status, "deferred")
  assert.equal(cleaned, 0)
  assert.equal(result.providers[0].backingObserver.terminal.native_quiescent, false)
  assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
  assert.ok(events.includes("end:workload-1bytes:false"))
})

test("runner records an incomplete helper result without changing benchmark status", async () => {
  const result = await runBenchmark(options(), {}, provider(), {
    backingObserver: { beginPhase: async () => ({ complete: false }), endPhase: async () => ({ complete: false }), finalize: async () => ({ complete: false }) },
  })
  assert.equal(result.status, "ok")
  assert.equal(result.providers[0].backingObserver.complete, false)
  assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
})

test("finalization is immutable and a spent owner budget fails closed", async () => {
  const fakeClock = clock()
  const observer = factory(fakeClock, transportFor(fakeClock))
  await observer.captureBoundary({ id: "start", kind: "idle" })
  fakeClock.advance(60_001)
  const first = await observer.finalize({ status: "ok", native_quiescent: true, cleanup_complete: true })
  assert.equal(first.complete, false)
  assert.equal(first.terminal.safe_to_continue_pair, false)
  assert.ok(first.issues.includes("owner_deadline"))
  fakeClock.advance(1000)
  assert.deepEqual(await observer.finalize({ status: "failed" }), first)
  assert.deepEqual(observer.receipt(), first)
})

test("a hanging runner hook is bounded, aborted and cannot replace cleanup", async () => {
  const fakeClock = clock()
  let aborted = 0, cleaned = 0, finalized = 0
  const observer = {
    beginPhase({ signal }) { signal.addEventListener("abort", () => ++aborted); fakeClock.advance(2001); return new Promise(() => {}) },
    endPhase: async () => {}, finalize: async () => { ++finalized },
  }
  const result = await runBenchmark(options(), {}, provider({ hooks: { cleanup: () => ++cleaned } }), { backingObserver: observer, observerClock: fakeClock })
  assert.equal(cleaned, 1)
  assert.equal(aborted, 1)
  assert.equal(finalized, 1)
  assert.equal(result.status, "ok")
  assert.equal(result.providers[0].backingObserver.complete, false)
  assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
  assert.equal(fakeClock.timers.size, 0)
})

test("a completed floor-only failure remains distinct from cleanup failure and unobserved native proof", async () => {
  const belowFloor = { ...options(), minIops: Number.MAX_SAFE_INTEGER }
  const floor = await runBenchmark(belowFloor, {}, provider(), { backingObserver: recorder([]) })
  assert.equal(floor.status, "failed")
  assert.equal(floor.results[0].summary.iopsTargetMet, false)
  assert.equal(floor.providers[0].backingObserver.terminal.owned_operations_settled, true)
  assert.equal(floor.providers[0].backingObserver.terminal.cleanup_complete, true)
  assert.equal(floor.providers[0].backingObserver.terminal.native_quiescent, null)
  assert.equal(floor.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
  const failedCleanup = await runBenchmark(options(), {}, provider({ hooks: { cleanup: () => { throw new Error("cleanup sentinel") } } }), { backingObserver: recorder([]) })
  assert.equal(failedCleanup.providers[0].cleanup.resource.status, "failed")
  assert.equal(failedCleanup.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
})

test("late transport settlement cannot change a finalized partial receipt", async () => {
  const fakeClock = clock()
  let resolveRequest
  const observer = factory(fakeClock, { request: () => new Promise((resolve) => { resolveRequest = resolve }) })
  const pending = observer.captureBoundary({ id: "hung", kind: "phase" })
  await Promise.resolve(); await Promise.resolve()
  fakeClock.advance(2001)
  await pending
  const final = await observer.finalize({ status: "failed", native_quiescent: false, cleanup_complete: false })
  resolveRequest({ status: 200, body: (async function* () { yield Buffer.from('{"ApiVersion":"1.51","MinAPIVersion":"1.24","Os":"linux"}') })() })
  for (let tick = 0; tick < 12; tick += 1) await Promise.resolve()
  assert.deepEqual(observer.receipt(), final)
})

test("streaming checks the monotonic deadline between chunks without a timer callback", async () => {
  const fakeClock = clock()
  let chunks = 0, aborted = false
  const observer = factory(fakeClock, { async request({ signal }) {
    signal.addEventListener("abort", () => { aborted = true })
    return { status: 200, body: (async function* () {
      for (let index = 0; index < 3000; index += 1) { ++chunks; fakeClock.advance(1, false); yield Buffer.alloc(0) }
    })() }
  } })
  assert.equal((await observer.captureBoundary({ id: "stream", kind: "phase" })).complete, false)
  assert.equal(aborted, true)
  assert.ok(chunks <= 2001, "stream must stop at the monotonic deadline despite zero byte chunks")
})

test("finalization rejects an unclosed phase even after successful sampling", async () => {
  const fakeClock = clock()
  const observer = factory(fakeClock, transportFor(fakeClock))
  await observer.beginPhase({ provider: "controlled", name: "create" })
  const final = await observer.finalize({ status: "ok", native_quiescent: true, cleanup_complete: true })
  assert.equal(final.complete, false)
  assert.equal(final.terminal.safe_to_continue_pair, false)
  assert.ok(final.issues.includes("unclosed_phase"))
})

test("receipt base size and projection policy cannot bypass configured bounds", () => {
  const fakeClock = clock()
  const oversized = Array.from({ length: 16 }, (_, index) => ({ cid: index.toString(16).padStart(64, "0"), role: `member-${index}`, labels: Object.fromEntries(["mount-rs.tidb.run", "mount-rs.foundationdb.run", "com.mount-rs.ozone-test", "com.mount-rs.ozone-test-run"].map((key) => [key, "x".repeat(160)])) }))
  assert.throws(() => factory(fakeClock, transportFor(fakeClock), oversized, { maxJournalBytes: 8192 }), /receipt_base_cap/)
  assert.throws(() => factory(fakeClock, transportFor(fakeClock), [owned], { constructor: 1 }), /observer_cap/)
  assert.throws(() => api().projectInspect(api().parseEngineJSON(inspect()), { cid, role: "tidb", labels: { secret } }), /ownership/)
})

test("enabled but unavailable native diagnostics cannot establish observer quiescence", async () => {
  const { createRequire } = await import("node:module")
  const { fileURLToPath } = await import("node:url")
  const capturePath = fileURLToPath(new URL("./capture-native.cjs", import.meta.url))
  const capture = createRequire(import.meta.url)(capturePath)
  assert.equal(capture.__napiBindingTarget, "storage-benchmark-capture")
  const previousPath = process.env.NAPI_RS_NATIVE_LIBRARY_PATH
  const previousProfile = process.env.MOUNT_RS_PROFILE_IO
  const originalWrite = process.stderr.write
  process.env.NAPI_RS_NATIVE_LIBRARY_PATH = capturePath
  process.stderr.write = () => true
  try {
    const events = []
    const result = await runBenchmark(options(), { MOUNT_RS_PROFILE_IO: "1" }, provider(), { backingObserver: recorder(events) })
    assert.ok(result.providers[0].storageDiagnostics.phases.every((phase) => !phase.native.complete))
    assert.equal(result.status, "ok", "observation incompleteness must not replace the provider result")
    assert.equal(result.providers[0].backingObserver.terminal.native_quiescent, null)
    assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
    assert.equal(result.providers[0].backingObserver.terminal.workload_native_evidence_complete, false)
    assert.ok(events.includes("end:workload-1bytes:null"))
  } finally {
    process.stderr.write = originalWrite
    if (previousPath === undefined) delete process.env.NAPI_RS_NATIVE_LIBRARY_PATH
    else process.env.NAPI_RS_NATIVE_LIBRARY_PATH = previousPath
    if (previousProfile === undefined) delete process.env.MOUNT_RS_PROFILE_IO
    else process.env.MOUNT_RS_PROFILE_IO = previousProfile
  }
})

function syntheticNativeSnapshot(live) {
  const zero = (fields) => Object.fromEntries(fields.map((field) => [field, "0"]))
  const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
  const rawFields = ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns"]
  const instance = {
    id: "7", ...zero(["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]),
    raw_api: { schema: "mount-rs.object-store-api.v1", scope: "one_object_store_block_store_instance", saturated: false, in_flight: "0", pending_claims: "0", claims: zero(["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]), entries: rawNames.map((name) => ({ name, ...zero(rawFields), latency_log2_us: Array(32).fill("0") })) },
  }
  return JSON.stringify({
    schema_version: "mount-rs.storage-diagnostics.v3", enabled: true, scope: "process", quiescent_snapshot_required: true, elapsed_semantics: "inclusive_wall_nanoseconds",
    measurement: {
      storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS, storage_rows: STORAGE_ROW_SEMANTICS,
      storage_operations: STORAGE_OPERATION_NAMES, storage_families: STORAGE_OPERATION_FAMILIES, storage_instrumented_operations: STORAGE_INSTRUMENTED_OPERATION_NAMES, tidb_coverage: TIDB_DIAGNOSTIC_COVERAGE,
      storage_duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap", forwarding_boxes: "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations", profile: "existing_core_profile_counters", sqlite: "live_connection_pager_and_sql_category_counters; pager_bytes_are_page_size_estimates", r2: "live_store_logical_calls_and_cache_hits; not_http_attempts",
      r2_api: { schema: "mount-rs.object-store-api.v1", scope: "live_registered_split_r2_block_store_instances", calls: "object_store_adapter_method_invocations; not_http_attempts_or_internal_retries", duration: "inclusive_wall_nanoseconds_at_invoked_adapter_await; excludes_argument_preparation", upload_bytes: "attempted=submitted_payload; confirmed=put_opts_ok_only", returned_bytes: "successful_body_materialization_before_integrity_validation", latency_max: "cumulative_per_instance; exact_phase_max_unavailable", reconcile_listing: "unavailable", excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "internal_client_retries"] },
      unavailable: { http_attempts: "unavailable", internal_successful_retries: "unavailable", physical_device_iops: "unavailable", tidb_pool_wait: "isolated_queue_only_wait_unavailable", native_allocation_count: "unavailable", js_allocation_count: "unavailable" },
      latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, bucket) => bucket === 0 ? { lower_inclusive_us: "0", upper_exclusive_us: "1" } : bucket === 31 ? { lower_inclusive_us: "1073741824", upper_exclusive_us: null, terminal_overflow: true } : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) }) },
    },
    backend_waits: { pglite_client_lock: "instrumented", tidb_pool: "instrumented_inclusive_checkout_including_lazy_connect_and_session_configuration" }, http_attempts: "unavailable", physical_device_iops: "unavailable",
    storage: { in_flight: "0", forwarding_boxes: { sites: "napi_dynamic_provider_forwarding_future", calls: "0", requested_object_bytes: "0" }, entries: STORAGE_OPERATION_NAMES.map((name) => ({ name, ...zero(["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "in_flight", "elapsed_ns"]), latency_log2_us: Array(32).fill("0") })) },
    profile: { entries: [] }, sqlite: { connections: [] }, r2: { scope: "process_live_instances", internal_successful_retries: "unavailable", instances: live ? [instance] : [] },
  })
}

test("expected shutdown registry retirement preserves complete workload proof", async () => {
  const capture = require(capturePath)
  const descriptor = Object.getOwnPropertyDescriptor(capture, "storageDiagnostics")
  const originalWrite = process.stderr.write
  const previousProfile = process.env.MOUNT_RS_PROFILE_IO
  let live = true
  capture.storageDiagnostics = () => syntheticNativeSnapshot(live)
  process.stderr.write = () => true
  try {
    const events = []
    const result = await runBenchmark({ ...options(), minIops: Number.MAX_SAFE_INTEGER }, { MOUNT_RS_PROFILE_IO: "1" }, provider({ hooks: { cleanup: () => { live = false } } }), { backingObserver: recorder(events) })
    const phases = result.providers[0].storageDiagnostics.phases
    assert.equal(phases.find((phase) => phase.name.startsWith("workload-")).native.complete, true, "synthetic zero-call workload must provide valid native evidence")
    const shutdown = phases.find((phase) => phase.name === "shutdown")
    assert.equal(shutdown.native.complete, false)
    assert.ok(shutdown.native.issues.includes("R2 instance closed during phase"))
    assert.equal(result.providers[0].cleanup.resource.status, "ok")
    assert.equal(result.status, "failed", "floor failure remains sticky")
    assert.equal(result.providers[0].backingObserver.terminal.owned_operations_settled, true)
    assert.equal(result.providers[0].backingObserver.terminal.workload_native_evidence_complete, true)
    assert.equal(result.providers[0].backingObserver.terminal.native_quiescent, true)
    assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, true)
    assert.ok(events.includes("end:shutdown:null"), "unavailable shutdown visibility is not observed nonquiescence")
  } finally {
    if (descriptor) Object.defineProperty(capture, "storageDiagnostics", descriptor)
    else delete capture.storageDiagnostics
    process.stderr.write = originalWrite
    if (previousProfile === undefined) delete process.env.MOUNT_RS_PROFILE_IO
    else process.env.MOUNT_RS_PROFILE_IO = previousProfile
  }
})

test("observed shutdown native work cannot be hidden by successful owned cleanup", async () => {
  const capture = require(capturePath)
  const descriptor = Object.getOwnPropertyDescriptor(capture, "storageDiagnostics")
  const originalWrite = process.stderr.write
  const previousProfile = process.env.MOUNT_RS_PROFILE_IO
  let closed = false
  capture.storageDiagnostics = () => {
    const snapshot = JSON.parse(syntheticNativeSnapshot(true))
    snapshot.storage.in_flight = closed ? "1" : "0"
    return JSON.stringify(snapshot)
  }
  process.stderr.write = () => true
  try {
    const result = await runBenchmark(options(), { MOUNT_RS_PROFILE_IO: "1" }, provider({ hooks: { cleanup: () => { closed = true } } }), { backingObserver: recorder([]) })
    assert.equal(result.providers[0].cleanup.resource.status, "ok")
    assert.equal(result.providers[0].backingObserver.terminal.owned_operations_settled, true)
    assert.equal(result.providers[0].backingObserver.terminal.workload_native_evidence_complete, true)
    assert.equal(result.providers[0].backingObserver.terminal.native_quiescent, false)
    assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
    assert.equal(result.providers[0].backingObserver.events.find((event) => event.phase === "shutdown" && event.hook === "endPhase").native_evidence_state, "nonquiescent")
  } finally {
    if (descriptor) Object.defineProperty(capture, "storageDiagnostics", descriptor)
    else delete capture.storageDiagnostics
    process.stderr.write = originalWrite
    if (previousProfile === undefined) delete process.env.MOUNT_RS_PROFILE_IO
    else process.env.MOUNT_RS_PROFILE_IO = previousProfile
  }
})

test("unsupported Engine contracts and foreign stats are incomplete without discovery", async () => {
  for (const version of [
    '{"ApiVersion":"1.40","MinAPIVersion":"1.24","Os":"linux"}',
    '{"ApiVersion":"1.55","MinAPIVersion":"1.52","Os":"linux"}',
    '{"ApiVersion":"1.51","MinAPIVersion":"1.24","Os":"windows"}',
    '{"ApiVersion":"malformed","MinAPIVersion":"1.24","Os":"linux"}',
  ]) {
    const fakeClock = clock()
    const transport = transportFor(fakeClock, () => ({ status: 200, body: (async function* () { yield Buffer.from(version) })() }))
    const observer = factory(fakeClock, transport)
    assert.equal((await observer.captureBoundary({ id: "unsupported", kind: "phase" })).complete, false)
    assert.equal(transport.requests.length, 1)
  }
  const fakeClock = clock()
  const observer = factory(fakeClock, transportFor(fakeClock, (request) => request.path.includes("/stats?") ? { status: 200, body: (async function* () { yield Buffer.from(stats({ id: secondCid })) })() } : undefined))
  assert.equal((await observer.captureBoundary({ id: "foreign", kind: "phase" })).complete, false)
  assert.ok(observer.receipt().issues.includes("stats_identity"))
})

test("concurrent captures cannot overlap a pinned CID request", async () => {
  const fakeClock = clock()
  let resolveInspect
  const transport = transportFor(fakeClock, (request) => request.path.endsWith("/json") ? new Promise((resolve) => { resolveInspect = resolve }) : undefined)
  const observer = factory(fakeClock, transport)
  const first = observer.captureBoundary({ id: "first", kind: "phase" })
  for (let tick = 0; tick < 30 && !resolveInspect; tick += 1) await Promise.resolve()
  assert.equal(typeof resolveInspect, "function")
  assert.equal((await observer.captureBoundary({ id: "overlap", kind: "phase" })).complete, false)
  assert.equal(transport.requests.filter((request) => request.cid === cid).length, 1)
  resolveInspect({ status: 200, body: (async function* () { yield Buffer.from(inspect()) })() })
  assert.equal((await first).complete, true)
  assert.ok(observer.receipt().issues.includes("request_overlap"))
})

test("endpoint skew stays separate from workload timing and duplicate members reject", () => {
  const { projectStats, parseEngineJSON, summarizeInterval } = api()
  const sample = (id, second, dispatch) => ({ cid: id, stats: projectStats(parseEngineJSON(stats({ id, second })), id), stats_window: { dispatch_ms: dispatch, response_ms: dispatch + 5 } })
  const before = { samples: [sample(cid, 1, 100), sample(secondCid, 2, 150)] }
  const after = { samples: [sample(cid, 3, 2000), sample(secondCid, 5, 2100)] }
  const delta = summarizeInterval([owned, secondOwned], before, after, { workload_elapsed_ms: 30 })
  assert.equal(delta.workload_elapsed_ms, 30)
  assert.equal(delta.endpoints.before.daemon_read_skew_ns, "1000000000")
  assert.deepEqual(delta.endpoints.after.local_enclosing_ms, { dispatch: 2000, response: 2105 })
  assert.equal(delta.containers[0].metrics.cpu_usage_ns.rate.denominator_ns, "2000000000")
  assert.equal(delta.containers[1].metrics.cpu_usage_ns.rate.denominator_ns, "3000000000")
  assert.equal(delta.cpu_percentage_aggregate, "unavailable_for_different_windows")
  assert.throws(() => summarizeInterval([owned], { samples: [before.samples[0], before.samples[0]] }, after), /duplicate_or_foreign/)
  const decreasing = summarizeInterval([owned], { samples: [after.samples[0]] }, { samples: [before.samples[0]] })
  assert.equal(decreasing.metrics.cpu_usage_ns.complete, false)
  assert.equal(decreasing.containers[0].metrics.cpu_usage_ns.issue, "nonincreasing_read_timestamp")
})

test("factory observations reach runner JSON with exact counters, identity, time and availability", async () => {
  const fakeClock = clock()
  let samples = 0, overriddenReceiptCalls = 0
  const transport = transportFor(fakeClock, (request) => {
    if (!request.path.includes("/stats?")) return
    const raw = stats({ total: (9007199254740993n + BigInt(samples)).toString(), second: Math.floor(fakeClock.now() / 1000), bytes: String(7 + samples * 2), omitted: ["io_serviced_recursive"] }).replace('"usage":0', '"usage":33')
    samples += 1
    return { status: 200, body: (async function* () { yield Buffer.from(raw) })() }
  })
  const observer = factory(fakeClock, transport)
  observer.receipt = () => { overriddenReceiptCalls += 1; throw new Error(secret) }
  const result = await runBenchmark(options(), {}, provider({ tick: () => fakeClock.advance(1000) }), { backingObserver: observer, observerClock: fakeClock })
  const wrapper = result.providers[0].backingObserver
  assert.ok(wrapper.backing_evidence, "runner JSON must retain the factory's original projected receipt")
  const evidence = wrapper.backing_evidence
  assert.equal(evidence.schema, "mount-rs.backing-observer.v1")
  const boundary = evidence.journal.find((entry) => entry.type === "boundary" && entry.id === "create:begin")
  const sample = boundary.samples[0]
  assert.equal(sample.stats.cpu_usage_ns, "9007199254740993")
  assert.equal(sample.stats.memory.usage_bytes, "33")
  assert.equal(sample.stats.memory.limit_bytes, "1024")
  assert.equal(sample.identity.cid, cid)
  assert.equal(sample.identity.image, image)
  assert.deepEqual(sample.identity.labels, owned.labels)
  assert.equal(sample.identity.limits.MemorySwap, "-1")
  assert.equal(sample.stats.read_ns, "1790380800123456789")
  assert.equal(sample.stats_window.dispatch_utc, "2027-01-15T08:00:00.000Z")
  assert.equal(sample.stats_window.dispatch_ms, 0)
  assert.match(sample.stats_body_sha256, /^[a-f0-9]{64}$/)
  const interval = evidence.journal.find((entry) => entry.type === "interval")
  assert.equal(interval.metrics.cpu_usage_ns.total, "1")
  assert.equal(interval.metrics.block_bytes.total, "2")
  assert.equal(interval.metrics.block_operations.total, null)
  assert.equal(interval.metrics.block_operations.complete, false)
  assert.deepEqual(interval.metrics.block_operations.missing_members, [cid])
  assert.equal(interval.containers[0].metrics.cpu_usage_ns.rate.denominator_ns, "1000000000")
  assert.equal(evidence.complete, false, "adjacent-boundary cadence remains an explicit limitation")
  assert.equal(wrapper.complete, false)
  assert.equal(wrapper.terminal.safe_to_continue_pair, false)
  assert.equal(overriddenReceiptCalls, 0)
  assert.equal(JSON.stringify(evidence).includes(secret), false)
  assert.ok(Buffer.byteLength(JSON.stringify(evidence)) <= evidence.caps.maxJournalBytes)
  const frozen = JSON.stringify(wrapper)
  fakeClock.advance(1000)
  await assert.rejects(observer.captureBoundary({ id: "after-finalization", kind: "idle" }), /observer_mutation_outside_session/)
  assert.equal(JSON.stringify(wrapper), frozen, "late helper activity must not mutate the exported final clone")
})

test("unregistered observers never export arbitrary receipt or hook bodies", async () => {
  let receiptCalls = 0
  const observer = {
    async beginPhase() { return { complete: true, body: secret } },
    async endPhase() { return { complete: true, body: secret } },
    async finalize() { return { complete: true, journal: [secret] } },
    get receipt() { receiptCalls += 1; throw new Error(secret) },
  }
  const result = await runBenchmark(options(), {}, provider(), { backingObserver: observer })
  assert.equal(result.status, "ok")
  assert.equal(result.providers[0].backingObserver.complete, true)
  assert.equal(Object.hasOwn(result.providers[0].backingObserver, "backing_evidence"), false)
  assert.equal(receiptCalls, 0)
  assert.equal(JSON.stringify(result.providers[0].backingObserver).includes(secret), false)
})

const positiveTerminal = { status: "ok", native_quiescent: true, owned_operations_settled: true, native_profiling_enabled: true, workload_native_evidence_complete: true, cleanup_complete: true }

test("factory export preserves bounded partial evidence and cap failure blocks continuation", async () => {
  for (const maxJournalBytes of [8192, 65536]) {
    const fakeClock = clock()
    const observer = factory(fakeClock, transportFor(fakeClock), [owned], { maxJournalBytes })
    const session = api().createRunnerObserverSession(observer, { provider: "controlled", runId: "controlled-run", clock: fakeClock })
    for (let phase = 0; phase < 3; phase += 1) {
      fakeClock.advance(1000)
      await session.begin(`phase-${phase}`)
      fakeClock.advance(1000)
      await session.end(`phase-${phase}`, true, 30, 0, "complete")
    }
    await session.finalize(positiveTerminal)
    const wrapper = session.receipt()
    assert.ok(wrapper.backing_evidence)
    assert.ok(Buffer.byteLength(JSON.stringify(wrapper.backing_evidence)) <= maxJournalBytes)
    assert.equal(JSON.stringify(wrapper.backing_evidence).includes(secret), false)
    assert.equal(wrapper.complete, maxJournalBytes === 65536)
    assert.equal(wrapper.terminal.safe_to_continue_pair, maxJournalBytes === 65536)
    if (maxJournalBytes === 8192) {
      assert.ok(wrapper.backing_evidence.dropped_entries > 0)
      assert.ok(wrapper.backing_evidence.issues.includes("journal_bytes_cap"))
    }
    const unchanged = JSON.stringify(wrapper)
    wrapper.backing_evidence.journal.length = 0
    assert.equal(JSON.stringify(session.receipt()), unchanged, "caller mutation must not change the session's frozen evidence")
  }
})

for (const state of ["prefinalized", "prepopulated", "claimed", "reused", "noop-phases"]) {
  test(`factory ownership rejects ${state} evidence hidden behind no-op public hooks`, async () => {
    const fakeClock = clock()
    const observer = factory(fakeClock, transportFor(fakeClock))
    if (state === "prepopulated" || state === "prefinalized") await observer.captureBoundary({ id: "old-sample", kind: "idle" })
    if (state === "prefinalized") await observer.finalize(positiveTerminal)
    if (state === "claimed" || state === "reused") {
      const prior = api().createRunnerObserverSession(observer, { provider: "prior", runId: "prior-run", clock: fakeClock })
      if (state === "reused") await prior.finalize(positiveTerminal)
    }
    observer.beginPhase = async () => ({ complete: true })
    observer.endPhase = async () => ({ complete: true })
    observer.finalize = async () => ({ complete: true })
    const result = await runBenchmark(options(), {}, provider(), { backingObserver: observer, observerClock: fakeClock })
    const wrapper = result.providers[0].backingObserver
    assert.equal(result.status, "ok", `${state}: observer rejection must preserve provider outcome`)
    assert.equal(wrapper.complete, false, state)
    assert.equal(wrapper.terminal.safe_to_continue_pair, false, state)
    assert.equal(Object.hasOwn(wrapper, "backing_evidence"), false, `${state}: stale or unrelated evidence must not be exported`)
  })
}

test("factory export requires original finalization for this private session claim", async () => {
  for (const state of ["external-finalization", "noop-finalization"]) {
    const fakeClock = clock()
    const observer = factory(fakeClock, transportFor(fakeClock))
    const session = api().createRunnerObserverSession(observer, { provider: "current", runId: "current-run", clock: fakeClock })
    if (state === "external-finalization") await observer.finalize(positiveTerminal).catch(() => {})
    else observer.finalize = async () => ({ complete: true })
    await session.finalize(positiveTerminal)
    assert.equal(session.receipt().complete, false, state)
    assert.equal(session.receipt().terminal.safe_to_continue_pair, false, state)
    if (state === "external-finalization") assert.ok(session.receipt().backing_evidence?.issues.includes("observer_mutation_outside_session"), state)
    else assert.equal(Object.hasOwn(session.receipt(), "backing_evidence"), false, state)
  }
})

for (const method of ["captureBoundary", "beginPhase", "endPhase", "finalize"]) {
  test(`claimed runner evidence rejects external ${method} mutation`, async () => {
    const fakeClock = clock()
    const observer = factory(fakeClock, transportFor(fakeClock))
    const definitions = provider({ tick: () => fakeClock.advance(1000) })
    const definition = definitions.get("mount-rs-memory")
    const originalCreate = definition.create
    let mutationError, readonlyReceipt
    definition.create = async (...args) => {
      readonlyReceipt = observer.receipt()
      fakeClock.advance(1000)
      try {
        if (method === "captureBoundary") await observer.captureBoundary({ id: "injected-sample", kind: "idle", secret })
        else if (method === "finalize") await observer.finalize({ ...positiveTerminal, secret })
        else await observer[method]({ provider: "mount-rs-memory", name: method === "endPhase" ? "create" : "foreign-phase", quiescent: true, owned_operations_settled: true, secret })
      } catch (error) { mutationError = error }
      return originalCreate(...args)
    }
    const result = await runBenchmark(options(), {}, definitions, { backingObserver: observer, observerClock: fakeClock })
    const wrapper = result.providers[0].backingObserver
    assert.equal(result.status, "ok", "observer ownership failures must preserve the provider outcome")
    assert.equal(mutationError?.code, "observer_mutation_outside_session")
    assert.equal(readonlyReceipt.journal.some((entry) => entry.type === "boundary"), true, "original receipt reads remain available")
    assert.ok(wrapper.backing_evidence, "retain the bounded runner-owned partial evidence")
    assert.equal(wrapper.complete, false)
    assert.equal(wrapper.terminal.safe_to_continue_pair, false)
    assert.ok(wrapper.backing_evidence.issues.includes("observer_mutation_outside_session"))
    assert.equal(wrapper.backing_evidence.journal.some((entry) => entry.id === "injected-sample" || entry.id?.startsWith("foreign-phase")), false)
    const serialized = JSON.stringify(wrapper)
    assert.equal(serialized.includes(secret), false)
    assert.equal(serialized.includes('"claim":'), false)
    assert.equal(serialized.includes('"capability":'), false)
  })
}

const withNetwork = (raw, options) => stats(options).replace('"additive_unknown":', `"networks":${raw},"additive_unknown":`)
const networkSample = (raw, id = cid, second = 0) => ({ cid: id, stats: api().projectStats(api().parseEngineJSON(withNetwork(raw, { id, second })), id) })
const networkInterval = (before, after, allowlist = [owned]) => api().summarizeInterval(allowlist, { samples: before }, { samples: after })

test("network byte projection and interval preserve exact direction counters", () => {
  const first = networkSample('{"eth1":{"rx_bytes":0,"tx_bytes":0},"eth0":{"rx_bytes":9007199254740993,"tx_bytes":0}}')
  const last = networkSample('{"eth0":{"rx_bytes":9007199254740994,"tx_bytes":0},"eth1":{"rx_bytes":0,"tx_bytes":0}}', cid, 1)
  assert.deepEqual(first.stats.network_rx_bytes, { eth0: "9007199254740993", eth1: "0" })
  assert.deepEqual(first.stats.network_tx_bytes, { eth0: "0", eth1: "0" })
  const interval = networkInterval([first], [last])
  assert.equal(interval.metrics.network_rx_bytes.total, "1")
  assert.equal(interval.metrics.network_tx_bytes.total, "0")
  assert.deepEqual(interval.containers[0].metrics.network_rx_bytes.interfaces, { eth0: "1", eth1: "0" })
  assert.equal(interval.containers[0].metrics.network_rx_bytes.rate.denominator_ns, "1000000000")
  assert.match(interval.network_attribution, /client_server.*same_traffic_multiple_times/)
  assert.match(interval.network_attribution, /no_physical_link_or_flow_attribution/)
})

test("missing network and independent missing directions never become observed zero", () => {
  const { projectStats, parseEngineJSON } = api()
  for (const raw of [stats(), withNetwork("null"), withNetwork("{}")]) {
    const projected = projectStats(parseEngineJSON(raw), cid)
    assert.equal(projected.network_rx_bytes, null)
    assert.equal(projected.network_tx_bytes, null)
    const interval = networkInterval([{ cid, stats: projected }], [networkSample('{"eth0":{"rx_bytes":0,"tx_bytes":0}}', cid, 1)])
    assert.equal(interval.metrics.network_rx_bytes.total, null)
    assert.equal(interval.metrics.network_tx_bytes.total, null)
  }
  for (const missing of ["", '"rx_bytes":null,']) {
    const first = networkSample(`{"eth0":{${missing}"tx_bytes":0}}`)
    const last = networkSample('{"eth0":{"rx_bytes":0,"tx_bytes":0}}', cid, 1)
    assert.deepEqual(first.stats.network_rx_bytes, { eth0: null })
    const interval = networkInterval([first], [last])
    assert.equal(interval.metrics.network_rx_bytes.total, null)
    assert.equal(interval.containers[0].metrics.network_rx_bytes.issue, "metric_unavailable")
    assert.equal(interval.metrics.network_tx_bytes.total, "0")
    assert.equal(interval.metrics.network_tx_bytes.complete, true)
  }
})

test("network interface changes and direction resets reject without invalidating other metrics", () => {
  const first = networkSample('{"eth0":{"rx_bytes":7,"tx_bytes":0}}')
  for (const raw of ['{"eth1":{"rx_bytes":8,"tx_bytes":0}}', '{"eth0":{"rx_bytes":8,"tx_bytes":0},"eth1":{"rx_bytes":0,"tx_bytes":0}}']) {
    const interval = networkInterval([first], [networkSample(raw, cid, 1)])
    assert.equal(interval.metrics.network_rx_bytes.total, null)
    assert.equal(interval.containers[0].metrics.network_rx_bytes.issue, "interface_keys_changed")
    assert.equal(interval.containers[0].metrics.network_tx_bytes.issue, "interface_keys_changed")
    assert.equal(interval.metrics.cpu_usage_ns.complete, true)
  }
  const reset = networkInterval([first], [networkSample('{"eth0":{"rx_bytes":6,"tx_bytes":0}}', cid, 1)])
  assert.equal(reset.containers[0].metrics.network_rx_bytes.issue, "counter_reset")
  assert.equal(reset.metrics.network_rx_bytes.total, null)
  assert.equal(reset.metrics.network_tx_bytes.total, "0")
})

test("network totals retain exact sums beyond uint64 and separate missing CID aggregates", () => {
  const zero = '{"eth0":{"rx_bytes":0,"tx_bytes":0},"eth1":{"rx_bytes":0,"tx_bytes":0}}'
  const maximum = '{"eth0":{"rx_bytes":18446744073709551615,"tx_bytes":0},"eth1":{"rx_bytes":18446744073709551615,"tx_bytes":0}}'
  const first = [networkSample(zero), networkSample(zero, secondCid)]
  const last = [networkSample(maximum, cid, 1), networkSample(maximum, secondCid, 1)]
  const exact = networkInterval(first, last, [owned, secondOwned])
  assert.equal(exact.containers[0].metrics.network_rx_bytes.value, "36893488147419103230")
  assert.equal(exact.metrics.network_rx_bytes.total, "73786976294838206460")
  assert.equal(exact.metrics.network_rx_bytes.partial_total, "73786976294838206460")
  assert.equal(exact.metrics.network_tx_bytes.total, "0")
  const partial = networkInterval([networkSample('{"eth0":{"rx_bytes":0,"tx_bytes":0}}'), networkSample('{"eth0":{"tx_bytes":0}}', secondCid)], [networkSample('{"eth0":{"rx_bytes":2,"tx_bytes":0}}', cid, 1), networkSample('{"eth0":{"tx_bytes":0}}', secondCid, 1)], [owned, secondOwned])
  assert.equal(partial.metrics.network_rx_bytes.total, null)
  assert.equal(partial.metrics.network_rx_bytes.partial_total, "2")
  assert.deepEqual(partial.metrics.network_rx_bytes.missing_members, [secondCid])
  assert.equal(partial.metrics.network_tx_bytes.total, "0")
})

test("network interface count has a hard 32 ceiling and caller limits only lower it", () => {
  const { projectStats, parseEngineJSON } = api()
  const interfaces = (count) => JSON.stringify(Object.fromEntries(Array.from({ length: count }, (_, index) => [`eth${index}`, { rx_bytes: 0, tx_bytes: 0 }])))
  assert.equal(Object.keys(projectStats(parseEngineJSON(withNetwork(interfaces(32))), cid).network_rx_bytes).length, 32)
  assert.throws(() => projectStats(parseEngineJSON(withNetwork(interfaces(33))), cid), /network_interfaces_cap/)
  assert.throws(() => projectStats(parseEngineJSON(withNetwork(interfaces(2))), cid, 128, 1), /network_interfaces_cap/)
  for (const maximum of [0, 33, 1.5]) assert.throws(() => projectStats(parseEngineJSON(stats()), cid, 128, maximum), /network_interfaces_cap/)
  assert.throws(() => factory(clock(), { request() {} }, [owned], { maxNetworkInterfaces: 33 }), /observer_cap/)
})

test("unsafe network keys, malformed objects and invalid selected uint64 values reject", () => {
  const { projectStats, parseEngineJSON } = api()
  for (const key of ["10.0.0.1", "eth0:1", "../eth0", "eth0 secret", "éth0", "a".repeat(16), "constructor", "prototype", "__proto__"]) {
    const raw = `{"${key}":{"rx_bytes":0,"tx_bytes":0}}`
    assert.throws(() => projectStats(parseEngineJSON(withNetwork(raw)), cid), /network_interface_key/)
  }
  for (const raw of ['[]', '"network"', '{"eth0":null}', '{"eth0":[]}']) assert.throws(() => projectStats(parseEngineJSON(withNetwork(raw)), cid), /network_shape/)
  for (const token of ["-1", "1.5", "1e3", "18446744073709551616", '"01"']) {
    assert.throws(() => projectStats(parseEngineJSON(withNetwork(`{"eth0":{"rx_bytes":${token},"tx_bytes":0}}`)), cid), /invalid_counter/)
  }
})

test("network projection drops addresses and additive fields before bounded journal retention", async () => {
  const { projectStats, parseEngineJSON } = api()
  const raw = `{"lo":{"rx_bytes":0,"tx_bytes":0,"rx_packets":123,"endpoint_id":"${secret}","address":"${secret}"}}`
  const projected = projectStats(parseEngineJSON(withNetwork(raw)), cid)
  assert.deepEqual(projected.network_rx_bytes, { lo: "0" })
  assert.equal(JSON.stringify(projected).includes(secret), false)
  assert.equal(JSON.stringify(projected).includes("rx_packets"), false)
  const fakeClock = clock()
  const transport = transportFor(fakeClock, (request) => request.path.includes("/stats?") ? { status: 200, body: (async function* () { yield Buffer.from(withNetwork(`{"${secret}":{"rx_bytes":0,"tx_bytes":0}}`)) })() } : undefined)
  const observer = factory(fakeClock, transport)
  assert.equal((await observer.captureBoundary({ id: "unsafe-key", kind: "phase" })).complete, false)
  await observer.finalize(positiveTerminal)
  assert.ok(observer.receipt().issues.includes("network_interface_key"))
  assert.equal(JSON.stringify(observer.receipt()).includes(secret), false)
})

test("factory network observations reach runner evidence without expanding native proof", async () => {
  const fakeClock = clock()
  let samples = 0
  const transport = transportFor(fakeClock, (request) => {
    if (!request.path.includes("/stats?")) return
    const raw = withNetwork(`{"eth0":{"rx_bytes":${9007199254740993n + BigInt(samples)},"tx_bytes":null,"address":"${secret}"}}`, { second: Math.floor(fakeClock.now() / 1000) })
    samples += 1
    return { status: 200, body: (async function* () { yield Buffer.from(raw) })() }
  })
  const result = await runBenchmark(options(), {}, provider({ tick: () => fakeClock.advance(1000) }), { backingObserver: factory(fakeClock, transport), observerClock: fakeClock })
  const wrapper = result.providers[0].backingObserver
  const boundary = wrapper.backing_evidence.journal.find((entry) => entry.type === "boundary" && entry.id === "create:begin")
  assert.deepEqual(boundary.samples[0].stats.network_rx_bytes, { eth0: "9007199254740993" })
  assert.deepEqual(boundary.samples[0].stats.network_tx_bytes, { eth0: null })
  assert.equal(boundary.samples[0].identity.cid, cid)
  assert.equal(boundary.samples[0].stats_window.dispatch_ms, 0)
  const interval = wrapper.backing_evidence.journal.find((entry) => entry.type === "interval")
  assert.equal(interval.metrics.network_rx_bytes.total, "1")
  assert.equal(interval.metrics.network_tx_bytes.total, null)
  assert.equal(wrapper.terminal.native_quiescent, null)
  assert.equal(wrapper.terminal.safe_to_continue_pair, false)
  assert.equal(JSON.stringify(wrapper).includes(secret), false)
})

test("wide network observations respect existing receipt bounds and lowerable factory cap", async () => {
  const fakeClock = clock()
  const raw = JSON.stringify(Object.fromEntries(Array.from({ length: 32 }, (_, index) => [`eth${index}`, { rx_bytes: "18446744073709551615", tx_bytes: "18446744073709551615" }])))
  const transport = transportFor(fakeClock, (request) => request.path.includes("/stats?") ? { status: 200, body: (async function* () { yield Buffer.from(withNetwork(raw, { second: Math.floor(fakeClock.now() / 1000) })) })() } : undefined)
  const observer = factory(fakeClock, transport, [owned], { maxJournalBytes: 8192 })
  await observer.captureBoundary({ id: "network-first", kind: "phase" })
  fakeClock.advance(1000)
  await observer.captureBoundary({ id: "network-last", kind: "phase" })
  await observer.finalize(positiveTerminal)
  const receipt = observer.receipt()
  assert.equal(receipt.caps.maxNetworkInterfaces, 32)
  assert.ok(receipt.dropped_entries > 0)
  assert.ok(receipt.issues.includes("journal_bytes_cap"))
  assert.ok(Buffer.byteLength(JSON.stringify(receipt)) <= 8192)
  assert.equal(receipt.terminal.safe_to_continue_pair, false)
  const lowered = factory(fakeClock, transport, [owned], { maxNetworkInterfaces: 1 })
  assert.equal((await lowered.captureBoundary({ id: "lowered", kind: "phase" })).complete, false)
  assert.ok(lowered.receipt().issues.includes("network_interfaces_cap"))
})

const rustfsLabels = { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "owned-run" }
const rustfsOwned = { cid, role: "rustfs", labels: rustfsLabels }

test("RustFS direct service ownership projects only its exact expected label pair", () => {
  const { projectInspect, parseEngineJSON } = api()
  const projected = projectInspect(parseEngineJSON(inspect(cid, { Config: { Env: [secret], Labels: { ...rustfsLabels, ignored: secret } } })), rustfsOwned)
  assert.deepEqual(projected.labels, rustfsLabels)
  assert.equal(projected.cid, cid)
  assert.equal(projected.role, "rustfs")
  assert.equal(JSON.stringify(projected).includes(secret), false)
})

test("RustFS qualification rejects cleanup helpers, missing pairs and ownership mismatches", () => {
  const { projectInspect, parseEngineJSON } = api()
  const raw = (labels) => parseEngineJSON(inspect(cid, { Config: { Labels: labels } }))
  for (const key of Object.keys(rustfsLabels)) assert.throws(() => factory(clock(), { request() {} }, [{ ...rustfsOwned, labels: { [key]: rustfsLabels[key] } }]), /ownership_labels/)
  for (const purpose of ["cleanup", "other", null]) assert.throws(() => projectInspect(raw({ ...rustfsLabels, "com.mount-rs.rustfs-test-purpose": purpose }), rustfsOwned), /rustfs_service_purpose/)
  assert.throws(() => projectInspect(raw({ ...rustfsLabels, "com.mount-rs.rustfs-test-run": "different" }), rustfsOwned), /ownership_mismatch/)
  assert.throws(() => projectInspect(raw({ "com.mount-rs.rustfs-test": "mount-rs-rustfs-test" }), rustfsOwned), /ownership_mismatch/)
  const fiveLabels = { ...rustfsLabels, "mount-rs.tidb.run": "run", "mount-rs.foundationdb.run": "run", "com.mount-rs.ozone-test": "run" }
  assert.throws(() => factory(clock(), { request() {} }, [{ ...rustfsOwned, labels: fiveLabels }]), /ownership_labels/)
})
