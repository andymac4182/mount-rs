import assert from "node:assert/strict"
import { test } from "node:test"

let api = {}
try { api = await import("./namespace-presence.mjs") } catch (error) {
  if (error.code !== "ERR_MODULE_NOT_FOUND" || !error.message.includes("namespace-presence.mjs")) throw error
}

function implementation() {
  assert.equal(typeof api.validateSplitNamespacePresenceReceipt, "function", "namespace presence validator is missing")
  assert.equal(typeof api.observeSplitNamespacePresence, "function", "namespace presence query wrapper is missing")
  return api
}

const good = () => ({
  schema: "mount-rs.split-namespace-presence.v1",
  namespace_absent: true,
  metadata: {
    provider: "tidb",
    key_scope: "exact_input_utf8_bytes",
    schema_setup: "shared_ddl_and_session_configuration",
    row_presence: { metadata: false, inodes: false, compact_guards: false, block_authority: false, blocks: false },
    observed_at_ns: "123456789",
  },
  blobs: {
    provider: "rustfs",
    scope: "canonical_ascii_prefix_descendants",
    observation: "signed_list_page_api",
    requested_max_keys: 1,
    prefix_absent: true,
    observed_at_ns: "234567890",
  },
  pool_shutdown: { confirmed: true, elapsed_ns: "12345" },
  clock: "elapsed_monotonic_since_native_preflight_start",
  consistency: "separate_observations_no_reservation",
  limits: {
    probe_deadline_ms: 30000,
    pool_shutdown_deadline_ms: 15000,
    list_request_keys: 1,
    response_byte_cap: "unavailable",
    server_truncation_flag: "unavailable",
    deadline_semantics: "cooperative_await_with_elapsed_recheck",
  },
})

function fixedFailure(code) {
  const messages = {
    NAMESPACE_PRESENCE_API_UNAVAILABLE: "namespace presence inspection is unavailable",
    NAMESPACE_PRESENCE_QUERY_FAILED: "namespace presence query failed",
    NAMESPACE_PRESENCE_INVALID_RECEIPT: "namespace presence receipt is invalid",
    NAMESPACE_PRESENCE_RECEIPT_TOO_LARGE: "namespace presence receipt exceeds its byte limit",
    NAMESPACE_PRESENCE_DEADLINE_EXCEEDED: "namespace presence observation exceeds its native deadline",
    NAMESPACE_PRESENCE_NOT_ABSENT: "namespace presence observations do not establish absence",
  }
  return (error) => {
    assert.equal(error.code, code)
    assert.equal(error.message, messages[code])
    assert.deepEqual(Object.keys(error), ["code"])
    assert.equal(error.cause, undefined)
    assert.equal(error.path, undefined)
    assert.equal(error.receipt, undefined)
    assert.equal(JSON.stringify(error).includes("private"), false)
    assert.equal(error.message.includes("private"), false)
    return true
  }
}

function nodeAt(source, path) {
  return path.reduce((value, field) => value[field], source)
}

function deeplyFrozen(value) {
  assert.equal(Object.isFrozen(value), true)
  for (const child of Object.values(value)) if (child && typeof child === "object") deeplyFrozen(child)
}

test("accepted receipt owns a deeply frozen fixed copy with explicit observation limits", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  const source = good()
  const first = validateSplitNamespacePresenceReceipt(JSON.stringify(source))
  const second = validateSplitNamespacePresenceReceipt(JSON.stringify(source))
  assert.deepEqual(first, source)
  assert.notEqual(first, second)
  assert.notEqual(first.metadata, second.metadata)
  deeplyFrozen(first)
  assert.throws(() => { first.metadata.row_presence.blocks = true }, TypeError)
  assert.equal(first.consistency, "separate_observations_no_reservation")
  assert.equal(first.limits.response_byte_cap, "unavailable")
  assert.equal(first.limits.server_truncation_flag, "unavailable")
})

test("zero and inclusive native deadline boundaries remain valid", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  for (const [observation, shutdown] of [["0", "0"], ["30000000000", "15000000000"]]) {
    const source = good()
    source.metadata.observed_at_ns = observation
    source.blobs.observed_at_ns = observation
    source.pool_shutdown.elapsed_ns = shutdown
    assert.deepEqual(validateSplitNamespacePresenceReceipt(JSON.stringify(source)), source)
  }
})

test("primitive native JSON strings are required without invoking coercion hooks", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  const hostile = { toString() { assert.fail("input coercion must not execute") } }
  for (const source of [null, undefined, good(), [], 1, false, new String(JSON.stringify(good())), Buffer.from(JSON.stringify(good())), hostile]) {
    assert.throws(() => validateSplitNamespacePresenceReceipt(source), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
  }
})

