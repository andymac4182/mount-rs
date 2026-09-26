import assert from "node:assert/strict"
import { EventEmitter } from "node:events"
import { PassThrough } from "node:stream"
import test from "node:test"
import { createBackingObserver, createRunnerObserverSession } from "./backing-observer.mjs"

const moduleUnderTest = await import("./backing-engine-transport.mjs").catch((error) => {
  if (error.code === "ERR_MODULE_NOT_FOUND") return null
  throw error
})

const cid = "a".repeat(64)
const otherCid = "b".repeat(64)
const socketPath = "/private/tmp/EXCLUDED_ENGINE_SOCKET_SENTINEL.sock"
const secret = "EXCLUDED_ENGINE_RESPONSE_SENTINEL"
const options = (overrides = {}) => ({ method: "GET", path: "/version", cid: undefined, signal: new AbortController().signal, maxResponseBytes: 32, ...overrides })

function transport(fake, cids = [cid]) {
  assert.ok(moduleUnderTest, "the Engine adapter must be implemented")
  return moduleUnderTest.createBackingEngineTransport({ socketPath, allowlistedCids: cids, requestImpl: fake.requestImpl })
}

function fakeHttp({ auto = true, status = 200, headers = {}, chunks = [Buffer.from("ok")], end = true } = {}) {
  const calls = []
  function requestImpl(requestOptions, onResponse) {
    const request = new EventEmitter()
    request.destroyed = false
    request.endCount = 0
    request.end = () => {
      request.endCount++
      if (auto) queueMicrotask(() => request.respond({ status, headers, chunks, end }))
    }
    request.destroy = () => {
      request.destroyed = true
      request.response?.destroy()
      request.emit("close")
      return request
    }
    request.respond = ({ status: replyStatus = 200, headers: replyHeaders = {}, chunks: replyChunks = [], end: replyEnd = true, preDestroy = false } = {}) => {
      const response = new PassThrough()
      response.closeSeen = false
      response.once("close", () => { response.closeSeen = true })
      response.statusCode = replyStatus
      response.headers = replyHeaders
      request.response = response
      if (preDestroy) response.destroy()
      const errorListenersBeforeAdapter = new Set(response.listeners("error"))
      const closeListenersBeforeAdapter = new Set(response.listeners("close"))
      onResponse(response)
      response.adapterListeners = {
        error: response.listeners("error").filter((listener) => !errorListenersBeforeAdapter.has(listener)),
        close: response.listeners("close").filter((listener) => !closeListenersBeforeAdapter.has(listener)),
      }
      if (!response.destroyed) {
        for (const chunk of replyChunks) response.write(chunk)
        if (replyEnd) response.end()
      }
      return response
    }
    calls.push({ options: requestOptions, request })
    return request
  }
  return { calls, requestImpl }
}

async function collect(body) {
  const chunks = []
  for await (const chunk of body) chunks.push(chunk)
  return Buffer.concat(chunks)
}

async function closed(stream) {
  if (stream.closeSeen) return
  await new Promise((resolve) => stream.once("close", resolve))
}

function errorCode(code) {
  return (error) => error?.code === code && error.message === code && !String(error).includes(secret) && !String(error).includes(socketPath)
}

function controlledClock() {
  let now = 0, next = 0
  const timers = new Map()
  return {
    now: () => now, utc: () => new Date(1_800_000_000_000 + now).toISOString(), cpu: () => ({ user: now * 10, system: now }),
    setTimeout(callback, ms) { const id = ++next; timers.set(id, { callback, at: now + ms }); return id },
    clearTimeout(id) { timers.delete(id) },
    advance(ms) { now += ms; for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.callback() } },
    timers,
  }
}

