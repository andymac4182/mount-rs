import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { pathToFileURL } from "node:url"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API NFS codec differential: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const require = createRequire(import.meta.url)
const root = require("@mount-rs/core")
const rootKeys = Object.keys(root)
const native = require("@mount-rs/core/nfs")
assert.deepEqual(Object.keys(root), rootKeys, "NFS facade must not mutate root exports")
assert.equal(native.createNfsServer, root.createNfsServer)
assert.equal(native.NfsServer, root.NfsServer)
assert.equal(native.NfsSession, root.NfsSession)
assert.equal(native.Nfs4Session, root.Nfs4Session)

const [upstreamXdr, upstreamRpc] = await Promise.all([
  import(pathToFileURL(`${source}/src/nfs/xdr.ts`).href),
  import(pathToFileURL(`${source}/src/nfs/rpc.ts`).href),
])

function bytes(value) {
  return Array.from(value)
}

function assertBytes(actual, expected, message) {
  assert.deepEqual(bytes(actual), bytes(expected), message)
}

function authShape(value) {
  return { flavor: value.flavor, body: bytes(value.body) }
}

function errorShape(api, operation, errorApi = api) {
  try {
    operation()
    assert.fail("expected an error")
  } catch (error) {
    return {
      isXdrError: errorApi.isXdrError(error),
      name: error.name,
      code: error.code,
      offset: error.offset,
      message: error.message,
    }
  }
}

function nativeAuth(value) {
  return { flavor: value.flavor, body: Buffer.from(value.body) }
}

function nativeBytes(value) {
  return Buffer.from(value)
}

function xdrSequence(api) {
  return api.encodeXdr((writer) => {
    writer.u32(0x01020304)
    writer.i32(-2)
    writer.u64(0x1122334455667788n)
    writer.i64(-0x0102030405060708n)
    writer.bool(true)
    writer.fixedOpaque(Buffer.from([1, 2, 3]), 5)
    writer.varOpaque(Buffer.from([4, 5, 6]))
    writer.string("café")
    writer.optional(0xfeed, (nested, value) => nested.u32(value))
    writer.array([7, 8, 9], (nested, value) => nested.u32(value))
    writer.list([10, 11], (nested, value) => nested.u32(value))
  }, 8)
}

function readXdrSequence(api, encoded) {
  return api.decodeXdr(encoded, (reader) => ({
    u32: reader.u32(),
    i32: reader.i32(),
    u64: reader.u64(),
    i64: reader.i64(),
    bool: reader.bool(),
    fixed: bytes(reader.fixedOpaque(5)),
    variable: bytes(reader.varOpaque()),
    string: reader.string(),
    optional: reader.optional((nested) => nested.u32()),
    array: reader.array((nested) => nested.u32()),
    list: reader.list((nested) => nested.u32()),
  }))
}

// ---------------------------------------------------------------------------
// XDR: every scalar, bounded aggregate, ownership rule, and callback adapter.
// ---------------------------------------------------------------------------

assert.equal(native.XDR_MAX_ITEM, upstreamXdr.XDR_MAX_ITEM)
for (const length of [0, 1, 3, 4, 7, 16]) {
  assert.equal(native.xdrPad(length), upstreamXdr.xdrPad(length))
  assert.equal(native.xdrAlign(length), upstreamXdr.xdrAlign(length))
}
for (const value of ["", "ascii", "café", "𐍈"]) {
  assert.equal(native.stringByteLength(value), upstreamXdr.stringByteLength(value))
}

const upstreamSequence = xdrSequence(upstreamXdr)
const nativeSequence = xdrSequence(native)
assertBytes(nativeSequence, upstreamSequence, "XDR encoded sequence")
assert.deepEqual(readXdrSequence(native, nativeSequence), readXdrSequence(upstreamXdr, upstreamSequence))

const upstreamWriter = new upstreamXdr.XdrWriter(4)
upstreamWriter.ensure(32).u32(1).u64(2n).truncate(4)
const nativeWriter = new native.XdrWriter(4)
nativeWriter.ensure(32).u32(1).u64(2n).truncate(4)
assertBytes(nativeWriter.bytes(), upstreamWriter.bytes(), "XDR writer bytes")
assertBytes(nativeWriter.view(), upstreamWriter.view(), "XDR writer view")
assert.equal(nativeWriter.length, upstreamWriter.length)

