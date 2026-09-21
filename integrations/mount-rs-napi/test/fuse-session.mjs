import assert from "node:assert/strict"
import * as fuse from "../fuse.cjs"

function frame(opcode, nodeid, body, unique = 42n) {
  const payload = Buffer.from(body)
  return Buffer.concat([
    fuse.encodeInHeader({
      len: 40 + payload.length,
      opcode,
      unique,
      nodeid,
      uid: 0,
      gid: 0,
      pid: 0,
      totalExtlen: 0,
    }),
    payload,
  ])
}

function initBody() {
  const body = Buffer.alloc(20)
  body.writeUInt32LE(7, 0)
  body.writeUInt32LE(41, 4)
  body.writeUInt32LE(65_536, 8)
  body.writeUInt32LE(0, 12)
  body.writeUInt32LE(0, 16)
  return body
}

function lookupBody(name) {
  return Buffer.from(`${name}\0`)
}

assert.equal(typeof fuse.Filesystem, "function")
assert.equal(typeof fuse.FuseSession, "function")

const filesystem = fuse.Filesystem.memory()
await filesystem.writeFile("/visible", Buffer.from("session"))
const observedErrors = []
const session = new fuse.FuseSession(filesystem, {
  maxRequest: 65_536,
  useDriverIno: false,
  attrTimeout: 1.25,
  entryTimeout: 2.5,
  negativeTimeout: 3.75,
  keepCache: false,
  flushMechanism: "noflush",
  init: { maxWrite: 65_536, readdirplus: false },
  onError: (error, request) => observedErrors.push({ error, request }),
})

assert.equal(session.destroyed, false)
assert.equal(session.openHandles, 0)
assert.equal(session.negotiated, undefined)

const init = await session.handle(frame(26, 0n, initBody()))
assert.ok(init instanceof Buffer)
assert.equal(init.readInt32LE(4), 0)

assert.equal(session.negotiated.major, 7)
assert.equal(session.negotiated.minor, 41)
assert.ok(session.negotiated.maxWrite <= 65_456)
assert.equal(session.negotiated.readdirplus, false)

const lookup = await session.handle(frame(1, 1n, lookupBody("visible")))
assert.ok(lookup instanceof Buffer)
assert.equal(lookup.readInt32LE(4), 0)
const inode = lookup.readBigUInt64LE(16)
assert.notEqual(inode, 0n)

const missing = await session.handle(frame(1, 1n, lookupBody("missing"), 43n))
assert.ok(missing instanceof Buffer)
assert.equal(missing.readInt32LE(4), 0)
assert.equal(missing.readBigUInt64LE(16), 0n)
assert.equal(missing.readUInt32LE(48), 750_000_000)

const observed = await session.handle(frame(5, inode, Buffer.alloc(0), 44n))
assert.ok(observed instanceof Buffer)
assert.equal(observed.readInt32LE(4), -22)
assert.equal(observedErrors.at(-1).error.code, "EINVAL")
assert.equal(observedErrors.at(-1).error.errno, -22)
assert.equal(observedErrors.at(-1).request.header.opcode, 5)
await session.handle(frame(5, inode, Buffer.alloc(0), 45n))
assert.equal(observedErrors.at(-1).error.code, "EINVAL")

await session.destroy()
assert.equal(session.destroyed, true)
assert.equal(session.openHandles, 0)
const afterDestroy = await session.handle(frame(1, 1n, lookupBody("visible"), 46n))
assert.ok(afterDestroy instanceof Buffer)
assert.equal(afterDestroy.readInt32LE(4), -19)
await filesystem.shutdown()

console.log("mount-rs N-API Rust-backed FUSE session: PASS")