test("UTF-8 receipt bytes are bounded before parsing with exact cap acceptance", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  const json = JSON.stringify(good())
  const exact = json + " ".repeat(4096 - Buffer.byteLength(json))
  assert.equal(Buffer.byteLength(exact), 4096)
  assert.deepEqual(validateSplitNamespacePresenceReceipt(exact), good())
  assert.throws(() => validateSplitNamespacePresenceReceipt(exact + " "), fixedFailure("NAMESPACE_PRESENCE_RECEIPT_TOO_LARGE"))
  const multibyte = json.replace("mount-rs.split-namespace-presence.v1", "é".repeat(2000))
  assert.ok(multibyte.length < 4096)
  assert.ok(Buffer.byteLength(multibyte) > 4096)
  assert.throws(() => validateSplitNamespacePresenceReceipt(multibyte), fixedFailure("NAMESPACE_PRESENCE_RECEIPT_TOO_LARGE"))
})

test("malformed and nonobject JSON exposes no native text", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  for (const source of ["", "private credential", "{\"private\":", "null", "[]", "true", "1", "\"private\"", JSON.stringify(good()) + " private"]) {
    assert.throws(() => validateSplitNamespacePresenceReceipt(source), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
  }
})

test("every object is closed against missing and extra fields", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  for (const path of [[], ["metadata"], ["metadata", "row_presence"], ["blobs"], ["pool_shutdown"], ["limits"]]) {
    for (const field of Object.keys(nodeAt(good(), path))) {
      const source = good()
      delete nodeAt(source, path)[field]
      assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
    }
    const extra = good()
    nodeAt(extra, path).private = "private bucket and namespace"
    assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(extra)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
  }
})

test("nested objects cannot be replaced by null or arrays", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  for (const path of [["metadata"], ["metadata", "row_presence"], ["blobs"], ["pool_shutdown"], ["limits"]]) {
    for (const value of [null, []]) {
      const source = good()
      nodeAt(source, path.slice(0, -1))[path.at(-1)] = value
      assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
    }
  }
})

test("all enum and numeric limit declarations must match the native contract", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  const paths = [["schema"], ["clock"], ["consistency"], ["metadata", "provider"], ["metadata", "key_scope"], ["metadata", "schema_setup"],
    ["blobs", "provider"], ["blobs", "scope"], ["blobs", "observation"], ["blobs", "requested_max_keys"],
    ...Object.keys(good().limits).map((field) => ["limits", field])]
  for (const path of paths) {
    const source = good()
    nodeAt(source, path.slice(0, -1))[path.at(-1)] = "private replacement"
    assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
  }
  for (const [field, value] of [["probe_deadline_ms", 30001], ["pool_shutdown_deadline_ms", 14999], ["list_request_keys", 2], ["response_byte_cap", 4096], ["server_truncation_flag", true]]) {
    const source = good()
    source.limits[field] = value
    assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
  }
})

test("each metadata row independently prevents an absence acceptance", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  for (const field of Object.keys(good().metadata.row_presence)) {
    const source = good()
    source.metadata.row_presence[field] = true
    assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_NOT_ABSENT"))
  }
})

test("absence flags cannot override contaminated metadata or blob observations", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  for (const path of [["namespace_absent"], ["blobs", "prefix_absent"]]) {
    const source = good()
    nodeAt(source, path.slice(0, -1))[path.at(-1)] = false
    assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_NOT_ABSENT"))
  }
})

test("boolean declarations are strict and pool shutdown must be confirmed", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  const paths = [["namespace_absent"], ["blobs", "prefix_absent"], ["pool_shutdown", "confirmed"],
    ...Object.keys(good().metadata.row_presence).map((field) => ["metadata", "row_presence", field])]
  for (const path of paths) {
    for (const value of [0, 1, "false", "true", null]) {
      const source = good()
      nodeAt(source, path.slice(0, -1))[path.at(-1)] = value
      assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
    }
  }
  const unconfirmed = good()
  unconfirmed.pool_shutdown.confirmed = false
  assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(unconfirmed)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
})

test("timestamps must be canonical lossless unsigned u128 decimals", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  for (const path of [["metadata", "observed_at_ns"], ["blobs", "observed_at_ns"], ["pool_shutdown", "elapsed_ns"]]) {
    for (const value of [1, true, null, "", "01", "00", "-1", "+1", " 1", "1 ", "1\n", "1e3", "1.0", "340282366920938463463374607431768211456", "9".repeat(1000)]) {
      const source = good()
      nodeAt(source, path.slice(0, -1))[path.at(-1)] = value
      assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
    }
  }
})

test("observation order and native elapsed deadlines cannot be weakened", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  const reversed = good()
  reversed.metadata.observed_at_ns = "234567891"
  assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(reversed)), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
  for (const [path, value] of [[["blobs", "observed_at_ns"], "30000000001"], [["metadata", "observed_at_ns"], "30000000001"],
    [["pool_shutdown", "elapsed_ns"], "15000000001"], [["blobs", "observed_at_ns"], "340282366920938463463374607431768211455"]]) {
    const source = good()
    nodeAt(source, path.slice(0, -1))[path.at(-1)] = value
    assert.throws(() => validateSplitNamespacePresenceReceipt(JSON.stringify(source)), fixedFailure("NAMESPACE_PRESENCE_DEADLINE_EXCEEDED"))
  }
})

