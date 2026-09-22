"use strict"

// This file adapts the owned N-API objects to mountx's callback/chaining API.
// It deliberately contains no wire-format implementation: all bytes,
// bounds-checking, framing, and scalar conversion happen in nfs_codec.rs.
const XDR_ERROR_MARKER = "__mount_rs_nfs_xdr_error_v1__"
const XDR_MAX_ITEM = 16 * 1024 * 1024
const WRITE_ACCEPTED_REPLY_HEADER = Symbol("writeAcceptedReplyHeader")

class XdrError extends Error {
  constructor(message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause })
    this.name = "XdrError"
    this.code = "ERR_NFS_XDR"
    this.offset = options.offset
  }
}

function isXdrError(error) {
  return error instanceof XdrError
}

function normalizeError(error) {
  if (!error || typeof error.message !== "string") return error
  const fields = error.message.split("|")
  if (fields.length !== 3 || fields[0] !== XDR_ERROR_MARKER) return error
  const [, offset, encoded] = fields
  const message = Buffer.from(encoded, "hex").toString("utf8")
  return new XdrError(message, { offset: Number(offset), cause: error })
}

function invoke(fn, thisArg, args) {
  try {
    return fn.apply(thisArg, args)
  } catch (error) {
    throw normalizeError(error)
  }
}

function copyBytes(value) {
  return Uint8Array.from(value)
}

class XdrReader {
  #inner

  constructor(bytes, offset = 0) {
    this.#inner = new this.constructor.native(Buffer.from(bytes), offset)
  }

