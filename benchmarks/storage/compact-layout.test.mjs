import assert from "node:assert/strict"
import { test } from "node:test"

import { observeCompactLayout, validateCompactLayoutReceipt } from "./compact-layout.mjs"

const good = () => ({
  schema: "mount-rs.compact-layout-receipt.v1",
  marker: "MRC5",
  backingId: "1234567890abcdef1234567890abcdef",
  structuralGeneration: "18446744073709551615",
  blockAuthorityVerified: true,
})

test("validated receipt keeps precise generation and owns a frozen fixed copy", () => {
  const source = good()
  const result = validateCompactLayoutReceipt(source)
  assert.deepEqual(result, source)
  assert.notEqual(result, source)
  source.marker = "MRC4"
  assert.equal(result.marker, "MRC5")
  assert.throws(() => { result.structuralGeneration = "0" }, TypeError)
})

for (const value of [null, undefined, [], "MRC5", { ...good(), privatePath: "/private/secret" }]) {
  test("missing or extra receipt data cannot prove compact selection", () => {
    assert.throws(() => validateCompactLayoutReceipt(value), (error) => {
      assert.equal(error.code, value === null ? "COMPACT_LAYOUT_UNAVAILABLE" : "COMPACT_LAYOUT_INVALID")
      assert.equal(JSON.stringify(error).includes("private"), false)
      return true
    })
  })
}

for (const change of [
  { schema: "private schema" }, { marker: "MRC4" }, { backingId: "0".repeat(32) },
  { backingId: "AB".repeat(16) }, { backingId: "private identifier" },
  { structuralGeneration: 1 }, { structuralGeneration: "01" },
  { structuralGeneration: "-1" }, { structuralGeneration: "18446744073709551616" },
  { structuralGeneration: "9".repeat(2000) }, { blockAuthorityVerified: false },
]) {
  test("invalid marker, authority or scalar stays unavailable and redacted", () => {
    assert.throws(() => validateCompactLayoutReceipt({ ...good(), ...change }), (error) => {
      assert.equal(error.code, "COMPACT_LAYOUT_INVALID")
      assert.equal(error.message, "compact layout receipt is invalid")
      return true
    })
  })
}

test("receipt accessors are rejected before invoking their code", () => {
  const source = good()
  Object.defineProperty(source, "marker", { get: () => assert.fail("getter called") })
  assert.throws(() => validateCompactLayoutReceipt(source), { code: "COMPACT_LAYOUT_INVALID" })
})

test("throwing receipt descriptor traps cannot expose arbitrary query data", () => {
  const source = new Proxy(good(), { ownKeys() { throw new Error("private SQL and credential") } })
  assert.throws(() => validateCompactLayoutReceipt(source), (error) => {
    assert.equal(error.code, "COMPACT_LAYOUT_INVALID")
    assert.equal(error.message, "compact layout receipt is invalid")
    return true
  })
})

test("an inherited descriptor cannot replace a missing receipt field", () => {
  const source = good()
  delete source.schema
  source.unrecognized = true
  const prior = Object.getOwnPropertyDescriptor(Object.prototype, "schema")
  try {
    Object.defineProperty(Object.prototype, "schema", { value: { value: good().schema }, configurable: true })
    assert.throws(() => validateCompactLayoutReceipt(source), { code: "COMPACT_LAYOUT_INVALID" })
  } finally {
    if (prior) Object.defineProperty(Object.prototype, "schema", prior)
    else delete Object.prototype.schema
  }
})

test("query uses exactly the same filesystem receiver once", async () => {
  let calls = 0
  const filesystem = { async inspectCompactLayout() { assert.equal(this, filesystem); calls++; return good() } }
  assert.deepEqual(await observeCompactLayout(filesystem), good())
  assert.equal(calls, 1)
})

test("missing API and recognized noncompact cannot become a constructor proof", async () => {
  await assert.rejects(() => observeCompactLayout({}), { code: "COMPACT_LAYOUT_UNAVAILABLE" })
  await assert.rejects(() => observeCompactLayout({ async inspectCompactLayout() { return null } }), { code: "COMPACT_LAYOUT_UNAVAILABLE" })
})

test("arbitrary query failures expose only fixed diagnostic fields", async () => {
  const privateError = Object.assign(new Error("private SQL and credential"), { code: "private code", path: "/private/path" })
  for (const filesystem of [
    { async inspectCompactLayout() { throw privateError } },
    Object.defineProperty({}, "inspectCompactLayout", { get() { throw privateError } }),
  ]) {
    await assert.rejects(() => observeCompactLayout(filesystem), (error) => {
      assert.equal(error.code, "COMPACT_LAYOUT_QUERY_FAILED")
      assert.equal(error.message, "compact layout query failed")
      assert.equal(error.cause, undefined)
      assert.equal(error.path, undefined)
      return true
    })
  }
})
