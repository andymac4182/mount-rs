"use strict"

// This facade only supplies the upstream callback/alias API.  All bytes,
// bounds checks, 64-bit conversion, typed fields, and framing are implemented
// by the Rust classes in p9_codec.rs.

const WRAPPED = Symbol("mountRsP9CodecWrapped")
const P9_ERROR_MARKER = "__mount_rs_p9_error_v1__"

class P9Error extends Error {
  constructor(message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause })
    this.name = "P9Error"
    this.code = "ERR_9P_WIRE"
    this.offset = options.offset
  }
}

function decodeHex(value) {
  if (value === "") return ""
  if (!/^(?:[0-9a-f]{2})+$/u.test(value)) return value
  return Buffer.from(value, "hex").toString("utf8")
}

function revive(error) {
  if (error instanceof P9Error) return error
  const message = String(error && error.message !== undefined ? error.message : error)
  const prefix = `${P9_ERROR_MARKER}|`
  if (!message.startsWith(prefix)) return error
  const parts = message.split("|")
  const offset = parts[1] === "-" || parts[1] === undefined ? undefined : Number(parts[1])
  return new P9Error(decodeHex(parts.slice(2).join("|")), { offset, cause: error })
}

function invoke(fn, receiver, args) {
  try {
    return fn.apply(receiver, args)
  } catch (error) {
    throw revive(error)
  }
}

function construct(ctor, args) {
  try {
    return Reflect.construct(ctor, args)
  } catch (error) {
    throw revive(error)
  }
}

function wrapPrototype(ctor) {
  if (!ctor || !ctor.prototype || ctor.prototype[WRAPPED]) return
  const prototype = ctor.prototype
  for (const name of Object.getOwnPropertyNames(prototype)) {
    if (name === "constructor") continue
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name)
    if (!descriptor) continue
    const replacement = { ...descriptor, configurable: true }
    if (typeof descriptor.value === "function") {
      const native = descriptor.value
      replacement.value = function wrappedP9Method(...args) {
        return invoke(native, this, args)
      }
    }
    if (typeof descriptor.get === "function") {
      const native = descriptor.get
      replacement.get = function wrappedP9Getter() {
        return invoke(native, this, [])
      }
    }
    if (typeof descriptor.set === "function") {
      const native = descriptor.set
      replacement.set = function wrappedP9Setter(value) {
        return invoke(native, this, [value])
      }
    }
    Object.defineProperty(prototype, name, replacement)
  }
  Object.defineProperty(prototype, WRAPPED, { value: true })
}

function chainWriterMethods(Writer) {
  if (!Writer || !Writer.prototype) return
  for (const name of ["u8", "u16", "u32", "u64", "string", "qid", "blob", "raw", "patchU32"]) {
    const method = Writer.prototype[name]
    if (typeof method !== "function" || method.__mountRsP9Chainable) continue
    const wrapped = function chainableP9WriterMethod(...args) {
      if (name === "blob" || name === "raw") args = [Buffer.from(args[0])]
      if (name === "u8") args = [Number(args[0]) & 0xff]
      if (name === "u16") args = [Number(args[0]) & 0xffff]
      if (name === "u32") args = [Number(args[0]) >>> 0]
      if (name === "patchU32") args = [Number(args[0]) >>> 0, Number(args[1]) >>> 0]
      method.apply(this, args)
      return this
    }
    Object.defineProperty(wrapped, "__mountRsP9Chainable", { value: true })
    Object.defineProperty(Writer.prototype, name, {
      configurable: true,
      enumerable: false,
      writable: true,
      value: wrapped,
    })
  }
}

function typedWriter(binding, name) {
  const method = `write${name}`
  return (writer, value) => {
    if (name === "Rread" || name === "Twrite" || name === "Rreaddir") {
      value = { ...value, data: Buffer.from(value.data) }
    }
    return invoke(writer[method], writer, [value])
  }
}

