import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { pathToFileURL } from "node:url"
import {
  P9Error as esmP9Error,
  isP9Error as esmIsP9Error,
  P9Reader as esmP9Reader,
  P9Writer as esmP9Writer,
  P9FrameAssembler as esmP9FrameAssembler,
  P9DirentPacker as esmP9DirentPacker,
  P9_QID_SIZE as esmP9QidSize,
  P9_MAX_STRING as esmP9MaxString,
  P9_MAX_ITEM as esmP9MaxItem,
  P9_DEFAULT_MAX_FRAME as esmP9DefaultMaxFrame,
  encodeP9 as esmEncodeP9,
  decodeP9 as esmDecodeP9,
  readHeader as esmReadHeader,
  writeHeader as esmWriteHeader,
  readTime as esmReadTime,
  writeTime as esmWriteTime,
  encodeMessage as esmEncodeMessage,
  decodeMessage as esmDecodeMessage,
  decodeMessageAs as esmDecodeMessageAs,
  readEmptyBody as esmReadEmptyBody,
  EMPTY_BODY as esmEmptyBody,
  stringByteLength as esmStringByteLength,
  direntSize as esmDirentSize,
  readDirents as esmReadDirents,
  framesFrom as esmFramesFrom,
  writeTversion as esmWriteTversion,
  readTversion as esmReadTversion,
  writeRversion as esmWriteRversion,
  readRversion as esmReadRversion,
  writeTauth as esmWriteTauth,
  readTauth as esmReadTauth,
  writeRauth as esmWriteRauth,
  readRauth as esmReadRauth,
  writeTattach as esmWriteTattach,
  readTattach as esmReadTattach,
  writeRattach as esmWriteRattach,
  readRattach as esmReadRattach,
  writeRlerror as esmWriteRlerror,
  readRlerror as esmReadRlerror,
  writeTflush as esmWriteTflush,
  readTflush as esmReadTflush,
  writeTwalk as esmWriteTwalk,
  readTwalk as esmReadTwalk,
  writeRwalk as esmWriteRwalk,
  readRwalk as esmReadRwalk,
  writeTread as esmWriteTread,
  readTread as esmReadTread,
  writeRread as esmWriteRread,
  readRread as esmReadRread,
  writeTwrite as esmWriteTwrite,
  readTwrite as esmReadTwrite,
  writeRwrite as esmWriteRwrite,
  readRwrite as esmReadRwrite,
  writeFidRequest as esmWriteFidRequest,
  readFidRequest as esmReadFidRequest,
  writeRstatfs as esmWriteRstatfs,
  readRstatfs as esmReadRstatfs,
  writeTlopen as esmWriteTlopen,
  readTlopen as esmReadTlopen,
  writeRlopen as esmWriteRlopen,
  readRlopen as esmReadRlopen,
  writeTlcreate as esmWriteTlcreate,
  readTlcreate as esmReadTlcreate,
  writeTsymlink as esmWriteTsymlink,
  readTsymlink as esmReadTsymlink,
  writeQidReply as esmWriteQidReply,
  readQidReply as esmReadQidReply,
  writeTmknod as esmWriteTmknod,
  readTmknod as esmReadTmknod,
  writeTmkdir as esmWriteTmkdir,
  readTmkdir as esmReadTmkdir,
  writeTrename as esmWriteTrename,
  readTrename as esmReadTrename,
  writeTrenameat as esmWriteTrenameat,
  readTrenameat as esmReadTrenameat,
  writeTunlinkat as esmWriteTunlinkat,
  readTunlinkat as esmReadTunlinkat,
  writeTlink as esmWriteTlink,
  readTlink as esmReadTlink,
  writeRreadlink as esmWriteRreadlink,
  readRreadlink as esmReadRreadlink,
  writeTgetattr as esmWriteTgetattr,
  readTgetattr as esmReadTgetattr,
  writeRgetattr as esmWriteRgetattr,
  readRgetattr as esmReadRgetattr,
  writeTsetattr as esmWriteTsetattr,
  readTsetattr as esmReadTsetattr,
  writeTxattrwalk as esmWriteTxattrwalk,
  readTxattrwalk as esmReadTxattrwalk,
  writeRxattrwalk as esmWriteRxattrwalk,
  readRxattrwalk as esmReadRxattrwalk,
  writeTxattrcreate as esmWriteTxattrcreate,
  readTxattrcreate as esmReadTxattrcreate,
  writeTreaddir as esmWriteTreaddir,
  readTreaddir as esmReadTreaddir,
  writeRreaddir as esmWriteRreaddir,
  readRreaddir as esmReadRreaddir,
  writeDirent as esmWriteDirent,
  readDirent as esmReadDirent,
  writeTfsync as esmWriteTfsync,
  readTfsync as esmReadTfsync,
  writeTlock as esmWriteTlock,
  readTlock as esmReadTlock,
  writeRlock as esmWriteRlock,
  readRlock as esmReadRlock,
  writeTgetlock as esmWriteTgetlock,
  readTgetlock as esmReadTgetlock,
  writeRgetlock as esmWriteRgetlock,
  readRgetlock as esmReadRgetlock,
  writeTclunk as esmWriteTclunk,
  readTclunk as esmReadTclunk,
  writeTremove as esmWriteTremove,
  readTremove as esmReadTremove,
  writeTstatfs as esmWriteTstatfs,
  readTstatfs as esmReadTstatfs,
  writeTreadlink as esmWriteTreadlink,
  readTreadlink as esmReadTreadlink,
  writeRsymlink as esmWriteRsymlink,
  readRsymlink as esmReadRsymlink,
  writeRmknod as esmWriteRmknod,
  readRmknod as esmReadRmknod,
  writeRmkdir as esmWriteRmkdir,
  readRmkdir as esmReadRmkdir,
  writeRlcreate as esmWriteRlcreate,
  readRlcreate as esmReadRlcreate,
} from "@mount-rs/core/9p"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API 9P codec differential: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const require = createRequire(import.meta.url)
const root = require("@mount-rs/core")
const rootKeys = Object.keys(root)
const native = require("@mount-rs/core/9p")
assert.deepEqual(Object.keys(root), rootKeys, "9P facade must not mutate root exports")
assert.equal(native.createP9Server, root.createP9Server)

