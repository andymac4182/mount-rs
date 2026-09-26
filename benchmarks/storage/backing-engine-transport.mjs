import http from "node:http"

const MAX_CIDS = 16
const MAX_REQUESTS = 257
const MAX_RESPONSE_BYTES = 1_048_576
const CID = /^[a-f0-9]{64}$/
const INSPECT = /^\/v1\.(4[1-9]|5[01])\/containers\/([a-f0-9]{64})\/json$/
const STATS = /^\/v1\.(4[1-9]|5[01])\/containers\/([a-f0-9]{64})\/stats\?stream=false$/

class EngineTransportError extends Error {
  constructor(code) {
    super(code)
    this.code = code
  }
}

const failure = (code) => new EngineTransportError(code)

function validateRoute(input, cids) {
  if (input === null || typeof input !== "object") throw failure("engine_request_contract")
  const { method, path, cid, signal, maxResponseBytes } = input
  if (method !== "GET" || !(signal instanceof AbortSignal) || !Number.isSafeInteger(maxResponseBytes) || maxResponseBytes < 1 || maxResponseBytes > MAX_RESPONSE_BYTES) throw failure("engine_request_contract")
  if (typeof path !== "string") throw failure("engine_request_contract")
  if (path === "/version") {
    if (cid !== undefined) throw failure("engine_request_contract")
  } else {
    const match = INSPECT.exec(path) ?? STATS.exec(path)
    if (!match || cid !== match[2] || !cids.has(cid)) throw failure("engine_request_contract")
  }
  if (signal.aborted) throw failure("engine_aborted")
}

function declaredLength(headers, maxResponseBytes) {
  const value = headers?.["content-length"]
  if (value === undefined) return
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) throw failure("engine_request_contract")
  if (BigInt(value) > BigInt(maxResponseBytes)) throw failure("engine_response_bytes_cap")
}

/** The fixture supplies the socket path and exact CIDs; no connection opens until request(). */
export function createBackingEngineTransport(input) {
  let socketPath, requestImpl, cids
  try {
    if (input === null || typeof input !== "object") throw failure("engine_request_contract")
    const { socketPath: path, allowlistedCids, requestImpl: implementation = http.request } = input
    if (typeof path !== "string" || !path.startsWith("/") || path.includes("\0") || Buffer.byteLength(path) > 4096 || typeof implementation !== "function") throw failure("engine_request_contract")
    if (!Array.isArray(allowlistedCids) || allowlistedCids.length < 1 || allowlistedCids.length > MAX_CIDS || allowlistedCids.some((id) => typeof id !== "string" || !CID.test(id))) throw failure("engine_request_contract")
    const unique = new Set(allowlistedCids)
    if (unique.size !== allowlistedCids.length) throw failure("engine_request_contract")
    socketPath = path
    requestImpl = implementation
    cids = unique
  } catch {
    throw failure("engine_request_contract")
  }

  const active = new Set()
  let dispatched = 0
  async function request(input) {
    validateRoute(input, cids)
    const key = input.cid ?? "version"
    if (active.has(key) || active.size >= cids.size || dispatched >= MAX_REQUESTS) throw failure("engine_request_contract")
    active.add(key)
    dispatched++
    const { path, signal, maxResponseBytes } = input
    let outgoing
    let incoming
    let headersSettled = false
    let finished = false
    let bodyError = null
    let rejectHeaders
    let onOutgoingError, onOutgoingClose, onIncomingError, onIncomingClose

    function detachOutgoing() {
      if (!outgoing) return
      outgoing.off("error", onOutgoingError)
      outgoing.off("close", onOutgoingClose)
    }
    function detachIncoming() {
      if (!incoming || !onIncomingError) return
      incoming.off("error", onIncomingError)
      incoming.off("close", onIncomingClose)
    }
    function discardLate(stream) {
      const ignoreError = () => {}
      const detach = () => {
        stream.off("error", ignoreError)
        stream.off("close", detach)
      }
      stream.on("error", ignoreError)
      stream.once("close", detach)
      if (!stream.destroyed) stream.destroy()
    }

    function finish() {
      if (finished) return
      finished = true
      active.delete(key)
      signal.removeEventListener("abort", abort)
      if (incoming && !incoming.destroyed) incoming.destroy()
      if (outgoing && !outgoing.destroyed) outgoing.destroy()
    }
    function fail(code) {
      const error = failure(code)
      bodyError ??= error
      if (!headersSettled) {
        headersSettled = true
        finish()
        rejectHeaders(error)
      } else {
        finish()
      }
    }
    function abort() { fail("engine_aborted") }

    const response = new Promise((resolve, reject) => {
      rejectHeaders = reject
      signal.addEventListener("abort", abort, { once: true })
      if (signal.aborted) { abort(); return }
      try {
        outgoing = requestImpl({ socketPath, method: "GET", path, agent: false, headers: { Accept: "application/json" }, signal }, (stream) => {
          incoming = stream
          if (finished || signal.aborted) {
            if (!finished) abort()
            discardLate(stream)
            return
          }
          onIncomingError = () => fail(signal.aborted ? "engine_aborted" : "engine_transport_io")
          onIncomingClose = () => {
            if (!finished && stream.complete !== true && !stream.readableEnded) fail(signal.aborted ? "engine_aborted" : "engine_transport_io")
            detachIncoming()
          }
          stream.on("error", onIncomingError)
          stream.on("close", onIncomingClose)
          if (stream.statusCode !== 200) { fail("engine_http_status"); return }
          try { declaredLength(stream.headers, maxResponseBytes) }
          catch (error) { fail(error.code === "engine_response_bytes_cap" ? error.code : "engine_request_contract"); return }
          headersSettled = true
          resolve({ status: 200, body: body() })
        })
        onOutgoingError = () => fail(signal.aborted ? "engine_aborted" : "engine_transport_io")
        onOutgoingClose = () => {
          if (!headersSettled) fail(signal.aborted ? "engine_aborted" : "engine_transport_io")
          detachOutgoing()
        }
        outgoing.on("error", onOutgoingError)
        outgoing.on("close", onOutgoingClose)
        outgoing.end()
      } catch {
        fail("engine_transport_io")
      }
    })

    async function* body() {
      let bytes = 0
      try {
        if (bodyError) throw bodyError
        for await (const chunk of incoming) {
          if (signal.aborted) throw failure("engine_aborted")
          if (bodyError) throw bodyError
          if (!(chunk instanceof Uint8Array)) throw failure("engine_request_contract")
          bytes += chunk.byteLength
          if (bytes > maxResponseBytes) throw failure("engine_response_bytes_cap")
          yield chunk
        }
        if (signal.aborted) throw failure("engine_aborted")
        if (bodyError) throw bodyError
        if (!incoming.readableEnded) throw failure("engine_transport_io")
      } catch (error) {
        throw error instanceof EngineTransportError ? error : failure(signal.aborted ? "engine_aborted" : "engine_transport_io")
      } finally {
        finish()
      }
    }
    return response
  }
  return Object.freeze({ request })
}
