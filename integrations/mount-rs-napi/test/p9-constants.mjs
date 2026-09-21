import assert from "node:assert/strict"
import * as native from "@mount-rs/core/9p"
import { pathToFileURL } from "node:url"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API 9P constants parity: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const upstream = await import(pathToFileURL(source + "/src/9p/constants.ts").href)

for (const name of Object.keys(upstream)) {
  assert.ok(Object.hasOwn(native, name), "missing 9P public export " + name)
  if (name !== "messageName") {
    assert.deepEqual(native[name], upstream[name], "9P public export " + name)
  }
}

for (const type of [
  -1,
  upstream.P9_TVERSION,
  upstream.P9_RGETATTR,
  upstream.P9_RWSTAT,
  255,
]) {
  assert.equal(native.messageName(type), upstream.messageName(type), "messageName(" + type + ")")
}

console.log("mount-rs N-API 9P constants parity: PASS (" + Object.keys(upstream).length + " exports)")