const upstreamReader = new upstreamXdr.XdrReader(upstreamSequence)
const nativeReader = new native.XdrReader(nativeSequence)
assert.equal(nativeReader.offset, upstreamReader.offset)
assert.equal(nativeReader.remaining, upstreamReader.remaining)
assert.equal(nativeReader.atEnd, upstreamReader.atEnd)
assert.equal(nativeReader.u32("first"), upstreamReader.u32("first"))
assertBytes(nativeReader.rest(), upstreamReader.rest(), "XDR rest copy")
assert.equal(nativeReader.offset, upstreamReader.offset)
assert.equal(nativeReader.atEnd, upstreamReader.atEnd)

for (const operation of [
  (api) => new api.XdrReader(Uint8Array.of(0, 0, 0, 2)).bool(),
  (api) => new api.XdrReader(Uint8Array.of(0, 0, 0)).u32(),
  (api) => new api.XdrReader(Uint8Array.of(0, 0, 0, 8)).varOpaque(),
  (api) => api.decodeXdr(Uint8Array.of(0, 0, 0, 1), (reader) => reader.u32(), "whole"),
]) {
  assert.deepEqual(
    errorShape(native, () => operation(native)),
    errorShape(upstreamXdr, () => operation(upstreamXdr)),
    "XDR error shape",
  )
}

// ---------------------------------------------------------------------------
// RPC: auth, call/reply headers, accepted/error replies, and record marking.
// ---------------------------------------------------------------------------

const rpcConstants = [
  "RPC_VERSION",
  "RPC_CALL",
  "RPC_REPLY",
  "MSG_ACCEPTED",
  "MSG_DENIED",
  "RPC_SUCCESS",
  "RPC_PROG_UNAVAIL",
  "RPC_PROG_MISMATCH",
  "RPC_PROC_UNAVAIL",
  "RPC_GARBAGE_ARGS",
  "RPC_SYSTEM_ERR",
  "RPC_MISMATCH",
  "RPC_AUTH_ERROR",
  "AUTH_OK",
  "AUTH_BADCRED",
  "AUTH_REJECTEDCRED",
  "AUTH_BADVERF",
  "AUTH_REJECTEDVERF",
  "AUTH_TOOWEAK",
  "AUTH_INVALIDRESP",
  "AUTH_FAILED",
  "AUTH_NONE",
  "AUTH_SYS",
  "AUTH_SHORT",
  "RPC_MAX_AUTH_BYTES",
  "RM_LAST_FRAGMENT",
  "RM_LENGTH_MASK",
  "ACCEPTED_REPLY_HEADER_SIZE",
]
for (const name of rpcConstants) assert.equal(native[name], upstreamRpc[name], name)
// The upstream surface does not export this as a named constant. Its
// RecordAssembler constructor default is the 8 MiB literal in
// src/nfs/rpc.ts; the native facade exposes the equivalent transport bound.
assert.equal(native.DEFAULT_RECORD_LIMIT, 8 * 1024 * 1024, "DEFAULT_RECORD_LIMIT")

const authParams = {
  stamp: 17,
  machineName: "nfs-client",
  uid: 1000,
  gid: 1001,
  gids: [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170],
}
const upstreamAuthBody = upstreamRpc.encodeAuthSys(authParams)
const nativeAuthBody = native.encodeAuthSys({ ...authParams, gids: authParams.gids })
assertBytes(nativeAuthBody, upstreamAuthBody, "AUTH_SYS body")
assert.deepEqual(native.decodeAuthSys(nativeAuthBody), upstreamRpc.decodeAuthSys(upstreamAuthBody))

const upstreamAuth = upstreamRpc.authSys()
const nativeAuthValue = native.authSys()
assert.deepEqual(authShape(nativeAuthValue), authShape(upstreamAuth))
assert.deepEqual(
  native.credentialsOf(nativeAuthValue),
  upstreamRpc.credentialsOf(upstreamAuth),
)
assert.deepEqual(
  native.credentialsOf({ flavor: native.AUTH_NONE, body: Buffer.from([1, 2, 3]) }),
  upstreamRpc.credentialsOf({ flavor: upstreamRpc.AUTH_NONE, body: Uint8Array.of(1, 2, 3) }),
)
assert.deepEqual(
  native.credentialsOf({ flavor: native.AUTH_SYS, body: Buffer.from([0, 0, 0, 1]) }),
  upstreamRpc.credentialsOf({ flavor: upstreamRpc.AUTH_SYS, body: Uint8Array.of(0, 0, 0, 1) }),
)