function controlledBacking(count = 8) {
  const clock = controlledClock(), fake = fakeHttp({ auto: false }), pendingStats = []
  const allowlist = Array.from({ length: count }, (_, index) => ({ cid: (index + 1).toString(16).padStart(64, "0"), role: `member-${index}`, labels: { "mount-rs.tidb.run": "owned-run" } }))
  const requestImpl = (requestOptions, onResponse) => {
    const request = fake.requestImpl(requestOptions, onResponse), end = request.end
    request.end = () => {
      end()
      if (requestOptions.path.includes("/stats?")) { pendingStats.push({ request, options: requestOptions, frames: 0 }); return }
      const id = requestOptions.path.split("/")[3], entry = allowlist.find((item) => item.cid === id)
      const value = requestOptions.path === "/version" ? { ApiVersion: "1.48", MinAPIVersion: "1.24", Os: "linux" }
        : { Id: id, Image: `sha256:${"c".repeat(64)}`, RestartCount: 0, State: { Running: true, StartedAt: "2026-09-26T00:00:00Z" }, Config: { Labels: entry.labels } }
      queueMicrotask(() => request.respond({ chunks: [Buffer.from(JSON.stringify(value))] }))
    }
    return request
  }
  const transport = moduleUnderTest.createBackingEngineTransport({ socketPath, allowlistedCids: allowlist.map((entry) => entry.cid), requestImpl })
  const observer = createBackingObserver({ allowlist, transport, clock })
  function primeStatsFrame(ms = 100) {
    clock.advance(ms)
    // Standard stream=false discards its first sampled frame internally.
    for (const held of pendingStats) { assert.equal(held.frames, 0); held.frames = 1 }
  }
  function releaseStats(ms = 1000, transform = (value) => value) {
    const held = pendingStats.splice(0)
    clock.advance(ms)
    for (const { request, options: requestOptions, frames } of held.reverse()) {
      assert.equal(frames, 1, "the standard API returns its second sampled frame")
      const id = requestOptions.path.split("/")[3]
      const value = { ...(requestOptions.path.includes("one-shot=true") ? {} : { id }), read: new Date(1_800_000_000_000 + clock.now()).toISOString(),
        cpu_stats: { cpu_usage: { total_usage: clock.now() } },
        blkio_stats: { io_service_bytes_recursive: [{ major: 8, minor: 0, op: "Read", value: clock.now() }], io_serviced_recursive: [{ major: 8, minor: 0, op: "Read", value: clock.now() }] },
        networks: { eth0: { rx_bytes: clock.now(), tx_bytes: clock.now() } } }
      request.respond({ chunks: [Buffer.from(JSON.stringify(transform(value, id)))] })
    }
    return held
  }
  return { clock, fake, allowlist, observer, transport, pendingStats, primeStatsFrame, releaseStats }
}

async function settleDispatch() {
  for (let turn = 0; turn < 4; turn++) await new Promise((resolve) => setImmediate(resolve))
}

test("construction is inert and one allowed GET streams exact bytes", async () => {
  const fake = fakeHttp({ chunks: [Buffer.from("ab"), Buffer.from("cd")] })
  const adapter = transport(fake)
  assert.equal(fake.calls.length, 0)
  assert.deepEqual(Object.keys(adapter), ["request"])
  const response = await adapter.request(options())
  assert.equal(response.status, 200)
  assert.equal((await collect(response.body)).toString(), "abcd")
  assert.equal(fake.calls.length, 1)
  assert.equal(fake.calls[0].request.endCount, 1)
  assert.equal(fake.calls[0].options.socketPath, socketPath)
  assert.equal(fake.calls[0].options.path, "/version")
  assert.equal(fake.calls[0].options.method, "GET")
  assert.equal(fake.calls[0].options.agent, false)
  assert.equal(fake.calls[0].request.destroyed, true)
})

test("exact owned inspect and standard non-streaming stats paths pass at both API bounds", async () => {
  const fake = fakeHttp()
  const adapter = transport(fake, [cid, otherCid])
  const paths = [
    [`/v1.41/containers/${cid}/json`, cid],
    [`/v1.41/containers/${cid}/stats?stream=false`, cid],
    [`/v1.51/containers/${otherCid}/stats?stream=false`, otherCid],
  ]
  for (const [path, ownedCid] of paths) {
    const response = await adapter.request(options({ path, cid: ownedCid }))
    assert.equal((await collect(response.body)).toString(), "ok")
  }
  assert.deepEqual(fake.calls.map((call) => call.options.path), paths.map(([path]) => path))
})

