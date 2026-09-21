import assert from "node:assert/strict"
import { Duplex } from "node:stream"

import root from "../index.js"
import p9 from "../p9.cjs"

const { Filesystem, createP9Server } = root
const {
  FidTable,
  FIRST_QID_PATH,
  P9_QTDIR,
  P9_QTFILE,
  P9_QTSYMLINK,
  encodeMessage,
  qidType,
  qidVersion,
  walkStep,
} = p9

const regularStats = (overrides = {}) => ({
  dev: 7,
  ino: 0,
  mode: 0o100644,
  mtimeMs: 0,
  ...overrides,
})

const table = new FidTable()
assert.equal(table.size, 0)
assert.equal(table.qidPathCount, 0)
assert.equal(table.get(9), undefined)

const source = table.create(1, "/a/./b/../c/")
assert.equal(source.path, "/a/c")
assert.equal(source.open, undefined)
assert.equal(source.iounit, 0)
assert.equal(source.cursor, undefined)
assert.deepEqual(table.fids(), [1])
assert.deepEqual(table.entries().map((entry) => entry.fid), [1])

assert.throws(() => table.create(1, "/other"), /already in use/)
assert.throws(() => table.create(0xffff_ffff, "/"), /P9_NOFID/)
assert.throws(() => table.require(9), /EBADF/)

const clone = table.clone(1, 2)
const listing = ["a", "b", "c"]
assert.equal(clone.path, "/a/c")
assert.equal(clone.open, undefined)
assert.equal(clone.iounit, 0)
assert.equal(clone.cursor, undefined)
assert.equal(table.clone(1, 1).fid, 1)
assert.throws(() => table.clone(1, 2), /already in use/)

source.open = { flags: 0, directory: false }
source.iounit = 8192
source.cursor = { entries: listing, offsets: [{ offset: 1n, index: 1 }] }
assert.equal(source.open.directory, false)
assert.equal(source.iounit, 8192)
assert.deepEqual(source.cursor.entries, listing)
assert.deepEqual(source.cursor.offsets, [{ offset: 1n, index: 1 }])
const openClone = table.clone(1, 6)
assert.equal(openClone.open, undefined)
assert.equal(openClone.iounit, 0)
assert.equal(openClone.cursor, undefined)
source.open = undefined
source.iounit = 0
source.cursor = undefined
assert.equal(source.open, undefined)

assert.equal(table.resume(source, 0n), undefined)
assert.deepEqual(table.snapshot(source, listing), { entries: listing, index: 0 })
table.noteOffset(source, 1n, 1)
table.noteOffset(source, 2n, 2)
assert.deepEqual(table.resume(source, 2n), { entries: listing, index: 2 })
assert.throws(() => table.resume(source, 99n), /never given readdir offset 99/)
source.path = "/moved"
assert.equal(source.cursor, undefined)
assert.throws(() => table.resume(source, 2n), /never given readdir offset 2/)

const at = table.create(3, "/src")
const below = table.create(4, "/src/child")
const sibling = table.create(5, "/src-other")
table.remap("/src", "/dest")
assert.equal(at.path, "/dest")
assert.equal(below.path, "/dest/child")
assert.equal(sibling.path, "/src-other")

assert.equal(qidType(0o040755), P9_QTDIR)
assert.equal(qidType(0o120777), P9_QTSYMLINK)
assert.equal(qidType(0o100644), P9_QTFILE)
assert.equal(qidVersion(regularStats({ mtimeMs: 1_700_000_000_123 })), (1_700_000_000_123 >>> 0))
assert.equal(qidVersion(regularStats({ mtimeMs: Number.NaN })), 0)

const firstQid = table.qidFor(regularStats({ ino: 42, mode: 0o040755, mtimeMs: 5 }), "/qid")
assert.deepEqual(firstQid, { type: P9_QTDIR, version: 5, path: FIRST_QID_PATH })
assert.equal(table.qidPathFor(regularStats({ ino: 42 }), "/hard-link"), FIRST_QID_PATH)
const hugeQid = table.qidPathFor(regularStats({ ino: 2 ** 63 }), "/huge")
const plainQid = table.qidPathFor(regularStats(), "/plain")
assert.notEqual(hugeQid, plainQid)
const perPath = new FidTable({ useDriverIno: false })
assert.equal(perPath.qidPathFor(regularStats({ ino: 42 }), "/one"), FIRST_QID_PATH)
assert.equal(perPath.qidPathFor(regularStats({ ino: 43 }), "/one"), FIRST_QID_PATH)
assert.equal(perPath.qidPathFor(regularStats({ ino: 42 }), "/two"), FIRST_QID_PATH + 1n)
table.release("/qid")
table.release("/hard-link")
assert.notEqual(table.qidPathFor(regularStats({ ino: 42 }), "/qid"), FIRST_QID_PATH)

table.clunk(1)
assert.equal(table.get(1), undefined)
assert.throws(() => table.clunk(1), /EBADF/)
table.clear()
assert.equal(table.size, 0)
assert.equal(table.qidPathCount, 0)

assert.equal(walkStep("/", "a"), "/a")
assert.equal(walkStep("/a/b", ".."), "/a")
assert.equal(walkStep("/", "."), "/")
assert.throws(() => walkStep("/a", ""), /EINVAL/)
assert.throws(() => walkStep("/a", "bad/name"), /separator/)
assert.throws(() => walkStep("/a", "bad\0name"), /NUL/)

function waitUntil(predicate, label) {
  const deadline = Date.now() + 5_000
  return new Promise((resolve, reject) => {
    const poll = () => {
      if (predicate()) return resolve()
      if (Date.now() >= deadline) return reject(new Error(`${label} timed out`))
      setImmediate(poll)
    }
    poll()
  })
}

const stream = new Duplex({
  read() {},
  write(_chunk, _encoding, callback) {
    callback()
  },
})
const server = createP9Server(Filesystem.memory(), { readOnly: true })
const connection = server.attach(stream, { own: false, peer: "fid-test" })

try {
  const sessionFids = connection.session.fids

  stream.push(encodeMessage(100, 1, (writer) => {
    writer.u32(32 * 1024)
    writer.string("9P2000.L")
  }))
  await waitUntil(() => connection.session.stats.replies >= 1, "fid version reply")

  stream.push(encodeMessage(104, 2, (writer) => {
    writer.u32(1)
    writer.u32(0xffff_ffff)
    writer.string("node")
    writer.string("")
    writer.u32(0xffff_ffff)
  }))
  await waitUntil(() => connection.session.stats.replies >= 2, "fid attach reply")
  assert.deepEqual(sessionFids.fids(), [1])
  assert.equal(sessionFids.get(1).path, "/")
  assert.equal(sessionFids.get(1).open, undefined)

  stream.push(encodeMessage(12, 3, (writer) => {
    writer.u32(1)
    writer.u32(0)
  }))
  await waitUntil(() => connection.session.stats.replies >= 3, "fid lopen reply")

  const opened = sessionFids.get(1)
  assert.ok(opened.open)
  assert.equal(opened.open.directory, true)
  assert.equal(sessionFids.openHandles().length, opened.open.handle ? 1 : 0)
  if (opened.open.handle) {
    assert.equal(sessionFids.openHandles()[0].fid.fid, 1)
    assert.ok(sessionFids.openHandles()[0].handle)
  }
} finally {
  await connection.close()
  await server.close()
  stream.destroy()
}

console.log("mount-rs N-API P9 fid surface: PASS")