// Node's CJS-to-ESM named-export bridge is syntax-discovered. Keep this list
// exhaustive so adding a codec alias without a direct `module.exports.name =`
// assignment in p9.cjs fails at the public package boundary.
const esmCodecExports = [
  ["P9Error", esmP9Error], ["isP9Error", esmIsP9Error], ["P9Reader", esmP9Reader],
  ["P9Writer", esmP9Writer], ["P9FrameAssembler", esmP9FrameAssembler],
  ["P9DirentPacker", esmP9DirentPacker], ["P9_QID_SIZE", esmP9QidSize],
  ["P9_MAX_STRING", esmP9MaxString], ["P9_MAX_ITEM", esmP9MaxItem],
  ["P9_DEFAULT_MAX_FRAME", esmP9DefaultMaxFrame], ["encodeP9", esmEncodeP9],
  ["decodeP9", esmDecodeP9], ["readHeader", esmReadHeader], ["writeHeader", esmWriteHeader],
  ["readTime", esmReadTime], ["writeTime", esmWriteTime],
  ["encodeMessage", esmEncodeMessage], ["decodeMessage", esmDecodeMessage],
  ["decodeMessageAs", esmDecodeMessageAs], ["readEmptyBody", esmReadEmptyBody],
  ["EMPTY_BODY", esmEmptyBody], ["stringByteLength", esmStringByteLength],
  ["direntSize", esmDirentSize], ["readDirents", esmReadDirents], ["framesFrom", esmFramesFrom],
  ["writeTversion", esmWriteTversion], ["readTversion", esmReadTversion],
  ["writeRversion", esmWriteRversion], ["readRversion", esmReadRversion],
  ["writeTauth", esmWriteTauth], ["readTauth", esmReadTauth],
  ["writeRauth", esmWriteRauth], ["readRauth", esmReadRauth],
  ["writeTattach", esmWriteTattach], ["readTattach", esmReadTattach],
  ["writeRattach", esmWriteRattach], ["readRattach", esmReadRattach],
  ["writeRlerror", esmWriteRlerror], ["readRlerror", esmReadRlerror],
  ["writeTflush", esmWriteTflush], ["readTflush", esmReadTflush],
  ["writeTwalk", esmWriteTwalk], ["readTwalk", esmReadTwalk],
  ["writeRwalk", esmWriteRwalk], ["readRwalk", esmReadRwalk],
  ["writeTread", esmWriteTread], ["readTread", esmReadTread],
  ["writeRread", esmWriteRread], ["readRread", esmReadRread],
  ["writeTwrite", esmWriteTwrite], ["readTwrite", esmReadTwrite],
  ["writeRwrite", esmWriteRwrite], ["readRwrite", esmReadRwrite],
  ["writeFidRequest", esmWriteFidRequest], ["readFidRequest", esmReadFidRequest],
  ["writeRstatfs", esmWriteRstatfs], ["readRstatfs", esmReadRstatfs],
  ["writeTlopen", esmWriteTlopen], ["readTlopen", esmReadTlopen],
  ["writeRlopen", esmWriteRlopen], ["readRlopen", esmReadRlopen],
  ["writeTlcreate", esmWriteTlcreate], ["readTlcreate", esmReadTlcreate],
  ["writeTsymlink", esmWriteTsymlink], ["readTsymlink", esmReadTsymlink],
  ["writeQidReply", esmWriteQidReply], ["readQidReply", esmReadQidReply],
  ["writeTmknod", esmWriteTmknod], ["readTmknod", esmReadTmknod],
  ["writeTmkdir", esmWriteTmkdir], ["readTmkdir", esmReadTmkdir],
  ["writeTrename", esmWriteTrename], ["readTrename", esmReadTrename],
  ["writeTrenameat", esmWriteTrenameat], ["readTrenameat", esmReadTrenameat],
  ["writeTunlinkat", esmWriteTunlinkat], ["readTunlinkat", esmReadTunlinkat],
  ["writeTlink", esmWriteTlink], ["readTlink", esmReadTlink],
  ["writeRreadlink", esmWriteRreadlink], ["readRreadlink", esmReadRreadlink],
  ["writeTgetattr", esmWriteTgetattr], ["readTgetattr", esmReadTgetattr],
  ["writeRgetattr", esmWriteRgetattr], ["readRgetattr", esmReadRgetattr],
  ["writeTsetattr", esmWriteTsetattr], ["readTsetattr", esmReadTsetattr],
  ["writeTxattrwalk", esmWriteTxattrwalk], ["readTxattrwalk", esmReadTxattrwalk],
  ["writeRxattrwalk", esmWriteRxattrwalk], ["readRxattrwalk", esmReadRxattrwalk],
  ["writeTxattrcreate", esmWriteTxattrcreate], ["readTxattrcreate", esmReadTxattrcreate],
  ["writeTreaddir", esmWriteTreaddir], ["readTreaddir", esmReadTreaddir],
  ["writeRreaddir", esmWriteRreaddir], ["readRreaddir", esmReadRreaddir],
  ["writeDirent", esmWriteDirent], ["readDirent", esmReadDirent],
  ["writeTfsync", esmWriteTfsync], ["readTfsync", esmReadTfsync],
  ["writeTlock", esmWriteTlock], ["readTlock", esmReadTlock],
  ["writeRlock", esmWriteRlock], ["readRlock", esmReadRlock],
  ["writeTgetlock", esmWriteTgetlock], ["readTgetlock", esmReadTgetlock],
  ["writeRgetlock", esmWriteRgetlock], ["readRgetlock", esmReadRgetlock],
  ["writeTclunk", esmWriteTclunk], ["readTclunk", esmReadTclunk],
  ["writeTremove", esmWriteTremove], ["readTremove", esmReadTremove],
  ["writeTstatfs", esmWriteTstatfs], ["readTstatfs", esmReadTstatfs],
  ["writeTreadlink", esmWriteTreadlink], ["readTreadlink", esmReadTreadlink],
  ["writeRsymlink", esmWriteRsymlink], ["readRsymlink", esmReadRsymlink],
  ["writeRmknod", esmWriteRmknod], ["readRmknod", esmReadRmknod],
  ["writeRmkdir", esmWriteRmkdir], ["readRmkdir", esmReadRmkdir],
  ["writeRlcreate", esmWriteRlcreate], ["readRlcreate", esmReadRlcreate],
]
for (const [name, value] of esmCodecExports) {
  assert.equal(value, native[name], `ESM named export ${name}`)
}