const callOptions = {
  xid: 7,
  program: 100003,
  version: 3,
  procedure: 6,
  cred: upstreamAuth,
  verf: { flavor: 42, body: Uint8Array.of(8, 9) },
  args: Uint8Array.of(1, 2, 3, 4),
}
const nativeCallOptions = {
  ...callOptions,
  cred: nativeAuth(callOptions.cred),
  verf: nativeAuth(callOptions.verf),
  args: nativeBytes(callOptions.args),
}
const upstreamCall = upstreamRpc.encodeCall(callOptions)
const nativeCall = native.encodeCall(nativeCallOptions)
assertBytes(nativeCall, upstreamCall, "RPC call")
const upstreamDecodedCall = upstreamRpc.decodeCall(upstreamCall)
const nativeDecodedCall = native.decodeCall(nativeCall)
assert.deepEqual(nativeDecodedCall.call, upstreamDecodedCall.call)
assertBytes(nativeDecodedCall.args.rest(), upstreamDecodedCall.args.rest(), "RPC call args")

const replyBytes = [
  upstreamRpc.encodeAcceptedReply(8, Uint8Array.of(0xaa, 0xbb)),
  upstreamRpc.encodeAcceptError(9, upstreamRpc.RPC_PROG_MISMATCH, { low: 2, high: 3 }),
  upstreamRpc.encodeAuthError(10, upstreamRpc.AUTH_BADCRED),
  upstreamRpc.encodeRpcMismatch(11),
]
const nativeReplyBytes = [
  native.encodeAcceptedReply(8, Buffer.from([0xaa, 0xbb])),
  native.encodeAcceptError(9, native.RPC_PROG_MISMATCH, { low: 2, high: 3 }),
  native.encodeAuthError(10, native.AUTH_BADCRED),
  native.encodeRpcMismatch(11),
]
for (let index = 0; index < replyBytes.length; index++) {
  assertBytes(nativeReplyBytes[index], replyBytes[index], `RPC reply ${index}`)
  const upstreamDecoded = upstreamRpc.decodeReply(replyBytes[index])
  const nativeDecoded = native.decodeReply(nativeReplyBytes[index])
  assert.deepEqual(nativeDecoded.reply, upstreamDecoded.reply)
  assertBytes(nativeDecoded.results.rest(), upstreamDecoded.results.rest(), `RPC results ${index}`)
}

const message = Uint8Array.from({ length: 37 }, (_, index) => index + 1)
assertBytes(native.recordMark(message.length), upstreamRpc.recordMark(message.length), "record mark")
assertBytes(native.frameRecord(message), upstreamRpc.frameRecord(message), "single record")
assertBytes(native.frameFragments(message, 5), upstreamRpc.frameFragments(message, 5), "fragmented record")
assertBytes(native.copyBytes(message, 3, 19), upstreamRpc.copyBytes(message, 3, 19), "copyBytes")

const upstreamAssembler = new upstreamRpc.RecordAssembler(1024)
const nativeAssembler = new native.RecordAssembler(1024)
const upstreamRecords = []
const nativeRecords = []
const framed = upstreamRpc.frameFragments(message, 5)
for (let offset = 0; offset < framed.length; offset += 3) {
  const chunk = framed.subarray(offset, offset + 3)
  upstreamRecords.push(...upstreamAssembler.push(chunk))
  nativeRecords.push(...nativeAssembler.push(Buffer.from(chunk)))
  assert.equal(nativeAssembler.pending, upstreamAssembler.pending)
}
assert.deepEqual(nativeRecords.map(bytes), upstreamRecords.map(bytes), "record assembler")

assertBytes(
  native.recordMark(native.RM_LENGTH_MASK + 1),
  upstreamRpc.recordMark(upstreamRpc.RM_LENGTH_MASK + 1),
  "oversized record mark",
)

for (const operation of [
  (api) => api.frameFragments(Uint8Array.of(1), 0),
  (api) => new api.RecordAssembler(2).push(Buffer.from([0x80, 0, 0, 3, 1, 2, 3])),
]) {
  const upstreamOperation = (api) => operation(api)
  assert.equal(
    errorShape(native, () => upstreamOperation(native)).isXdrError,
    errorShape(upstreamRpc, () => upstreamOperation(upstreamRpc), upstreamXdr).isXdrError,
    "RPC error class",
  )
}

console.log("mount-rs N-API NFS codec differential: PASS")
