import assert from "node:assert/strict"
import { EventEmitter } from "node:events"
import { PassThrough } from "node:stream"
import test from "node:test"

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

test("exact owned inspect and one-shot stats paths pass at both API bounds", async () => {
  const fake = fakeHttp()
  const adapter = transport(fake, [cid, otherCid])
  const paths = [
    [`/v1.41/containers/${cid}/json`, cid],
    [`/v1.51/containers/${otherCid}/stats?stream=false&one-shot=true`, otherCid],
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
    { path: `/v1.41/containers/${cid}/json#fragment`, cid },
    { path: "/_ping" }, { maxResponseBytes: 1_048_577 },
  ]
  for (const edit of invalid) await assert.rejects(adapter.request(options(edit)), errorCode("engine_request_contract"))
  assert.equal(fake.calls.length, 0)
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