const [upstreamWire, upstreamProtocol, upstreamConstants] = await Promise.all([
  import(pathToFileURL(`${source}/src/9p/wire.ts`).href),
  import(pathToFileURL(`${source}/src/9p/protocol.ts`).href),
  import(pathToFileURL(`${source}/src/9p/constants.ts`).href),
])

const bytes = (value) => Array.from(value)

function plain(value) {
  if (ArrayBuffer.isView(value)) return bytes(value)
  if (Array.isArray(value)) return value.map(plain)
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, plain(entry)]))
  }
  return value
}

function assertBytes(actual, expected, message) {
  assert.deepEqual(bytes(actual), bytes(expected), message)
}

function errorShape(api, operation, errorApi = api) {
  try {
    operation()
    assert.fail("expected an error")
  } catch (error) {
    return {
      isP9Error: errorApi.isP9Error(error),
      name: error.name,
      code: error.code,
      offset: error.offset,
      message: error.message,
    }
  }
}

const qid = { type: 1, version: 0x10203040, path: 0x1122334455667788n }
const time = { sec: 0x0102030405060708n, nsec: 0x8877665544332211n }

function primitiveSequence(api) {
  return api.encodeP9((writer) => {
    writer.u8(0xaa)
    writer.u16(0x1122)
    writer.u32(0x33445566)
    writer.u64(0x778899aabbccddeen)
    writer.string("café")
    writer.qid(qid)
    writer.blob(Uint8Array.of(0, 1, 2, 255))
    writer.raw(Uint8Array.of(9, 8, 7))
  }, 1)
}