  get bytes() {
    return invoke(this.#inner.__nfsBytes, this.#inner, [])
  }

  get offset() {
    return invoke(this.#inner.__nfsOffset, this.#inner, [])
  }

  get remaining() {
    return invoke(this.#inner.__nfsRemaining, this.#inner, [])
  }

  get atEnd() {
    return invoke(this.#inner.__nfsAtEnd, this.#inner, [])
  }

  u32(...args) {
    return invoke(this.#inner.__nfsU32, this.#inner, args)
  }

  i32(...args) {
    return invoke(this.#inner.__nfsI32, this.#inner, args)
  }

  u64(...args) {
    return invoke(this.#inner.__nfsU64, this.#inner, args)
  }

  i64(...args) {
    return invoke(this.#inner.__nfsI64, this.#inner, args)
  }

  bool(...args) {
    return invoke(this.#inner.__nfsBool, this.#inner, args)
  }

  fixedOpaque(...args) {
    return invoke(this.#inner.__nfsFixedOpaque, this.#inner, args)
  }

  varOpaque(...args) {
    return invoke(this.#inner.__nfsVarOpaque, this.#inner, args)
  }

  string(...args) {
    return invoke(this.#inner.__nfsString, this.#inner, args)
  }

  optional(read, what = "optional") {
    return this.bool(`${what} present`) ? read(this) : undefined
  }

  array(read, max = 1 << 20, what = "array") {
    const count = this.u32(`${what} count`)
    if (count > max || count > this.remaining / 4) {
      throw new XdrError(`${what} claims ${count} items, which cannot fit`, {
        offset: this.offset - 4,
      })
    }
    const values = []
    for (let index = 0; index < count; index++) values.push(read(this))
    return values
  }

  list(read, max = 1 << 20, what = "list") {
    const values = []
    while (this.bool(`${what} next`)) {
      if (values.length >= max) {
        throw new XdrError(`${what} is longer than ${max} items`, { offset: this.offset })
      }
      values.push(read(this))
    }
    return values
  }

  rest() {
    return invoke(this.#inner.__nfsRest, this.#inner, [])
  }

  end(...args) {
    return invoke(this.#inner.__nfsEnd, this.#inner, args)
  }
}

class XdrWriter {
  #inner

  constructor(capacity) {
    this.#inner = new this.constructor.native(capacity)
  }

  get length() {
    return invoke(this.#inner.__nfsLength, this.#inner, [])
  }

  ensure(count) {
    invoke(this.#inner.__nfsEnsure, this.#inner, [count])
    return this
  }

  truncate(length) {
    invoke(this.#inner.__nfsTruncate, this.#inner, [length])
    return this
  }

  u32(value) {
    invoke(this.#inner.__nfsU32, this.#inner, [value])
    return this
  }

  i32(value) {
    invoke(this.#inner.__nfsI32, this.#inner, [value])
    return this
  }

  u64(value) {
    invoke(this.#inner.__nfsU64, this.#inner, [value])
    return this
  }

  i64(value) {
    invoke(this.#inner.__nfsI64, this.#inner, [value])
    return this
  }

  bool(value) {
    invoke(this.#inner.__nfsBool, this.#inner, [value])
    return this
  }

  fixedOpaque(value, length) {
    invoke(this.#inner.__nfsFixedOpaque, this.#inner, [copyBytes(value), length])
    return this
  }

  varOpaque(value) {
    invoke(this.#inner.__nfsVarOpaque, this.#inner, [copyBytes(value)])
    return this
  }

  string(value) {
    invoke(this.#inner.__nfsString, this.#inner, [value])
    return this
  }

  optional(value, write) {
    if (value === undefined) return this.bool(false)
    this.bool(true)
    write(this, value)
    return this
  }

  array(values, write) {
    this.u32(values.length)
    for (const value of values) write(this, value)
    return this
  }

  list(values, write) {
    for (const value of values) {
      this.bool(true)
      write(this, value)
    }
    return this.bool(false)
  }

  raw(value) {
    invoke(this.#inner.__nfsRaw, this.#inner, [copyBytes(value)])
    return this
  }

  bytes() {
    return invoke(this.#inner.__nfsBytes, this.#inner, [])
  }

  view() {
    return invoke(this.#inner.__nfsView, this.#inner, [])
  }

  [WRITE_ACCEPTED_REPLY_HEADER](xid) {
    invoke(this.#inner.__nfsWriteAcceptedReplyHeader, this.#inner, [xid])
    return this
  }
}

class RecordAssembler {
  #inner

  constructor(limit) {
    this.#inner = new RecordAssembler.native(limit)
  }

  get pending() {
    return invoke(this.#inner.__nfsPending, this.#inner, [])
  }

  push(chunk) {
    return invoke(this.#inner.__nfsPush, this.#inner, [copyBytes(chunk)])
  }
}

function normalizeAuth(value) {
  return value === undefined
    ? undefined
    : { flavor: value.flavor, body: copyBytes(value.body) }
}

function install(binding) {
  if (!binding || !binding.NfsXdrReader || !binding.NfsXdrWriter) return binding
  if (binding.__mountRsNfsCodecInstalled) return binding

  const NativeReader = binding.NfsXdrReader
  const NativeWriter = binding.NfsXdrWriter
  const NativeAssembler = binding.NfsRecordAssembler

  // Keep the raw native methods under private adapter names. The public
  // classes above normalize Uint8Array inputs and preserve upstream method
  // chaining without putting codec behavior in JavaScript.
  XdrReader.native = NativeReader
  XdrWriter.native = NativeWriter
  RecordAssembler.native = NativeAssembler
  const expose = (prototype, publicName, nativeName, getter = false) => {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, nativeName)
    const value = getter
      ? function getNativeProperty() {
          return descriptor.get.call(this)
        }
      : descriptor.value
    Object.defineProperty(prototype, publicName, {
      configurable: true,
      enumerable: false,
      value,
    })
  }
  for (const [publicName, nativeName] of [
    ["__nfsBytes", "bytes"],
    ["__nfsOffset", "offset"],
    ["__nfsRemaining", "remaining"],
    ["__nfsAtEnd", "atEnd"],
    ["__nfsU32", "u32"],
    ["__nfsI32", "i32"],
    ["__nfsU64", "u64"],
    ["__nfsI64", "i64"],
    ["__nfsBool", "bool"],
    ["__nfsFixedOpaque", "fixedOpaque"],
    ["__nfsVarOpaque", "varOpaque"],
    ["__nfsString", "string"],
    ["__nfsRest", "rest"],
    ["__nfsEnd", "end"],
  ]) {
    expose(
      NativeReader.prototype,
      publicName,
      nativeName,
      publicName === "__nfsBytes" ||
        publicName === "__nfsOffset" ||
        publicName === "__nfsRemaining" ||
        publicName === "__nfsAtEnd",
    )
  }
  for (const [publicName, nativeName] of [
    ["__nfsLength", "length"],
    ["__nfsEnsure", "ensure"],
    ["__nfsTruncate", "truncate"],
    ["__nfsU32", "u32"],
    ["__nfsI32", "i32"],
    ["__nfsU64", "u64"],
    ["__nfsI64", "i64"],
    ["__nfsBool", "bool"],
    ["__nfsFixedOpaque", "fixedOpaque"],
    ["__nfsVarOpaque", "varOpaque"],
    ["__nfsString", "string"],
    ["__nfsRaw", "raw"],
    ["__nfsBytes", "bytes"],
    ["__nfsView", "view"],
    ["__nfsWriteAcceptedReplyHeader", "nfsWriteAcceptedReplyHeader"],
  ]) {
    expose(NativeWriter.prototype, publicName, nativeName, publicName === "__nfsLength")
  }
  for (const [publicName, nativeName] of [
    ["__nfsPending", "pending"],
    ["__nfsPush", "push"],
  ]) {
    expose(NativeAssembler.prototype, publicName, nativeName, publicName === "__nfsPending")
  }

  binding.XdrError = XdrError
  binding.isXdrError = isXdrError
  binding.XdrReader = XdrReader
  binding.XdrWriter = XdrWriter
  binding.XDR_MAX_ITEM = XDR_MAX_ITEM
  binding.xdrPad = (...args) => invoke(binding.nfsXdrPad, binding, args)
  binding.xdrAlign = (...args) => invoke(binding.nfsXdrAlign, binding, args)
  binding.stringByteLength = (...args) => invoke(binding.nfsStringByteLength, binding, args)

  binding.encodeXdr = (write, capacity) => {
    const writer = new XdrWriter(capacity)
    write(writer)
    return writer.bytes()
  }
  binding.decodeXdr = (bytes, read, what) => {
    const reader = new XdrReader(bytes)
    const value = read(reader)
    reader.end(what)
    return value
  }

  if (typeof binding.nfsRpcConstants === "function") {
    Object.assign(binding, invoke(binding.nfsRpcConstants, binding, []))
  }
  binding.AUTH_NULL = Object.freeze(normalizeAuth(invoke(binding.nfsAuthNull, binding, [])))

  binding.encodeAuthSys = (params) => invoke(binding.nfsEncodeAuthSys, binding, [params])
  binding.decodeAuthSys = (body) => invoke(binding.nfsDecodeAuthSys, binding, [copyBytes(body)])
  binding.authSys = (
    uid = process.getuid?.() ?? 0,
    gid = process.getgid?.() ?? 0,
    machineName = "mountx",
  ) => normalizeAuth(invoke(binding.nfsAuthSys, binding, [uid, gid, machineName]))
  binding.credentialsOf = (credential) => {
    const value = invoke(binding.nfsCredentialsOf, binding, [normalizeAuth(credential)])
    // napi omits undefined optional object fields; the upstream RPC surface
    // keeps uid/gid present with undefined for AUTH_NONE and malformed
    // AUTH_SYS credentials.
    return {
      ...value,
      uid: value.uid ?? undefined,
      gid: value.gid ?? undefined,
      gids: value.gids ?? [],
    }
  }
  binding.decodeCall = (bytes) => {
    const decoded = invoke(binding.nfsDecodeCall, binding, [copyBytes(bytes)])
    return {
      call: {
        ...decoded.call,
        cred: normalizeAuth(decoded.call.cred),
        verf: normalizeAuth(decoded.call.verf),
      },
      args: new XdrReader(decoded.argsBytes, decoded.argsOffset),
    }
  }
  binding.encodeCall = (options) =>
    invoke(binding.nfsEncodeCall, binding, [
      {
        ...options,
        cred: normalizeAuth(options.cred),
        verf: normalizeAuth(options.verf),
        args: options.args === undefined ? undefined : copyBytes(options.args),
      },
    ])
  binding.decodeReply = (bytes) => {
    const decoded = invoke(binding.nfsDecodeReply, binding, [copyBytes(bytes)])
    return {
      reply: {
        ...decoded.reply,
        acceptStat: decoded.reply.acceptStat ?? undefined,
        rejectStat: decoded.reply.rejectStat ?? undefined,
        authStat: decoded.reply.authStat ?? undefined,
        low: decoded.reply.low ?? undefined,
        high: decoded.reply.high ?? undefined,
        verf: normalizeAuth(decoded.reply.verf),
      },
      results: new XdrReader(decoded.resultsBytes, decoded.resultsOffset),
    }
  }
  binding.writeAcceptedReplyHeader = (writer, xid) => writer[WRITE_ACCEPTED_REPLY_HEADER](xid)
  binding.encodeAcceptedReply = (xid, results) =>
    invoke(binding.nfsEncodeAcceptedReply, binding, [
      xid,
      results === undefined ? undefined : copyBytes(results),
    ])
  binding.encodeAcceptError = (xid, acceptStat, mismatch) =>
    invoke(binding.nfsEncodeAcceptError, binding, [xid, acceptStat, mismatch])
  binding.encodeAuthError = (xid, authStat) =>
    invoke(binding.nfsEncodeAuthError, binding, [xid, authStat])
  binding.encodeRpcMismatch = (xid, low = binding.RPC_VERSION, high = binding.RPC_VERSION) =>
    invoke(binding.nfsEncodeRpcMismatch, binding, [xid, low, high])
  binding.recordMark = (length) => {
    // The raw upstream helper deliberately relies on DataView's uint32
    // conversion, so an oversized number is truncated instead of rejected.
    // Keep the Rust implementation strict for transport validation while
    // preserving that compatibility at this stateless codec boundary.
    if (typeof length === "number" && length > binding.RM_LENGTH_MASK) {
      const mark = new Uint8Array(4)
      new DataView(mark.buffer).setUint32(
        0,
        binding.RM_LAST_FRAGMENT | length,
        false,
      )
      return mark
    }
    return invoke(binding.nfsRecordMark, binding, [length])
  }
  binding.frameRecord = (message) =>
    invoke(binding.nfsFrameRecord, binding, [copyBytes(message)])
  binding.frameFragments = (message, size) =>
    invoke(binding.nfsFrameFragments, binding, [copyBytes(message), size])
  binding.copyBytes = (bytes, start, end) =>
    invoke(binding.nfsCopyBytes, binding, [copyBytes(bytes), start, end])
  binding.RecordAssembler = RecordAssembler

  Object.defineProperty(binding, "__mountRsNfsCodecInstalled", { value: true })
  return binding
}

module.exports = install