test("foreign, malformed and alternate routes never dispatch", async () => {
  const fake = fakeHttp()
  const adapter = transport(fake)
  const invalid = [
    { method: "POST" }, { path: "/containers/json" }, { path: "/version", cid },
    { path: `/v1.41/containers/${otherCid}/json`, cid: otherCid },
    { path: `/v1.41/containers/${cid}/json`, cid: otherCid },
    { path: `/v1.40/containers/${cid}/json`, cid },
    { path: `/v1.52/containers/${cid}/json`, cid },
    { path: `/v1.41/containers/${cid}/json/`, cid },
    { path: `/v1.41/containers/${cid}/%6ason`, cid },
    { path: `/v1.41/containers/${cid}/stats?one-shot=true&stream=false`, cid },
    { path: `/v1.41/containers/${cid}/stats?stream=true&one-shot=true`, cid },
    { path: `/v1.41/containers/${cid}/stats?stream=false&one-shot=true`, cid },
    { path: `/v1.41/containers/${cid}/stats?stream=false&one-shot=false`, cid },
    { path: `/v1.41/containers/${cid}/stats?stream=false&extra=1`, cid },
    { path: `/v1.41/containers/${cid}/stats?stream=true`, cid },
    { path: `/v1.41/containers/${cid}/json#fragment`, cid },
    { path: "/_ping" }, { maxResponseBytes: 1_048_577 },
  ]
  for (const edit of invalid) await assert.rejects(adapter.request(options(edit)), errorCode("engine_request_contract"))
  assert.equal(fake.calls.length, 0)
})

test("the observer requests server-identified stats through the exact production transport", async () => {
  const fake = fakeHttp({ auto: false })
  const owned = { cid, role: "tidb", labels: { "mount-rs.tidb.run": "owned-run" } }
  const requestImpl = (requestOptions, onResponse) => {
    const request = fake.requestImpl(requestOptions, onResponse)
    const end = request.end
    request.end = () => {
      end()
      // Pinned Moby v28.0.0 daemon/stats.go adds ID on the normal path;
      // one-shot encodes the Linux stats builder without that assignment.
      const value = requestOptions.path === "/version"
        ? { ApiVersion: "1.48", MinAPIVersion: "1.24", Os: "linux" }
        : requestOptions.path.endsWith("/json")
          ? { Id: cid, Image: `sha256:${"c".repeat(64)}`, RestartCount: 0, State: { Running: true, StartedAt: "2026-09-26T00:00:00Z" }, Config: { Labels: owned.labels } }
          : { ...(requestOptions.path.includes("one-shot=true") ? {} : { id: cid }), read: "2026-09-26T00:00:01Z", cpu_stats: { cpu_usage: { total_usage: 1 } } }
      queueMicrotask(() => request.respond({ chunks: [Buffer.from(JSON.stringify(value))] }))
    }
    return request
  }
  const observer = createBackingObserver({ allowlist: [owned], transport: moduleUnderTest.createBackingEngineTransport({ socketPath, allowlistedCids: [cid], requestImpl }) })
  const boundary = await observer.captureBoundary({ id: "selected", kind: "phase" })
  assert.equal(boundary.complete, true, "server ID must be observed rather than supplied by the observer")
  assert.equal(boundary.samples[0].stats.cid, cid)
  assert.equal(fake.calls.at(-1).options.path, `/v1.48/containers/${cid}/stats?stream=false`)
  assert.equal(observer.receipt().journal[0].mode, "stream=false")
  assert.equal(fake.calls.length, 3, "no discovery or identity-repair request is permitted")
})