const upstreamPrimitive = primitiveSequence(upstreamWire)
const nativePrimitive = primitiveSequence(native)
assertBytes(nativePrimitive, upstreamPrimitive, "9P primitive bytes")

const upstreamPrimitiveDecoded = upstreamWire.decodeP9(upstreamPrimitive, (reader) => ({
  u8: reader.u8(),
  u16: reader.u16(),
  u32: reader.u32(),
  u64: reader.u64(),
  string: reader.string(),
  qid: reader.qid(),
  blob: reader.blob(),
  raw: reader.rest(),
}))
const nativePrimitiveDecoded = native.decodeP9(nativePrimitive, (reader) => ({
  u8: reader.u8(),
  u16: reader.u16(),
  u32: reader.u32(),
  u64: reader.u64(),
  string: reader.string(),
  qid: reader.qid(),
  blob: reader.blob(),
  raw: reader.rest(),
}))
assert.deepEqual(plain(nativePrimitiveDecoded), plain(upstreamPrimitiveDecoded), "9P primitive decode")

for (const value of ["", "ascii", "café", "𐍈"]) {
  assert.equal(native.stringByteLength(value), upstreamWire.stringByteLength(value), `UTF-8 length ${value}`)
}

// P9 blobs carry a little-endian u32 byte length.  Keep the payload bytes
// after the complete four-byte prefix; putting payload bytes in that prefix
// makes the native reader correctly interpret 0x02010004 and reject it as an
// oversized item.
const copyInput = Buffer.from([4, 0, 0, 0, 0, 1, 2, 3])
const copyReader = new native.P9Reader(copyInput)
const copied = copyReader.blob()
copyInput[4] = 99
assertBytes(copied, [0, 1, 2, 3], "9P blob output is copied")

for (const operation of [
  (api) => new api.P9Reader(Buffer.from([1, 2, 3])).u32("word"),
  (api) => new api.P9Reader(Buffer.from([0xff, 0xff, 0, 0])).blob(),
  (api) => api.decodeP9(Buffer.from([1]), (reader) => reader.u8(), "whole"),
  (api) => api.encodeP9((writer) => writer.string("x".repeat(65_536))),
]) {
  assert.deepEqual(errorShape(native, () => operation(native)), errorShape(upstreamWire, () => operation(upstreamWire)), "9P wire error shape")
}

const versionType = upstreamConstants.P9_TVERSION
const flushType = upstreamConstants.P9_RFLUSH
const versionFrame = native.encodeMessage(versionType, upstreamConstants.P9_NOTAG, (writer) => {
  writer.writeTversion({ msize: 65_536, version: "9P2000.L" })
})
const flushFrame = upstreamProtocol.encodeMessage(flushType, 7)
assert.equal(versionFrame.readUInt32LE(0), versionFrame.length, "9P frame size is backfilled")
assertBytes(
  native.decodeMessage(versionFrame).body.rest(),
  upstreamProtocol.decodeMessage(versionFrame).body.rest(),
  "9P frame body decode",
)

