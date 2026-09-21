import assert from "node:assert/strict"
import * as native from "@mount-rs/core/9p"
import { pathToFileURL } from "node:url"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API 9P constants parity: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const upstream = await import(pathToFileURL(source + "/src/9p/constants.ts").href)
const upstreamServer = await import(pathToFileURL(source + "/src/9p/server.ts").href)
const upstreamSession = await import(pathToFileURL(source + "/src/9p/session.ts").href)
const upstreamLocks = await import(pathToFileURL(source + "/src/9p/locks.ts").href)

for (const name of Object.keys(upstream)) {
  assert.ok(Object.hasOwn(native, name), "missing 9P public export " + name)
  if (name !== "messageName") {
    assert.deepEqual(native[name], upstream[name], "9P public export " + name)
  }
}

for (const [name, value] of [
  ["DEFAULT_P9_PORT", upstreamServer.DEFAULT_P9_PORT],
  ["DEFAULT_SOCKET_MODE", upstreamServer.DEFAULT_SOCKET_MODE],
  ["DEFAULT_MAX_IN_FLIGHT", upstreamServer.DEFAULT_MAX_IN_FLIGHT],
  ["DEFAULT_MSIZE", upstreamSession.DEFAULT_MSIZE],
  ["P9_LOCK_EOF_END", upstreamLocks.P9_LOCK_EOF_END],
  ["DEFAULT_MAX_LOCKS_PER_FILE", upstreamLocks.DEFAULT_MAX_LOCKS_PER_FILE],
]) {
  assert.ok(Object.hasOwn(native, name), "missing 9P public export " + name)
  assert.deepEqual(native[name], value, "9P public export " + name)
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