test("eight owned peers share two sampled frames and fold replies in canonical order", async () => {
  const fixture = controlledBacking()
  const { observer, clock, allowlist, fake, pendingStats, primeStatsFrame, releaseStats } = fixture
  const session = createRunnerObserverSession(observer, { provider: "controlled", runId: "parallel", clock })
  const begin = session.begin("selected")
  await settleDispatch()
  const count = pendingStats.length
  if (count !== 8) { clock.advance(2001); await begin; assert.equal(count, 8, "all peers must await their server responses concurrently") }
  assert.equal(observer.receipt().journal.filter((entry) => entry.type === "boundary").length, 0, "IDs have not arrived yet")
  primeStatsFrame()
  assert.equal(observer.receipt().journal.filter((entry) => entry.type === "boundary").length, 0, "the first sampled frame is discarded by the server")
  releaseStats()
  await begin
  assert.equal(session.receipt().events[0].status, "ok")
  const end = session.end("selected", true, 50, 0, "complete")
  await settleDispatch()
  assert.equal(pendingStats.length, 8)
  primeStatsFrame()
  releaseStats()
  await end
  await session.finalize({ status: "ok", native_quiescent: true, owned_operations_settled: true, native_profiling_enabled: true, workload_native_evidence_complete: true, cleanup_complete: true })
  const receipt = session.receipt(), backing = receipt.backing_evidence
  assert.equal(receipt.complete, true)
  assert.deepEqual(backing.journal.filter((entry) => entry.type === "boundary").map((entry) => entry.samples.map((sample) => sample.cid)), [allowlist.map((entry) => entry.cid), allowlist.map((entry) => entry.cid)])
  assert.equal(backing.journal.find((entry) => entry.type === "interval").complete, true)
  assert.equal(fake.calls.filter((entry) => entry.options.path === "/version").length, 1)
  assert.equal(fake.calls.length, 33)
  assert.equal(backing.cost.wall_ms, 17600, "request waits overlap; do not sum them as exclusive observer wall time")
  assert.equal(backing.cost.peak_in_flight, 8)
  assert.equal(backing.cost.in_flight, 0)
  assert.match(backing.cost.wall_scope, /inclusive.*overlap/)
  assert.match(backing.cost.cpu_scope, /inclusive.*overlap/)
  assert.equal(receipt.events[0].wall_ms, 1100)
  assert.equal(receipt.events[1].wall_ms, 1100)
  assert.equal(clock.timers.size, 0)
})

test("one failed peer retains successful sibling samples without leaking failed identity values", async () => {
  const fixture = controlledBacking()
  const capture = fixture.observer.captureBoundary({ id: "peer-failure", kind: "phase" })
  await settleDispatch()
  const count = fixture.pendingStats.length
  if (count !== 8) { fixture.clock.advance(2001); await capture; assert.equal(count, 8) }
  const failed = fixture.allowlist[3].cid
  fixture.primeStatsFrame()
  fixture.releaseStats(1000, (value, id) => id === failed ? { ...value, id: secret } : value)
  const boundary = await capture
  assert.equal(boundary.complete, false)
  assert.deepEqual(boundary.samples.map((sample) => sample.cid), fixture.allowlist.filter((entry) => entry.cid !== failed).map((entry) => entry.cid))
  assert.ok(boundary.issues.includes("stats_identity_mismatch"))
  assert.equal(JSON.stringify(fixture.observer.receipt()).includes(secret), false)
})

test("a delayed second sampled frame preserves the parallel deadline and retires all requests", async () => {
  const fixture = controlledBacking()
  const session = createRunnerObserverSession(fixture.observer, { provider: "controlled", runId: "aborted", clock: fixture.clock })
  const begin = session.begin("selected")
  await settleDispatch()
  const count = fixture.pendingStats.length
  fixture.primeStatsFrame(1000)
  assert.equal(session.receipt().events.length, 0, "the first frame cannot finish the hook")
  fixture.clock.advance(1001)
  await begin
  await session.finalize({ status: "ok", cleanup_complete: true })
  assert.equal(count, 8)
  assert.equal(session.receipt().complete, false)
  assert.equal(session.receipt().events[0].issue, "request_deadline")
  const held = fixture.pendingStats.splice(0)
  for (const { request } of held) {
    assert.equal(request.destroyed, true)
    assert.equal(request.listenerCount("error"), 0)
    assert.equal(request.listenerCount("close"), 0)
    const late = request.respond({ chunks: [Buffer.from(secret)], end: false })
    late.emit("error", new Error(secret))
    await closed(late)
    assert.equal(late.destroyed, true)
    assert.equal(late.listenerCount("error"), 0)
    assert.equal(late.listenerCount("close"), 0)
  }
  assert.equal(JSON.stringify(session.receipt()).includes(secret), false)
  assert.equal(fixture.clock.timers.size, 0)
})