const stream = Buffer.concat([versionFrame, flushFrame])
const upstreamAssembler = new upstreamProtocol.P9FrameAssembler()
const nativeAssembler = new native.P9FrameAssembler()
const expectedFrames = []
const actualFrames = []
for (let offset = 0; offset < stream.length; offset += 3) {
  const chunk = stream.subarray(offset, offset + 3)
  expectedFrames.push(...upstreamAssembler.push(chunk))
  actualFrames.push(...nativeAssembler.push(chunk))
  assert.equal(nativeAssembler.pending, upstreamAssembler.pending, "9P pending bytes")
}
assert.deepEqual(actualFrames.map(bytes), expectedFrames.map(bytes), "9P split/coalesced frames")
assert.equal(nativeAssembler.failed, false)

async function* splitFrames() {
  for (let offset = 0; offset < stream.length; offset += 3) {
    yield stream.subarray(offset, offset + 3)
  }
}

const upstreamStreamedFrames = []
for await (const frame of upstreamProtocol.framesFrom(splitFrames())) {
  upstreamStreamedFrames.push(frame)
}
const nativeStreamedFrames = []
for await (const frame of native.framesFrom(splitFrames())) {
  nativeStreamedFrames.push(frame)
}
assert.deepEqual(nativeStreamedFrames.map(bytes), upstreamStreamedFrames.map(bytes), "9P async framesFrom")

const upstreamSharedAssembler = new upstreamProtocol.P9FrameAssembler()
const nativeSharedAssembler = new native.P9FrameAssembler()
const upstreamSharedFrames = []
for await (const frame of upstreamProtocol.framesFrom([versionFrame, flushFrame], upstreamSharedAssembler)) {
  upstreamSharedFrames.push(frame)
}
const nativeSharedFrames = []
for await (const frame of native.framesFrom([versionFrame, flushFrame], nativeSharedAssembler)) {
  nativeSharedFrames.push(frame)
}
assert.deepEqual(nativeSharedFrames.map(bytes), upstreamSharedFrames.map(bytes), "9P sync framesFrom")
assert.equal(nativeSharedAssembler.pending, upstreamSharedAssembler.pending, "9P shared assembler pending")

for (const operation of [
  (api) => new api.P9FrameAssembler(6),
  (api) => {
    const assembler = new api.P9FrameAssembler()
    assembler.push(Buffer.from([6, 0, 0, 0, 1, 0, 0]))
  },
]) {
  assert.deepEqual(
    errorShape(native, () => operation(native)),
    errorShape(upstreamProtocol, () => operation(upstreamProtocol), upstreamWire),
    "9P frame error shape",
  )
}