function typedReader(binding, name) {
  const method = `read${name}`
  return (reader) => invoke(reader[method], reader, [])
}

function installP9Codec(binding) {
  const Reader = binding && binding.NativeP9Reader
  const Writer = binding && binding.NativeP9Writer
  const Assembler = binding && binding.NativeP9FrameAssembler
  if (!Reader || !Writer || !Assembler) return binding

  wrapPrototype(Reader)
  wrapPrototype(Writer)
  wrapPrototype(Assembler)
  wrapPrototype(binding.NativeP9DirentPacker)
  chainWriterMethods(Writer)

  const nativePush = Assembler.prototype.push
  if (typeof nativePush === "function") {
    Object.defineProperty(Assembler.prototype, "push", {
      configurable: true,
      enumerable: false,
      writable: true,
      value(chunk) {
        return invoke(nativePush, this, [Buffer.from(chunk)])
      },
    })
  }

  // N-API's Buffer parameter is the zero-copy/native representation.  Keep
  // the upstream Uint8Array call convention at the public boundary for the
  // two readers whose optional arguments differ from the Rust ABI.
  const nativeString = Reader.prototype.string
  if (typeof nativeString === "function") {
    Object.defineProperty(Reader.prototype, "string", {
      configurable: true,
      enumerable: false,
      writable: true,
      value(max, what) {
        if (typeof max === "string") {
          what = max
          max = undefined
        }
        return invoke(nativeString, this, [max, what])
      },
    })
  }
  const nativeBlob = Reader.prototype.blob
  if (typeof nativeBlob === "function") {
    Object.defineProperty(Reader.prototype, "blob", {
      configurable: true,
      enumerable: false,
      writable: true,
      value(max, what) {
        if (typeof max === "string") {
          what = max
          max = undefined
        }
        return invoke(nativeBlob, this, [max, what])
      },
    })
  }

  binding.P9Error = P9Error
  binding.isP9Error = (error) => error instanceof P9Error
  // Keep the upstream `new P9Reader(Uint8Array)` constructor contract while
  // retaining the native class as the actual receiver.  Returning the native
  // object also preserves `instanceof P9Reader` because both prototypes are
  // deliberately the same.
  function P9Reader(bytes, offset = 0) {
    return construct(Reader, [Buffer.from(bytes), offset])
  }
  P9Reader.prototype = Reader.prototype
  binding.P9Reader = P9Reader
  binding.P9Writer = Writer
  function P9FrameAssembler(limit) {
    return construct(Assembler, [limit])
  }
  P9FrameAssembler.prototype = Assembler.prototype
  binding.P9FrameAssembler = P9FrameAssembler
  binding.P9DirentPacker = binding.NativeP9DirentPacker
  binding.P9_DEFAULT_MAX_FRAME = 1024 * 1024
  binding.P9_QID_SIZE = 13
  binding.P9_MAX_STRING = 0xffff
  binding.P9_MAX_ITEM = 16 * 1024 * 1024
  binding.DEFAULT_P9_PORT = 564
  binding.DEFAULT_SOCKET_MODE = 0o600
  binding.DEFAULT_MAX_IN_FLIGHT = 16
  binding.DEFAULT_MSIZE = 1024 * 1024
  binding.P9_LOCK_EOF_END = 1n << 64n
  binding.DEFAULT_MAX_LOCKS_PER_FILE = 1024

  binding.encodeP9 = (write, capacity = 256) => {
    const writer = new Writer(capacity)
    if (write !== undefined) invoke(write, undefined, [writer])
    return invoke(writer.bytes, writer, [])
  }
  binding.decodeP9 = (bytes, read, what = "message") => {
    const reader = new P9Reader(bytes)
    const value = invoke(read, undefined, [reader])
    invoke(reader.end, reader, [what])
    return value
  }
  binding.stringByteLength = (value) => invoke(binding.nativeP9StringByteLength, binding, [value])

  binding.readHeader = (reader) => invoke(reader.readHeader, reader, [])
  binding.writeHeader = (writer, header) => invoke(writer.writeHeader, writer, [header])
  binding.readTime = (reader, what = "time") => invoke(reader.readTime, reader, [what])
  binding.writeTime = (writer, value) => invoke(writer.writeTime, writer, [value])

  binding.encodeMessage = (type, tag, write, capacity = 128) => {
    const body = binding.encodeP9(write)
    return invoke(binding.nativeP9EncodeMessage, binding, [type, tag, Buffer.from(body), capacity])
  }
  binding.decodeMessage = (bytes) => {
    const decoded = invoke(binding.nativeP9DecodeMessage, binding, [Buffer.from(bytes)])
    return { ...decoded.header, body: new P9Reader(decoded.body) }
  }
  binding.decodeMessageAs = (bytes, read) => {
    const message = binding.decodeMessage(bytes)
    const value = invoke(read, undefined, [message.body])
    const name = typeof binding.messageName === "function" ? binding.messageName(message.type) : `9P message ${message.type}`
    invoke(message.body.end, message.body, [name])
    return { size: message.size, type: message.type, tag: message.tag, value }
  }
  binding.readEmptyBody = (reader, what = "message") => invoke(reader.end, reader, [what])

  for (const name of [
    "Tversion", "Rversion", "Tauth", "Rauth", "Tattach", "Rattach", "Rlerror", "Tflush",
    "Twalk", "Rwalk", "Tread", "Rread", "Twrite", "Rwrite", "FidRequest", "Rstatfs",
    "Tlopen", "Rlopen", "Tlcreate", "Tsymlink", "QidReply", "Tmknod", "Tmkdir", "Trename",
    "Trenameat", "Tunlinkat", "Tlink", "Rreadlink", "Tgetattr", "Rgetattr", "Tsetattr",
    "Txattrwalk", "Rxattrwalk", "Txattrcreate", "Treaddir", "Rreaddir", "Dirent", "Tfsync",
    "Tlock", "Rlock", "Tgetlock", "Rgetlock",
  ]) {
    const readName = `read${name}`
    const writeName = `write${name}`
    if (typeof Reader.prototype[readName] === "function") binding[readName] = typedReader(binding, name)
    if (typeof Writer.prototype[writeName] === "function") binding[writeName] = typedWriter(binding, name)
  }

  // These are aliases in the upstream module and intentionally stay aliases
  // here too; the Rust codec has one implementation for each identical body.
  for (const name of ["Tclunk", "Tremove", "Tstatfs", "Treadlink"]) {
    binding[`read${name}`] = binding.readFidRequest
    binding[`write${name}`] = binding.writeFidRequest
  }
  for (const name of ["Rsymlink", "Rmknod", "Rmkdir"]) {
    binding[`read${name}`] = binding.readQidReply
    binding[`write${name}`] = binding.writeQidReply
  }
  binding.readRlcreate = binding.readRlopen
  binding.writeRlcreate = binding.writeRlopen

  binding.direntSize = (name) => invoke(binding.nativeP9DirentSize, binding, [name])
  binding.readDirents = (bytes) => invoke(binding.nativeP9ReadDirents, binding, [Buffer.from(bytes)])

  const emptyTypes = [
    "RFLUSH", "RCLUNK", "RREMOVE", "RRENAME", "RSETATTR", "RXATTRCREATE", "RFSYNC", "RLINK",
    "RRENAMEAT", "RUNLINKAT",
  ]
  binding.EMPTY_BODY = new Set(emptyTypes.map((name) => binding[`P9_${name}`]).filter((value) => typeof value === "number"))

  binding.framesFrom = async function* framesFrom(chunks, assembler = new P9FrameAssembler()) {
    for await (const chunk of chunks) {
      for (const frame of assembler.push(chunk)) yield frame
    }
  }
  return binding
}

module.exports = installP9Codec