test("external finalization drains all real transport requests before freezing their accounting", async () => {
  const fixture = controlledBacking()
  const capture = fixture.observer.captureBoundary({ id: "retire", kind: "phase" })
  await settleDispatch()
  const count = fixture.pendingStats.length
  const firstFinalization = fixture.observer.finalize({ status: "failed", cleanup_complete: true })
  const secondFinalization = fixture.observer.finalize({ status: "failed", cleanup_complete: true })
  const [finalized, repeated] = await Promise.all([firstFinalization, secondFinalization])
  await capture
  assert.equal(count, 8)
  assert.equal(finalized.complete, false)
  assert.ok(finalized.issues.includes("unsettled_request_at_finalization"))
  assert.equal(finalized.cost.in_flight, 0, "freeze accounting after the aborted wire requests settle")
  assert.equal(finalized.cost.peak_in_flight, 8)
  assert.equal(fixture.clock.timers.size, 0)
  for (const { request } of fixture.pendingStats) {
    assert.equal(request.destroyed, true)
    assert.equal(request.listenerCount("error"), 0)
    assert.equal(request.listenerCount("close"), 0)
  }
  assert.deepEqual(fixture.observer.receipt(), finalized)
  assert.deepEqual(repeated, finalized)
})

test("transport bounds parallelism at sixteen owned CIDs and one request per CID", async () => {
  const fake = fakeHttp({ auto: false })
  const cids = Array.from({ length: 16 }, (_, index) => (index + 1).toString(16).padStart(64, "0"))
  const adapter = transport(fake, cids), controllers = cids.map(() => new AbortController())
  const pending = cids.map((id, index) => adapter.request(options({ path: `/v1.48/containers/${id}/json`, cid: id, signal: controllers[index].signal })))
  for (const promise of pending) promise.catch(() => {})
  try { assert.equal(fake.calls.length, 16) }
  catch (error) { controllers.forEach((controller) => controller.abort()); await Promise.allSettled(pending); throw error }
  await assert.rejects(adapter.request(options({ path: `/v1.48/containers/${cids[0]}/stats?stream=false`, cid: cids[0] })), errorCode("engine_request_contract"))
  await assert.rejects(adapter.request(options()), errorCode("engine_request_contract"))
  const foreign = "f".repeat(64)
  await assert.rejects(adapter.request(options({ path: `/v1.48/containers/${foreign}/json`, cid: foreign })), errorCode("engine_request_contract"))
  assert.throws(() => transport(fake, [...cids, foreign]), /engine_request_contract/)
  controllers[0].abort()
  await assert.rejects(pending[0], errorCode("engine_aborted"))
  const replacement = adapter.request(options({ path: `/v1.48/containers/${cids[0]}/json`, cid: cids[0] }))
  const late = fake.calls[0].request.respond({ chunks: [Buffer.from(secret)], end: false })
  late.emit("error", new Error(secret))
  await closed(late)
  await assert.rejects(adapter.request(options({ path: `/v1.48/containers/${cids[0]}/json`, cid: cids[0] })), errorCode("engine_request_contract"))
  for (const call of fake.calls.slice(1)) call.request.respond({ chunks: [Buffer.from("ok")] })
  for (const result of await Promise.all(pending.slice(1))) await collect(result.body)
  await collect((await replacement).body)
  assert.equal(fake.calls.length, 17)
  assert.ok(fake.calls.every((call) => call.request.destroyed))
})

test("missing request input has a fixed contract error before dispatch", async () => {
  const fake = fakeHttp()
  await assert.rejects(transport(fake).request(undefined), errorCode("engine_request_contract"))
  assert.equal(fake.calls.length, 0)
})

test("constructor rejects unowned or duplicate CIDs without opening a request", () => {
  const fake = fakeHttp()
  for (const cids of [[], ["A".repeat(64)], [cid, cid], Array(17).fill(cid)]) {
    assert.throws(() => transport(fake, cids), errorCode("engine_request_contract"))
  }
  assert.equal(fake.calls.length, 0)
})

test("constructor failures and throwing accessors have fixed errors", () => {
  const fake = fakeHttp()
  for (const input of [undefined, null, { get socketPath() { throw new Error(secret) } }, { socketPath, get allowlistedCids() { throw new Error(secret) } }]) {
    assert.throws(() => moduleUnderTest.createBackingEngineTransport(input), errorCode("engine_request_contract"))
  }
  assert.equal(fake.calls.length, 0)
})