const cases = [
  ["Tversion", { msize: 65_536, version: "9P2000.L" }],
  ["Tauth", { afid: 9, uname: "alice", aname: "/srv", nUname: 1000 }],
  ["Rauth", { aqid: qid }],
  ["Tattach", { fid: 1, afid: 0xffff_ffff, uname: "alice", aname: "/", nUname: 1000 }],
  ["Rattach", { qid }],
  ["Rlerror", { ecode: 5 }],
  ["Tflush", { oldtag: 19 }],
  ["Twalk", { fid: 1, newfid: 2, wnames: ["a", "b", "café"] }],
  ["Rwalk", { wqids: [qid, { ...qid, path: 9n }] }],
  ["Tread", { fid: 3, offset: 0x1_0000_0000n, count: 4096 }],
  ["Rread", { data: Uint8Array.of(1, 2, 3, 4) }],
  ["Twrite", { fid: 3, offset: 0x2_0000_0000n, data: Uint8Array.of(8, 7, 6) }],
  ["Rwrite", { count: 3 }],
  ["Tclunk", { fid: 3 }],
  ["Tstatfs", { fid: 3 }],
  ["Rstatfs", { type: 1, bsize: 4096, blocks: 2n, bfree: 3n, bavail: 4n, files: 5n, ffree: 6n, fsid: 7n, namelen: 255 }],
  ["Tlopen", { fid: 3, flags: 0x1234 }],
  ["Rlopen", { qid, iounit: 65_536 }],
  ["Tlcreate", { fid: 3, name: "new", flags: 1, mode: 0o644, gid: 1000 }],
  ["Tsymlink", { dfid: 3, name: "link", symtgt: "/target", gid: 1000 }],
  ["Rsymlink", { qid }],
  ["Tmknod", { dfid: 3, name: "node", mode: 0o600, major: 1, minor: 2, gid: 1000 }],
  ["Rmknod", { qid }],
  ["Tmkdir", { dfid: 3, name: "dir", mode: 0o755, gid: 1000 }],
  ["Rmkdir", { qid }],
  ["Trename", { fid: 4, dfid: 3, name: "renamed" }],
  ["Trenameat", { olddirfid: 3, oldname: "old", newdirfid: 4, newname: "new" }],
  ["Tunlinkat", { dirfid: 3, name: "gone", flags: 0x200 }],
  ["Tlink", { dfid: 3, fid: 4, name: "hard" }],
  ["Treadlink", { fid: 4 }],
  ["Rreadlink", { target: "/target" }],
  ["Tgetattr", { fid: 3, requestMask: 0x1_0000_0000n }],
  ["Rgetattr", {
    valid: 0x1_0000_0000n, qid, mode: 0o644, uid: 1000, gid: 1001, nlink: 2n, rdev: 0n,
    size: 17n, blksize: 4096n, blocks: 1n, atime: time, mtime: time, ctime: time, btime: time,
    gen: 8n, dataVersion: 9n,
  }],
  ["Tsetattr", { fid: 3, valid: 1, mode: 0o600, uid: 1000, gid: 1001, size: 18n, atime: time, mtime: time }],
  ["Txattrwalk", { fid: 3, newfid: 5, name: "user.test" }],
  ["Rxattrwalk", { size: 32n }],
  ["Txattrcreate", { fid: 5, name: "user.test", attrSize: 32n, flags: 0 }],
  ["Treaddir", { fid: 3, offset: 0x1_0000_0000n, count: 4096 }],
  ["Rreaddir", { data: Uint8Array.of(5, 4, 3, 2, 1) }],
  ["Tfsync", { fid: 3, datasync: 1 }],
  ["Tlock", { fid: 3, type: 1, flags: 2, start: 3n, length: 4n, procId: 5, clientId: "client" }],
  ["Rlock", { status: 1 }],
  ["Tgetlock", { fid: 3, type: 1, start: 3n, length: 4n, procId: 5, clientId: "client" }],
  ["Rgetlock", { type: 1, start: 3n, length: 4n, procId: 5, clientId: "client" }],
]

for (const [name, value] of cases) {
  const write = upstreamProtocol[`write${name}`]
  const read = upstreamProtocol[`read${name}`]
  assert.equal(typeof write, "function", `upstream write${name}`)
  assert.equal(typeof read, "function", `upstream read${name}`)
  const upstreamBody = upstreamWire.encodeP9((writer) => write(writer, value))
  const nativeBody = native.encodeP9((writer) => native[`write${name}`](writer, value))
  assertBytes(nativeBody, upstreamBody, `9P ${name} bytes`)
  const upstreamValue = upstreamWire.decodeP9(upstreamBody, (reader) => read(reader))
  const nativeValue = native.decodeP9(nativeBody, (reader) => native[`read${name}`](reader))
  assert.deepEqual(plain(nativeValue), plain(upstreamValue), `9P ${name} round trip`)
}

const dirents = [
  { qid, offset: 1n, type: 4, name: "alpha" },
  { qid: { ...qid, path: 2n }, offset: 2n, type: 8, name: "café" },
]
const upstreamDirents = upstreamWire.encodeP9((writer) => {
  for (const value of dirents) upstreamProtocol.writeDirent(writer, value)
})
const nativeDirents = native.encodeP9((writer) => {
  for (const value of dirents) native.writeDirent(writer, value)
})
assertBytes(nativeDirents, upstreamDirents, "9P dirent bytes")
assert.deepEqual(plain(native.readDirents(nativeDirents)), plain(upstreamProtocol.readDirents(upstreamDirents)), "9P dirent list")
assert.equal(native.direntSize("café"), upstreamProtocol.direntSize("café"))

const packer = new native.P9DirentPacker(64)
assert.equal(packer.maxSize, 64, "9P dirent packer maxSize")
assert.equal(packer.size + packer.remaining, packer.maxSize, "9P dirent packer budget")
assert.equal(packer.add(dirents[0]), true, "9P dirent packer add")
assert.equal(packer.maxSize, 64, "9P dirent packer maxSize remains stable")

console.log(`mount-rs N-API 9P codec differential: PASS (${cases.length} typed cases)`)