test("duplicate JSON members cannot hide contamination including escaped names", () => {
  const { validateSplitNamespacePresenceReceipt } = implementation()
  const json = JSON.stringify(good())
  for (const source of [json.replace('"namespace_absent":true', '"namespace_absent":false,"namespace_absent":true'),
    json.replace('"blocks":false', '"blocks":true,"blocks":false'),
    json.replace('"blocks":false', '"blocks":true,"\\u0062locks":false'),
    json.replace('"provider":"rustfs"', '"provider":"private","provider":"rustfs"')]) {
    assert.throws(() => validateSplitNamespacePresenceReceipt(source), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
  }
})

test("query reads the API once and preserves its receiver and exact argument references", async () => {
  const { observeSplitNamespacePresence } = implementation()
  const metadata = Object.freeze({ private: "private metadata URI" })
  const blocks = Object.freeze({ private: "private bucket and credential" })
  let reads = 0, calls = 0
  const binding = Object.defineProperty({}, "inspectSplitNamespacePresence", { get() {
    reads++
    return function(receivedMetadata, receivedBlocks) {
      calls++
      assert.equal(this, binding)
      assert.equal(receivedMetadata, metadata)
      assert.equal(receivedBlocks, blocks)
      return Promise.resolve(JSON.stringify(good()))
    }
  } })
  const accepted = await observeSplitNamespacePresence(binding, metadata, blocks)
  assert.deepEqual(accepted, good())
  assert.equal(reads, 1)
  assert.equal(calls, 1)
  assert.equal(JSON.stringify(accepted).includes("private"), false)
})

test("query awaits the one owned promise without returning a partial receipt", async () => {
  const { observeSplitNamespacePresence } = implementation()
  let release, calls = 0, settled = false
  const pending = new Promise((resolve) => { release = resolve })
  const binding = { inspectSplitNamespacePresence() { calls++; return pending } }
  const observed = observeSplitNamespacePresence(binding, {}, {}).then((receipt) => { settled = true; return receipt })
  await Promise.resolve()
  assert.equal(calls, 1)
  assert.equal(settled, false)
  release(JSON.stringify(good()))
  assert.deepEqual(await observed, good())
  assert.equal(calls, 1)
})

test("missing native API remains unavailable without construction", async () => {
  const { observeSplitNamespacePresence } = implementation()
  for (const binding of [null, undefined, {}, { inspectSplitNamespacePresence: "private" }, { createChunkedDriver() { assert.fail("filesystem must not be created") } }]) {
    await assert.rejects(() => observeSplitNamespacePresence(binding, {}, {}), fixedFailure("NAMESPACE_PRESENCE_API_UNAVAILABLE"))
  }
})

test("throwing lookup and query errors become fixed failures without retries or retained details", async () => {
  const { observeSplitNamespacePresence } = implementation()
  const raw = Object.assign(new Error("private SQL, credential and namespace"), { code: "private code", path: "private path", cause: "private cause" })
  let reads = 0
  const accessor = Object.defineProperty({}, "inspectSplitNamespacePresence", { get() { reads++; throw raw } })
  await assert.rejects(() => observeSplitNamespacePresence(accessor, {}, {}), fixedFailure("NAMESPACE_PRESENCE_QUERY_FAILED"))
  assert.equal(reads, 1)
  for (const asynchronous of [false, true]) {
    let calls = 0
    const binding = { inspectSplitNamespacePresence() { calls++; if (asynchronous) return Promise.reject(raw); throw raw } }
    await assert.rejects(() => observeSplitNamespacePresence(binding, {}, {}), fixedFailure("NAMESPACE_PRESENCE_QUERY_FAILED"))
    assert.equal(calls, 1)
  }
})

test("query cannot use a shadowed function call property", async () => {
  const { observeSplitNamespacePresence } = implementation()
  let calls = 0
  const query = function() { calls++; return JSON.stringify(good()) }
  Object.defineProperty(query, "call", { get() { assert.fail("shadowed call must not be accessed") } })
  assert.deepEqual(await observeSplitNamespacePresence({ inspectSplitNamespacePresence: query }, {}, {}), good())
  assert.equal(calls, 1)
})

test("one invalid returned receipt fails without retry or raw native object coercion", async () => {
  const { observeSplitNamespacePresence } = implementation()
  for (const returned of ["private credential", good(), { toString() { assert.fail("native return coercion must not execute") } }]) {
    let calls = 0
    const binding = { inspectSplitNamespacePresence() { calls++; return Promise.resolve(returned) } }
    await assert.rejects(() => observeSplitNamespacePresence(binding, {}, {}), fixedFailure("NAMESPACE_PRESENCE_INVALID_RECEIPT"))
    assert.equal(calls, 1)
  }
})