test("redirect and other non-200 status stop without following or exposing headers", async () => {
  for (const status of [301, 302, 307, 308, 404, 500]) {
    const fake = fakeHttp({ status, headers: { location: secret }, chunks: [Buffer.from(secret)] })
    await assert.rejects(transport(fake).request(options()), errorCode("engine_http_status"))
    assert.equal(fake.calls.length, 1)
    assert.equal(fake.calls[0].request.destroyed, true)
  }
})

test("declared and streamed body limits fail closed; exact cap succeeds", async () => {
  const declared = fakeHttp({ headers: { "content-length": "33" }, chunks: [Buffer.from(secret)] })
  await assert.rejects(transport(declared).request(options()), errorCode("engine_response_bytes_cap"))
  assert.equal(declared.calls[0].request.destroyed, true)
  const streamed = fakeHttp({ chunks: [Buffer.alloc(31), Buffer.from("xy")] })
  const over = await transport(streamed).request(options())
  await assert.rejects(collect(over.body), errorCode("engine_response_bytes_cap"))
  assert.equal(streamed.calls[0].request.destroyed, true)
  const exact = fakeHttp({ chunks: [Buffer.alloc(31), Buffer.from("x")] })
  const within = await transport(exact).request(options())
  assert.equal((await collect(within.body)).length, 32)
})

test("invalid or ambiguous declared length cannot bypass the stream cap", async () => {
  for (const length of ["NaN", "-1", ["1", "2"]]) {
    const fake = fakeHttp({ headers: { "content-length": length }, chunks: [Buffer.from("ok")] })
    await assert.rejects(transport(fake).request(options()), errorCode("engine_request_contract"))
    assert.equal(fake.calls[0].request.destroyed, true)
  }
})

test("pre-abort and abort before headers settle without raw error details", async () => {
  const fake = fakeHttp({ auto: false })
  const adapter = transport(fake)
  const before = new AbortController()
  before.abort()
  await assert.rejects(adapter.request(options({ signal: before.signal })), errorCode("engine_aborted"))
  assert.equal(fake.calls.length, 0)
  const during = new AbortController()
  const pending = adapter.request(options({ signal: during.signal }))
  assert.equal(fake.calls.length, 1)
  during.abort()
  await assert.rejects(pending, errorCode("engine_aborted"))
  assert.equal(fake.calls[0].request.destroyed, true)
})

test("abort during body, underlying error and early close are sanitized", async () => {
  const fake = fakeHttp({ auto: false })
  const adapter = transport(fake)
  const controller = new AbortController()
  const pending = adapter.request(options({ signal: controller.signal }))
  fake.calls[0].request.respond({ chunks: [Buffer.from("ok")], end: false })
  const response = await pending
  controller.abort()
  await assert.rejects(collect(response.body), errorCode("engine_aborted"))

  const second = adapter.request(options())
  fake.calls[1].request.emit("error", new Error(secret))
  await assert.rejects(second, errorCode("engine_transport_io"))

  const third = adapter.request(options())
  const request = fake.calls[2].request
  request.respond({ chunks: [Buffer.from("ok")], end: false })
  const early = await third
  request.response.destroy(new Error(secret))
  await assert.rejects(collect(early.body), errorCode("engine_transport_io"))
})

test("consumer break destroys the unfinished response and releases overlap", async () => {
  const fake = fakeHttp({ chunks: [Buffer.from("ok")], end: false })
  const adapter = transport(fake)
  const response = await adapter.request(options())
  for await (const chunk of response.body) { assert.equal(chunk.toString(), "ok"); break }
  assert.equal(fake.calls[0].request.destroyed, true)
  const next = await adapter.request(options())
  for await (const _chunk of next.body) break
  assert.equal(fake.calls.length, 2)
})

test("abort after headers releases the transport even if the caller never iterates", async () => {
  const fake = fakeHttp({ auto: false })
  const adapter = transport(fake)
  const controller = new AbortController()
  const first = adapter.request(options({ signal: controller.signal }))
  fake.calls[0].request.respond({ chunks: [Buffer.from("ok")], end: false })
  const response = await first
  controller.abort()
  const next = adapter.request(options())
  next.catch(() => {})
  assert.equal(fake.calls.length, 2, "an aborted unconsumed body must release the request slot")
  fake.calls[1].request.respond({ chunks: [Buffer.from("new")] })
  assert.equal((await collect((await next).body)).toString(), "new")
  await assert.rejects(collect(response.body), errorCode("engine_aborted"))
})

