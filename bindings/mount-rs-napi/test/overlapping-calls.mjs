// Calls that JavaScript makes before the previous call's promise settles.
//
// A FUSE session and a streamed response body each hold state that one call
// at a time may use. The native side fixes the position of a call when the
// call is made, so overlapping calls run in the order JavaScript made them:
// a request sees the effects of every request issued before it, the error of
// a request is reported for that request, consecutive pulls get consecutive
// chunks, and a close runs after the pulls issued before it and before the
// pulls issued after it.
import assert from "node:assert/strict"
import { createRequire } from "node:module"

import * as fuse from "../fuse.cjs"
import {
  Filesystem,
  createDriver,
  createMemoryDriver,
  createS3Server,
  createWebdavServer,
} from "../index.js"

const native = createRequire(import.meta.url)("../index.js")

function frame(opcode, nodeid, body, unique) {
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

const FUSE_LOOKUP = 1
const FUSE_GETATTR = 3
const FUSE_READLINK = 5
const FUSE_MKDIR = 9
const FUSE_RMDIR = 11
const FUSE_INIT = 26

function initBody() {
  const body = Buffer.alloc(20)
  body.writeUInt32LE(7, 0)
  body.writeUInt32LE(41, 4)
  body.writeUInt32LE(65_536, 8)
  return body
}

function name(value) {
  return Buffer.from(`${value}\0`)
}

function mkdirBody(value) {
  const head = Buffer.alloc(8)
  head.writeUInt32LE(0o40755, 0)
  return Buffer.concat([head, name(value)])
}

const errno = (reply) => reply.readInt32LE(4)

async function fuseOrder() {
  const filesystem = Filesystem.memory()
  const session = new native.FuseSession(filesystem, { negativeTimeout: 0 })
  assert.equal(errno(await session.handle(frame(FUSE_INIT, 0n, initBody(), 1n))), 0)

  // Sixteen calls at once. Each directory is made, found, removed and then
  // missed; any pair that ran out of order changes one of the four replies.
  let unique = 2n
  for (let round = 0; round < 300; round += 1) {
    const calls = []
    for (let slot = 0; slot < 4; slot += 1) {
      const directory = `d${round}-${slot}`
      calls.push(
        session.handle(frame(FUSE_MKDIR, 1n, mkdirBody(directory), unique++)),
        session.handle(frame(FUSE_LOOKUP, 1n, name(directory), unique++)),
        session.handle(frame(FUSE_RMDIR, 1n, name(directory), unique++)),
        session.handle(frame(FUSE_LOOKUP, 1n, name(directory), unique++)),
      )
    }
    const replies = await Promise.all(calls)
    assert.deepEqual(
      replies.map(errno),
      Array.from({ length: 4 }, () => [0, 0, 0, -2]).flat(),
      `round ${round}: overlapping requests ran out of call order`,
    )
  }

  // The reply header carries the request's own unique id, so a reply handed
  // to the wrong caller would show here.
  const uniques = Array.from({ length: 64 }, (_, index) => 10_000n + BigInt(index))
  const replies = await Promise.all(
    uniques.map((id) => session.handle(frame(FUSE_GETATTR, 1n, Buffer.alloc(16), id))),
  )
  assert.deepEqual(replies.map((reply) => reply.readBigUInt64LE(8)), uniques)

  // Requests issued before a destroy run before it; those after it see a
  // destroyed session.
  const before = session.handle(frame(FUSE_GETATTR, 1n, Buffer.alloc(16), unique++))
  const alsoBefore = session.handle(frame(FUSE_LOOKUP, 1n, name("absent"), unique++))
  const destroyed = session.destroy()
  const after = session.handle(frame(FUSE_GETATTR, 1n, Buffer.alloc(16), unique++))
  const state = session.__state()
  assert.equal(errno(await before), 0)
  assert.equal(errno(await alsoBefore), -2)
  await destroyed
  assert.equal(errno(await after), -19)
  assert.equal((await state).destroyed, true)
  await filesystem.shutdown()
}

async function fuseErrorAttribution() {
  const filesystem = Filesystem.memory()
  await filesystem.writeFile("/file", Buffer.from("not a link"))

  // The native observed form: the structured error belongs to the request
  // whose reply it arrives with.
  const session = new native.FuseSession(filesystem, { negativeTimeout: 0 })
  assert.equal(errno(await session.handle(frame(FUSE_INIT, 0n, initBody(), 1n))), 0)
  const found = await session.handle(frame(FUSE_LOOKUP, 1n, name("file"), 2n))
  assert.equal(errno(found), 0)
  const inode = found.readBigUInt64LE(16)
  const kinds = [
    { code: null, errno: 0, make: (id) => frame(FUSE_GETATTR, 1n, Buffer.alloc(16), id) },
    { code: "ENOENT", errno: -2, make: (id) => frame(FUSE_LOOKUP, 1n, name(`missing-${id}`), id) },
    { code: "EINVAL", errno: -22, make: (id) => frame(FUSE_READLINK, inode, Buffer.alloc(0), id) },
  ]
  for (let round = 0; round < 200; round += 1) {
    const issued = Array.from({ length: 16 }, (_, index) => {
      const kind = kinds[(index * 7 + round) % kinds.length]
      const id = BigInt(1_000 + round * 16 + index)
      return { kind, id, observed: session.__handleObserved(kind.make(id)) }
    })
    for (const { kind, id, observed } of issued) {
      const { reply, error } = await observed
      assert.equal(reply.readBigUInt64LE(8), id)
      assert.equal(errno(reply), kind.errno)
      assert.equal(error?.code ?? null, kind.code, `request ${id} was given another request's error`)
      if (error) assert.equal(error.errno, kind.errno)
    }
  }
  await session.destroy()

  // The public facade reports each error with the request that caused it,
  // in the order the requests were made.
  const reported = []
  const facade = new fuse.FuseSession(filesystem, {
    negativeTimeout: 0,
    onError: (error, request) => reported.push({ code: error.code, unique: request.header.unique }),
  })
  assert.equal(errno(await facade.handle(frame(FUSE_INIT, 0n, initBody(), 1n))), 0)
  const facadeInode = (await facade.handle(frame(FUSE_LOOKUP, 1n, name("file"), 2n))).readBigUInt64LE(16)
  const expected = []
  const calls = []
  for (let index = 0; index < 48; index += 1) {
    const id = BigInt(5_000 + index)
    if (index % 3 === 0) {
      calls.push(facade.handle(frame(FUSE_GETATTR, 1n, Buffer.alloc(16), id)))
    } else if (index % 3 === 1) {
      calls.push(facade.handle(frame(FUSE_LOOKUP, 1n, name(`gone-${index}`), id)))
      expected.push({ code: "ENOENT", unique: id })
    } else {
      calls.push(facade.handle(frame(FUSE_READLINK, facadeInode, Buffer.alloc(0), id)))
      expected.push({ code: "EINVAL", unique: id })
    }
  }
  const replies = await Promise.all(calls)
  assert.deepEqual(
    replies.map(errno),
    Array.from({ length: 48 }, (_, index) => [0, -2, -22][index % 3]),
  )
  assert.deepEqual(reported, expected)
  await facade.destroy()
  await filesystem.shutdown()
}

function patterned(length) {
  const bytes = Buffer.alloc(length)
  for (let offset = 0; offset + 4 <= length; offset += 4) bytes.writeUInt32LE(offset / 4, offset)
  return bytes
}

const object = patterned(1024 * 1024 + 13)

// Pull four chunks at a time until the body ends. The chunks, taken in the
// order the pulls were made, are the body: none swapped, none missing, and
// no pull reports the end while another pull still has bytes to deliver.
async function overlappingPulls(open, label) {
  const response = await open()
  assert.equal(response.status, 200, label)
  const iterator = response.body[Symbol.asyncIterator]()
  const chunks = []
  let ended = false
  while (!ended) {
    const pulls = await Promise.all([iterator.next(), iterator.next(), iterator.next(), iterator.next()])
    for (const pull of pulls) {
      if (pull.done) {
        ended = true
      } else {
        assert.equal(ended, false, `${label}: a chunk arrived after the end was reported`)
        chunks.push(Buffer.from(pull.value))
      }
    }
  }
  assert.ok(chunks.length > 8, `${label}: expected a body of many chunks, got ${chunks.length}`)
  assert.ok(Buffer.concat(chunks).equals(object), `${label}: overlapping pulls damaged the body`)
}

// Two pulls, a close and a third pull, all at once. The pulls made before
// the close deliver the first two chunks in order; the close then ends the
// body, and the pull made after it reports the end.
async function pullsAroundClose(open, label) {
  const response = await open()
  assert.equal(response.status, 200, label)
  const iterator = response.body[Symbol.asyncIterator]()
  const first = iterator.next()
  const second = iterator.next()
  const closed = iterator.return()
  const third = iterator.next()
  const [a, b, c, d] = await Promise.all([first, second, closed, third])
  assert.equal(a.done, false, label)
  assert.equal(b.done, false, label)
  const head = Buffer.concat([Buffer.from(a.value), Buffer.from(b.value)])
  assert.ok(head.length > 0)
  assert.ok(head.equals(object.subarray(0, head.length)), `${label}: pulls before a close were reordered`)
  assert.equal(c.done, true, label)
  assert.equal(d.done, true, label)
  assert.equal((await iterator.next()).done, true, label)
}

async function s3Bodies() {
  const bucket = Filesystem.memory()
  await bucket.writeFile("/big.bin", object)
  const server = createS3Server({ buckets: { bucket } }, { readChunkBytes: 4096 })
  const open = () =>
    server.session.handleRequestStream({ method: "GET", target: "/bucket/big.bin", headers: [] }, null)
  for (let round = 0; round < 5; round += 1) {
    await overlappingPulls(open, "S3")
    await pullsAroundClose(open, "S3")
  }
  await bucket.shutdown()
}

async function webdavBodies() {
  const filesystem = Filesystem.memory()
  await filesystem.writeFile("/big.bin", object)
  const server = createWebdavServer(filesystem, { host: "127.0.0.1", port: 0, readChunkBytes: 4096 })
  const open = () =>
    server.session.handleRequestStream({ method: "GET", target: "/big.bin", headers: [] }, null)
  for (let round = 0; round < 5; round += 1) {
    await overlappingPulls(open, "WebDAV")
    await pullsAroundClose(open, "WebDAV")
  }
  await filesystem.shutdown()
}

// A driver whose reads come up short and whose handles record what happens
// to them. A position that advanced by the requested length rather than the
// length read would skip bytes; a close that overtook a pull, or a pull that
// outlived the close, shows as a read on a closed handle; a body that
// outlived its close shows as a handle never closed.
async function webdavShortReads() {
  const backing = createMemoryDriver()
  const seed = await backing.open("/big.bin", "w+", 0o644)
  await seed.write(object, 0, object.length, 0)
  await seed.close()

  const handles = []
  const driver = {
    capabilities: backing.capabilities,
    open: async (path, flags, mode) => {
      const inner = await backing.open(path, flags, mode)
      if (path !== "/big.bin") return inner
      const record = { reads: 0, closes: 0, readAfterClose: false, overlapped: false, reading: false }
      handles.push(record)
      return {
        get fd() {
          return inner.fd
        },
        stat: (...args) => inner.stat(...args),
        sync: (...args) => inner.sync(...args),
        datasync: (...args) => inner.datasync(...args),
        truncate: (...args) => inner.truncate(...args),
        write: (...args) => inner.write(...args),
        read: async (buffer, offset, length, position) => {
          if (record.closes > 0) record.readAfterClose = true
          if (record.reading) record.overlapped = true
          record.reading = true
          record.reads += 1
          await new Promise((resolve) => setImmediate(resolve))
          const short = Math.max(1, Math.min(length, 700 + (record.reads % 5) * 611))
          const result = await inner.read(buffer, offset, short, position)
          record.reading = false
          return result
        },
        close: async () => {
          if (record.reading) record.overlapped = true
          record.closes += 1
          return inner.close()
        },
      }
    },
  }
  for (const method of [
    "stat", "readdir", "readdirBounded", "mkdir", "rmdir", "unlink", "rename", "link", "symlink",
    "readlink", "chmod", "chown", "lchown", "truncate", "utimes", "lutimes",
  ]) {
    if (typeof backing[method] === "function") driver[method] = backing[method].bind(backing)
  }
  const filesystem = createDriver(driver)
  const server = createWebdavServer(filesystem, { host: "127.0.0.1", port: 0, readChunkBytes: 4096 })
  const open = () =>
    server.session.handleRequestStream({ method: "GET", target: "/big.bin", headers: [] }, null)

  await overlappingPulls(open, "WebDAV short reads")
  for (let round = 0; round < 20; round += 1) {
    await pullsAroundClose(open, "WebDAV short reads")
  }
  // A close is the last thing done to a body handle; give a late one a turn.
  await new Promise((resolve) => setTimeout(resolve, 50))
  assert.ok(handles.length >= 21)
  for (const [index, record] of handles.entries()) {
    assert.equal(record.readAfterClose, false, `handle ${index}: a pull ran after the close`)
    assert.equal(record.overlapped, false, `handle ${index}: two calls used the handle at once`)
    assert.equal(record.closes, 1, `handle ${index}: closed ${record.closes} times`)
  }
  // The pulls made before each close both ran: two reads, then the close.
  for (const record of handles.slice(1)) assert.equal(record.reads, 2)
  await filesystem.shutdown()
}

await fuseOrder()
await fuseErrorAttribution()
await s3Bodies()
await webdavBodies()
await webdavShortReads()

console.log("mount-rs N-API overlapping calls: PASS")