test("late headers after abort or request error are destroyed without reclaiming the slot", async () => {
  for (const stop of ["abort", "error"]) {
    const fake = fakeHttp({ auto: false })
    const adapter = transport(fake)
    const controller = new AbortController()
    const first = adapter.request(options({ signal: controller.signal }))
    const oldRequest = fake.calls[0].request
    if (stop === "abort") controller.abort()
    else oldRequest.emit("error", new Error(secret))
    await assert.rejects(first, errorCode(stop === "abort" ? "engine_aborted" : "engine_transport_io"))
    const late = oldRequest.respond({ chunks: [Buffer.from(secret)], end: false })
    assert.equal(late.destroyed, true, "a post-terminal response must be closed immediately")
    late.emit("error", new Error(secret))
    await closed(late)
    assert.equal(late.listenerCount("error"), 0)
    assert.equal(late.listenerCount("close"), 0)
    const next = adapter.request(options())
    assert.equal(fake.calls.length, 2)
    fake.calls[1].request.respond({ chunks: [Buffer.from("new")] })
    assert.equal((await collect((await next).body)).toString(), "new")
  }
})

test("an already destroyed late response still guards errors until close", async () => {
  const fake = fakeHttp({ auto: false })
  const adapter = transport(fake)
  const controller = new AbortController()
  const pending = adapter.request(options({ signal: controller.signal }))
  controller.abort()
  await assert.rejects(pending, errorCode("engine_aborted"))
  const late = fake.calls[0].request.respond({ preDestroy: true, end: false })
  late.emit("error", new Error(secret))
  await closed(late)
  assert.equal(late.listenerCount("error"), 0)
  assert.equal(late.listenerCount("close"), 0)
})

test("request and response listeners retire after EOF, break and abort", async () => {
  const fake = fakeHttp({ auto: false })
  const adapter = transport(fake)
  const states = ["eof", "break", "abort"]
  for (const state of states) {
    const controller = new AbortController()
    const pending = adapter.request(options({ signal: controller.signal }))
    const request = fake.calls.at(-1).request
    const stream = request.respond({ chunks: [Buffer.from("ok")], end: state === "eof" })
    const boundRequestError = request.listeners("error")[0]
    const boundRequestClose = request.listeners("close")[0]
    assert.equal(stream.adapterListeners.error.length, 1, `${state}: exactly one adapter error listener`)
    assert.equal(stream.adapterListeners.close.length, 1, `${state}: exactly one adapter close listener`)
    const boundResponseError = stream.adapterListeners.error[0]
    const boundResponseClose = stream.adapterListeners.close[0]
    for (const listener of [boundRequestError, boundRequestClose, boundResponseError, boundResponseClose]) assert.equal(typeof listener, "function")
    const result = await pending
    if (state === "eof") await collect(result.body)
    if (state === "break") for await (const _chunk of result.body) break
    if (state === "abort") {
      controller.abort()
      await assert.rejects(collect(result.body), errorCode("engine_aborted"))
    }
    await closed(stream)
    assert.equal(request.listeners("error").includes(boundRequestError), false, `${state}: request error listener`)
    assert.equal(request.listeners("close").includes(boundRequestClose), false, `${state}: request close listener`)
    assert.equal(stream.listeners("error").includes(boundResponseError), false, `${state}: response error listener`)
    assert.equal(stream.listeners("close").includes(boundResponseClose), false, `${state}: response close listener`)
  }
  assert.equal(fake.calls.length, 3)
})

test("only one request remains active and the 258th dispatch is refused", async () => {
  const held = fakeHttp({ auto: false })
  const adapter = transport(held)
  const pending = adapter.request(options())
  await assert.rejects(adapter.request(options()), errorCode("engine_request_contract"))
  assert.equal(held.calls.length, 1)
  held.calls[0].request.respond({ chunks: [Buffer.from("ok")] })
  await collect((await pending).body)

  const fast = fakeHttp()
  const bounded = transport(fast)
  for (let index = 0; index < 257; index++) await collect((await bounded.request(options())).body)
  await assert.rejects(bounded.request(options()), errorCode("engine_request_contract"))
  assert.equal(fast.calls.length, 257)
})
