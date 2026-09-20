"use strict"

// This postlude is the mount-free part of the FUSE public surface.  The Rust
// transport owns byte layouts and validation; JavaScript only supplies the
// oracle-shaped names, error classes, constants, and recorder clock option.
// There is intentionally no session/device/mount binding here.

const ERROR_MARKER = "__mount_rs_fuse_codec_error_v1__"

class ProtocolError extends Error {
  constructor(message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause })
    this.name = "ProtocolError"
    this.code = "ERR_FUSE_PROTOCOL"
    this.offset = options.offset
  }
}

class TranscriptError extends Error {
  constructor(message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause })
    this.name = "TranscriptError"
  }
}

function decodeHex(value) {
  return Buffer.from(value, "hex").toString("utf8")
}

function revive(error) {
  if (!error || typeof error.message !== "string") return error
  const fields = error.message.split("|")
  if (fields[0] !== ERROR_MARKER || fields.length !== 4) return error
  const [, kind, offset, encoded] = fields
  const options = { cause: error }
  if (offset !== "-") options.offset = Number(offset)
  return kind === "transcript"
    ? new TranscriptError(decodeHex(encoded), options)
    : new ProtocolError(decodeHex(encoded), options)
}

function invoke(fn, receiver, args) {
  try {
    return fn.apply(receiver, args)
  } catch (error) {
    throw revive(error)
  }
}

function copyBytes(value) {
  return Buffer.from(value)
}

function context(value) {
  return value === undefined ? undefined : { minor: value.minor, setxattrExt: value.setxattrExt }
}

function installConstants(binding) {
  const constants = {
    FUSE_KERNEL_VERSION: 7,
    FUSE_KERNEL_MINOR_VERSION: 41,
    FUSE_ROOT_ID: 1n,
    FUSE_MIN_READ_BUFFER: 8192,
    FUSE_PAGE_SIZE: 4096,
    FUSE_MAX_MAX_PAGES: 256,
    FUSE_DEFAULT_MAX_PAGES_PER_REQ: 32,
    FUSE_LOOKUP: 1,
    FUSE_FORGET: 2,
    FUSE_GETATTR: 3,
    FUSE_SETATTR: 4,
    FUSE_READLINK: 5,
    FUSE_SYMLINK: 6,
    FUSE_MKNOD: 8,
    FUSE_MKDIR: 9,
    FUSE_UNLINK: 10,
    FUSE_RMDIR: 11,
    FUSE_RENAME: 12,
    FUSE_LINK: 13,
    FUSE_OPEN: 14,
    FUSE_READ: 15,
    FUSE_WRITE: 16,
    FUSE_STATFS: 17,
    FUSE_RELEASE: 18,
    FUSE_FSYNC: 20,
    FUSE_SETXATTR: 21,
    FUSE_GETXATTR: 22,
    FUSE_LISTXATTR: 23,
    FUSE_REMOVEXATTR: 24,
    FUSE_FLUSH: 25,
    FUSE_INIT: 26,
    FUSE_OPENDIR: 27,
    FUSE_READDIR: 28,
    FUSE_RELEASEDIR: 29,
    FUSE_FSYNCDIR: 30,
    FUSE_GETLK: 31,
    FUSE_SETLK: 32,
    FUSE_SETLKW: 33,
    FUSE_ACCESS: 34,
    FUSE_CREATE: 35,
    FUSE_INTERRUPT: 36,
    FUSE_BMAP: 37,
    FUSE_DESTROY: 38,
    FUSE_IOCTL: 39,
    FUSE_POLL: 40,
    FUSE_NOTIFY_REPLY: 41,
    FUSE_BATCH_FORGET: 42,
    FUSE_FALLOCATE: 43,
    FUSE_READDIRPLUS: 44,
    FUSE_RENAME2: 45,
    FUSE_LSEEK: 46,
    FUSE_COPY_FILE_RANGE: 47,
    FUSE_SETUPMAPPING: 48,
    FUSE_REMOVEMAPPING: 49,
    FUSE_SYNCFS: 50,
    FUSE_TMPFILE: 51,
    FUSE_STATX: 52,
    CUSE_INIT: 4096,
    FUSE_NOTIFY_POLL: 1,
    FUSE_NOTIFY_INVAL_INODE: 2,
    FUSE_NOTIFY_INVAL_ENTRY: 3,
    FUSE_NOTIFY_STORE: 4,
    FUSE_NOTIFY_RETRIEVE: 5,
    FUSE_NOTIFY_DELETE: 6,
    FUSE_NOTIFY_RESEND: 7,
    FUSE_NOTIFY_UNIQUE: 0n,
    FATTR_MODE: 1 << 0,
    FATTR_UID: 1 << 1,
    FATTR_GID: 1 << 2,
    FATTR_SIZE: 1 << 3,
    FATTR_ATIME: 1 << 4,
    FATTR_MTIME: 1 << 5,
    FATTR_FH: 1 << 6,
    FATTR_ATIME_NOW: 1 << 7,
    FATTR_MTIME_NOW: 1 << 8,
    FATTR_LOCKOWNER: 1 << 9,
    FATTR_CTIME: 1 << 10,
    FATTR_KILL_SUIDGID: 1 << 11,
    FOPEN_DIRECT_IO: 1 << 0,
    FOPEN_KEEP_CACHE: 1 << 1,
    FOPEN_NONSEEKABLE: 1 << 2,
    FOPEN_CACHE_DIR: 1 << 3,
    FOPEN_STREAM: 1 << 4,
    FOPEN_NOFLUSH: 1 << 5,
    FOPEN_PARALLEL_DIRECT_WRITES: 1 << 6,
    FOPEN_PASSTHROUGH: 1 << 7,
    FUSE_ASYNC_READ: 1n << 0n,
    FUSE_POSIX_LOCKS: 1n << 1n,
    FUSE_FILE_OPS: 1n << 2n,
    FUSE_ATOMIC_O_TRUNC: 1n << 3n,
    FUSE_EXPORT_SUPPORT: 1n << 4n,
    FUSE_BIG_WRITES: 1n << 5n,
    FUSE_DONT_MASK: 1n << 6n,
    FUSE_SPLICE_WRITE: 1n << 7n,
    FUSE_SPLICE_MOVE: 1n << 8n,
    FUSE_SPLICE_READ: 1n << 9n,
    FUSE_FLOCK_LOCKS: 1n << 10n,
    FUSE_HAS_IOCTL_DIR: 1n << 11n,
    FUSE_AUTO_INVAL_DATA: 1n << 12n,
    FUSE_DO_READDIRPLUS: 1n << 13n,
    FUSE_READDIRPLUS_AUTO: 1n << 14n,
    FUSE_ASYNC_DIO: 1n << 15n,
    FUSE_WRITEBACK_CACHE: 1n << 16n,
    FUSE_NO_OPEN_SUPPORT: 1n << 17n,
    FUSE_PARALLEL_DIROPS: 1n << 18n,
    FUSE_HANDLE_KILLPRIV: 1n << 19n,
    FUSE_POSIX_ACL: 1n << 20n,
    FUSE_ABORT_ERROR: 1n << 21n,
    FUSE_MAX_PAGES: 1n << 22n,
    FUSE_CACHE_SYMLINKS: 1n << 23n,
    FUSE_NO_OPENDIR_SUPPORT: 1n << 24n,
    FUSE_EXPLICIT_INVAL_DATA: 1n << 25n,
    FUSE_MAP_ALIGNMENT: 1n << 26n,
    FUSE_SUBMOUNTS: 1n << 27n,
    FUSE_HANDLE_KILLPRIV_V2: 1n << 28n,
    FUSE_SETXATTR_EXT: 1n << 29n,
    FUSE_INIT_EXT: 1n << 30n,
    FUSE_INIT_RESERVED: 1n << 31n,
    FUSE_SECURITY_CTX: 1n << 32n,
    FUSE_HAS_INODE_DAX: 1n << 33n,
    FUSE_CREATE_SUPP_GROUP: 1n << 34n,
    FUSE_HAS_EXPIRE_ONLY: 1n << 35n,
    FUSE_DIRECT_IO_ALLOW_MMAP: 1n << 36n,
    FUSE_PASSTHROUGH: 1n << 37n,
    FUSE_NO_EXPORT_SUPPORT: 1n << 38n,
    FUSE_HAS_RESEND: 1n << 39n,
    FUSE_ALLOW_IDMAP: 1n << 40n,
    FUSE_RELEASE_FLUSH: 1 << 0,
    FUSE_RELEASE_FLOCK_UNLOCK: 1 << 1,
    FUSE_GETATTR_FH: 1 << 0,
    FUSE_LK_FLOCK: 1 << 0,
    FUSE_WRITE_CACHE: 1 << 0,
    FUSE_WRITE_LOCKOWNER: 1 << 1,
    FUSE_WRITE_KILL_SUIDGID: 1 << 2,
    FUSE_READ_LOCKOWNER: 1 << 1,
    FUSE_POLL_SCHEDULE_NOTIFY: 1 << 0,
    FUSE_FSYNC_FDATASYNC: 1 << 0,
    FUSE_ATTR_SUBMOUNT: 1 << 0,
    FUSE_ATTR_DAX: 1 << 1,
    FUSE_OPEN_KILL_SUIDGID: 1 << 0,
    FUSE_SETXATTR_ACL_KILL_SGID: 1 << 0,
    FUSE_EXPIRE_ONLY: 1 << 0,
    FUSE_UNIQUE_RESEND: 1n << 63n,
    FUSE_INVALID_UIDGID: 0xffffffff,
    FUSE_MAX_NR_SECCTX: 31,
    FUSE_EXT_GROUPS: 32,
    DT_UNKNOWN: 0,
    DT_FIFO: 1,
    DT_CHR: 2,
    DT_DIR: 4,
    DT_BLK: 6,
    DT_REG: 8,
    DT_LNK: 10,
    DT_SOCK: 12,
    O_ACCMODE: 0o3,
    O_RDONLY: 0o0,
    O_WRONLY: 0o1,
    O_RDWR: 0o2,
    O_CREAT: 0o100,
    O_EXCL: 0o200,
    O_TRUNC: 0o1000,
    O_APPEND: 0o2000,
    SEEK_SET: 0,
    SEEK_CUR: 1,
    SEEK_END: 2,
    SEEK_DATA: 3,
    SEEK_HOLE: 4,
    F_RDLCK: 0,
    F_WRLCK: 1,
    F_UNLCK: 2,
    XATTR_CREATE: 1,
    XATTR_REPLACE: 2,
    FUSE_IN_HEADER_SIZE: 40,
    FUSE_OUT_HEADER_SIZE: 16,
    FUSE_DIRENT_HEADER_SIZE: 24,
    FUSE_INIT_OUT_SIZE: 64,
    FUSE_COMPAT_INIT_OUT_SIZE: 8,
    FUSE_COMPAT_22_INIT_OUT_SIZE: 24,
    FUSE_COMPAT_ENTRY_OUT_SIZE: 120,
    FUSE_COMPAT_ATTR_OUT_SIZE: 96,
    FUSE_COMPAT_STATFS_SIZE: 48,
    FUSE_COMPAT_WRITE_IN_SIZE: 24,
    FUSE_COMPAT_MKNOD_IN_SIZE: 8,
    FUSE_COMPAT_SETXATTR_IN_SIZE: 8,
  }

  constants.OPCODE_NAMES = Object.freeze({
    1: "LOOKUP", 2: "FORGET", 3: "GETATTR", 4: "SETATTR", 5: "READLINK", 6: "SYMLINK",
    8: "MKNOD", 9: "MKDIR", 10: "UNLINK", 11: "RMDIR", 12: "RENAME", 13: "LINK",
    14: "OPEN", 15: "READ", 16: "WRITE", 17: "STATFS", 18: "RELEASE", 20: "FSYNC",
    21: "SETXATTR", 22: "GETXATTR", 23: "LISTXATTR", 24: "REMOVEXATTR", 25: "FLUSH",
    26: "INIT", 27: "OPENDIR", 28: "READDIR", 29: "RELEASEDIR", 30: "FSYNCDIR",
    31: "GETLK", 32: "SETLK", 33: "SETLKW", 34: "ACCESS", 35: "CREATE", 36: "INTERRUPT",
    37: "BMAP", 38: "DESTROY", 39: "IOCTL", 40: "POLL", 41: "NOTIFY_REPLY",
    42: "BATCH_FORGET", 43: "FALLOCATE", 44: "READDIRPLUS", 45: "RENAME2", 46: "LSEEK",
    47: "COPY_FILE_RANGE", 48: "SETUPMAPPING", 49: "REMOVEMAPPING", 50: "SYNCFS",
    51: "TMPFILE", 52: "STATX", 4096: "CUSE_INIT",
  })
  constants.SUPPORTED_OPCODES = Object.freeze(invoke(binding.fuseSupportedOpcodes, binding, []))
  constants.UNIMPLEMENTED_OPCODES = Object.freeze(invoke(binding.fuseUnimplementedOpcodes, binding, []))
  constants.opcodeName = (opcode) => invoke(binding.fuseOpcodeName, binding, [opcode])
  constants.DEFAULT_WANTED_FLAGS =
    constants.FUSE_ASYNC_READ |
    constants.FUSE_ATOMIC_O_TRUNC |
    constants.FUSE_BIG_WRITES |
    constants.FUSE_AUTO_INVAL_DATA |
    constants.FUSE_DO_READDIRPLUS |
    constants.FUSE_READDIRPLUS_AUTO |
    constants.FUSE_ASYNC_DIO |
    constants.FUSE_PARALLEL_DIROPS |
    constants.FUSE_MAX_PAGES |
    constants.FUSE_SETXATTR_EXT
  constants.DEFAULT_MAX_WRITE = 1024 * 1024
  constants.DEFAULT_PROTOCOL = Object.freeze({ minor: 41, setxattrExt: false })

  Object.assign(binding, constants)
}

function install(binding) {
  if (!binding || typeof binding.fuseDecodeInHeader !== "function") return binding
  if (binding.__mountRsFuseCodecInstalled) return binding

  installConstants(binding)
  binding.ProtocolError = ProtocolError
  binding.isProtocolError = (error) => error instanceof ProtocolError
  binding.TranscriptError = TranscriptError

  const call = (name, args) => invoke(binding[name], binding, args)
  binding.encodeNotify = (code, body) => call("fuseEncodeNotify", [code, copyBytes(body)])
  binding.decodeNotify = (message) => call("fuseDecodeNotify", [copyBytes(message)])
  binding.encodeNotifyInvalInode = (value) => call("fuseEncodeNotifyInvalInode", [value])
  binding.decodeNotifyInvalInode = (body) => call("fuseDecodeNotifyInvalInode", [copyBytes(body)])
  binding.encodeNotifyInvalEntry = (value) => call("fuseEncodeNotifyInvalEntry", [value])
  binding.decodeNotifyInvalEntry = (body) => call("fuseDecodeNotifyInvalEntry", [copyBytes(body)])
  binding.FUSE_NAME_MAX = 1024

  binding.encodeTranscript = (frames) => call("fuseEncodeTranscript", [frames.map((frame) => ({
    direction: frame.direction,
    timestamp: BigInt.asUintN(64, frame.timestamp),
    bytes: copyBytes(frame.bytes),
  }))])
  binding.decodeTranscript = (bytes) => call("fuseDecodeTranscript", [copyBytes(bytes)])

  class TranscriptRecorder {
    #inner
    #now

    constructor(options = {}) {
      this.#inner = new binding.NativeFuseTranscriptRecorder(options.limit)
      this.#now = options.now ?? process.hrtime.bigint.bind(process.hrtime)
      this.tap = (direction, bytes) => {
        const now = this.#now()
        callInner("tapAt", [direction, copyBytes(bytes), BigInt.asUintN(64, now)])
      }
      const callInner = (name, args) => invoke(this.#inner[name], this.#inner, args)
    }

    get frames() { return invoke(this.#inner.__lookupGetter__("frames"), this.#inner, []) }
    get truncated() { return this.#inner.truncated }
    get bytes() { return this.#inner.bytes }
    encode() { return invoke(this.#inner.encode, this.#inner, []) }
  }
  binding.TranscriptRecorder = TranscriptRecorder

  // Keep the inode table native: the transport owns the identity, orphan,
  // hardlink, and subtree-remap invariants.  This small facade only restores
  // the oracle's object-shaped `Inode` view (`paths` is a Set) and accepts an
  // Inode object where the native bridge uses its nodeid.
  const NativeFuseInodeTable = binding.NativeFuseInodeTable
  if (typeof NativeFuseInodeTable === "function") {
    class InodeTable {
      #inner
      #options
      #views = new Map()
      #nodeids = new Set([1n])

      constructor(options = {}) {
        this.#options = options
        this.#inner = new NativeFuseInodeTable(options)
      }

      #nodeid(value) {
        return typeof value === "bigint" ? value : value.nodeid
      }

      #view(raw) {
        if (raw == null) return undefined
        const nodeid = raw.nodeid
        let view = this.#views.get(nodeid)
        if (view == null) {
          view = { nodeid, key: undefined, nlookup: 0n, paths: new Set() }
          this.#views.set(nodeid, view)
        }
        view.key = raw.key ?? undefined
        view.nlookup = raw.nlookup
        view.paths.clear()
        for (const path of raw.paths) view.paths.add(path)
        this.#nodeids.add(nodeid)
        return view
      }

      #sync() {
        const nativeNodeids = new Set(this.#inner.nodeids())
        for (const nodeid of this.#nodeids) {
          if (!nativeNodeids.has(nodeid)) {
            const view = this.#views.get(nodeid)
            if (view != null) {
              view.nlookup = 0n
              view.paths.clear()
            }
            this.#nodeids.delete(nodeid)
          }
        }
        for (const nodeid of nativeNodeids) {
          const raw = this.#inner.get(nodeid)
          if (raw == null) {
            const view = this.#views.get(nodeid)
            if (view != null) {
              view.nlookup = 0n
              view.paths.clear()
            }
            this.#nodeids.delete(nodeid)
          } else {
            this.#view(raw)
          }
        }
      }

      get root() {
        return this.#view(this.#inner.root)
      }

      get size() {
        this.#sync()
        return this.#nodeids.size
      }

      get pathCount() {
        this.#sync()
        let count = 0
        for (const nodeid of this.#nodeids) count += this.#views.get(nodeid)?.paths.size ?? 0
        return count
      }

      get(nodeid) {
        return this.#view(this.#inner.get(nodeid))
      }

      at(path) {
        return this.#view(this.#inner.at(path))
      }

      require(nodeid) {
        return this.#view(this.#inner.require(nodeid))
      }

      pathOf(inode) {
        return this.#inner.pathOf(this.#nodeid(inode))
      }

      requirePath(nodeid) {
        return this.#inner.requirePath(nodeid)
      }

      bind(path, stats) {
        const result = this.#view(this.#inner.bind(path, { dev: stats.dev, ino: stats.ino }))
        this.#sync()
        return result
      }

      acquire(inode) {
        const result = this.#view(this.#inner.acquire(this.#nodeid(inode)))
        this.#sync()
        return result
      }

      forget(nodeid, count) {
        const removed = this.#inner.forget(nodeid, count)
        this.#sync()
        return removed
      }

      unbind(path) {
        const result = this.#view(this.#inner.unbind(path))
        this.#sync()
        return result
      }

      remap(from, to) {
        this.#inner.remap(from, to)
        this.#sync()
      }

      nodeids() {
        this.#sync()
        return this.#inner.nodeids()
      }

      clear() {
        this.#inner = new NativeFuseInodeTable(this.#options)
        this.#views.clear()
        this.#nodeids = new Set([1n])
      }
    }
    binding.InodeTable = InodeTable
    binding.INODE_GENERATION = 0n
  }

  binding.decodeInHeader = (bytes) => call("fuseDecodeInHeader", [copyBytes(bytes)])
  binding.encodeInHeader = (value) => call("fuseEncodeInHeader", [value])
  binding.decodeOutHeader = (bytes) => call("fuseDecodeOutHeader", [copyBytes(bytes)])
  binding.encodeOutHeader = (value) => call("fuseEncodeOutHeader", [value])
  binding.writeOutHeaderInto = (target, value) => {
    if (target.length < 16) throw new ProtocolError(`fuse_out_header needs 16 bytes, target holds ${target.length}`)
    const view = new DataView(target.buffer, target.byteOffset, target.byteLength)
    view.setUint32(0, value.len >>> 0, true)
    view.setInt32(4, value.error | 0, true)
    view.setBigUint64(8, BigInt.asUintN(64, value.unique), true)
  }
  binding.fuseErrno = (code) => {
    if (typeof code === "number") return call("fuseFuseErrno", [code])
    const errno = binding.ERRNO_CODES?.[code]
    if (errno === undefined) throw new ProtocolError(`unknown errno code ${String(code)}`)
    return -errno
  }
  binding.allocReply = (size) => {
    if (!Number.isSafeInteger(size) || size < 0) {
      throw new ProtocolError(`reply body size must be a non-negative integer, got ${size}`)
    }
    const message = Buffer.alloc(16 + size)
    return { message, body: message.subarray(16) }
  }
  binding.finishReply = (reply, unique, bytesUsed = reply.body.length) => {
    if (!Number.isSafeInteger(bytesUsed) || bytesUsed < 0 || bytesUsed > reply.body.length) {
      throw new ProtocolError(`reply used ${bytesUsed} of a ${reply.body.length}-byte body`)
    }
    binding.writeOutHeaderInto(reply.message, { len: 16 + bytesUsed, error: 0, unique })
    return reply.message.subarray(0, 16 + bytesUsed)
  }
  binding.encodeReply = (unique, body) => call("fuseEncodeReply", [unique, body === undefined ? undefined : copyBytes(body)])
  binding.encodeErrorReply = (unique, code) => call("fuseEncodeErrorReply", [unique, binding.fuseErrno(code)])
  binding.encodeErrorReplyFor = (unique, error) => binding.encodeErrorReply(unique, -binding.errnoOf(error))

  binding.attrSize = (minor) => call("fuseAttrSize", [minor])
  binding.entryOutSize = (minor) => call("fuseEntryOutSize", [minor])
  binding.attrOutSize = (minor) => call("fuseAttrOutSize", [minor])
  binding.kstatfsSize = (minor) => call("fuseKstatfsSize", [minor])
  binding.initOutSize = (minor) => call("fuseInitOutSize", [minor])
  binding.readWriteInSize = (minor) => call("fuseReadWriteInSize", [minor])
  binding.joinInitFlags = (flags, flags2) => call("fuseJoinInitFlags", [flags, flags2])
  binding.splitInitFlags = (flags) => call("fuseSplitInitFlags", [flags])
  binding.direntAlign = (size) => call("fuseDirentAlign", [size])
  binding.direntSize = (size) => call("fuseDirentSize", [size])
  binding.direntPlusSize = (size, ctx) => call("fuseDirentPlusSize", [size, context(ctx)])
  binding.direntType = (mode) => call("fuseDirentType", [mode])

  // `READDIR` bodies are variable-length records rather than one of the
  // fixed-size structs owned by the Rust codec. Keep these small body codecs
  // in the public postlude so they follow the pinned oracle without adding a
  // second native allocation layer. `fuse_entry_out` itself is still encoded
  // and decoded by the already-tested native protocol implementation.
  const direntName = (name) => {
    if (name.includes("\0")) throw new ProtocolError("dirent name contains a NUL byte")
    return Buffer.from(name)
  }
  const direntBody = (value, what) => {
    const body = Buffer.from(value)
    const read = (offset, size) => {
      if (offset + size > body.length) {
        throw new ProtocolError(
          `truncated ${what}: need ${size} byte(s) at offset ${offset}, have ${body.length - offset}`,
          { offset },
        )
      }
    }
    return { body, read }
  }
  binding.packDirents = (entries, maxSize) => {
    const limit = Math.max(0, Math.trunc(maxSize))
    const chunks = []
    let used = 0
    for (const dirent of entries) {
      const name = direntName(dirent.name)
      const total = binding.direntSize(name.length)
      if (used + total > limit) break
      const chunk = Buffer.alloc(total)
      chunk.writeBigUInt64LE(BigInt.asUintN(64, dirent.ino), 0)
      chunk.writeBigUInt64LE(BigInt.asUintN(64, dirent.off), 8)
      chunk.writeUInt32LE(name.length >>> 0, 16)
      chunk.writeUInt32LE(dirent.type >>> 0, 20)
      name.copy(chunk, 24)
      chunks.push(chunk)
      used += total
    }
    return { buffer: Buffer.concat(chunks, used), packed: chunks.length }
  }
  binding.unpackDirents = (value) => {
    const { body, read } = direntBody(value, "fuse_dirent")
    const entries = []
    let offset = 0
    while (offset < body.length) {
      const start = offset
      read(offset, 24)
      const ino = body.readBigUInt64LE(offset)
      const off = body.readBigUInt64LE(offset + 8)
      const namelen = body.readUInt32LE(offset + 16)
      const type = body.readUInt32LE(offset + 20)
      offset += 24
      if (namelen > body.length - offset) {
        throw new ProtocolError(
          `fuse_dirent.namelen is ${namelen} but only ${body.length - offset} byte(s) remain`,
          { offset },
        )
      }
      const name = body.toString("utf8", offset, offset + namelen)
      offset += namelen
      const padded = start + binding.direntAlign(offset - start)
      if (padded > body.length) {
        throw new ProtocolError("fuse_dirent padding runs past the end of the buffer", {
          offset,
        })
      }
      offset = padded
      entries.push({ ino, off, type, name })
    }
    return entries
  }

  const plusContext = (value) => value == null ? undefined : context(value)
  const plusEntryFieldSizes = (minor) => minor >= 9
    ? [8, 8, 8, 8, 4, 4, 8, 8, 8, 8, 8, 8, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4]
    : [8, 8, 8, 8, 4, 4, 8, 8, 8, 8, 8, 8, 4, 4, 4, 4, 4, 4, 4, 4]
  const plusEntrySize = (ctx) => binding.entryOutSize(
    (ctx ?? binding.DEFAULT_PROTOCOL).minor,
  )
  const plusEntryTruncation = (body, start, entrySize, minor) => {
    const available = body.length - start
    if (available >= entrySize) return
    let relative = 0
    for (const size of plusEntryFieldSizes(minor)) {
      if (relative + size > available) {
        const offset = start + relative
        throw new ProtocolError(
          `truncated fuse_direntplus: need ${size} byte(s) at offset ${offset}, have ${body.length - offset}`,
          { offset },
        )
      }
      relative += size
    }
    // The field list is kept in lockstep with fuse_entry_out. This is an
    // internal invariant, but retain the public error type if it ever drifts.
    throw new ProtocolError("truncated fuse_direntplus: incomplete fuse_entry_out", {
      offset: start + relative,
    })
  }
  binding.packDirentsPlus = (entries, maxSize, ctx) => {
    const normalized = plusContext(ctx)
    const entrySize = plusEntrySize(normalized)
    const limit = Math.max(0, Math.trunc(maxSize))
    const chunks = []
    let used = 0
    for (const value of entries) {
      if (value.entry === undefined) {
        throw new ProtocolError("fuse_direntplus needs a fuse_entry_out")
      }
      const dirent = value.dirent
      const name = direntName(dirent.name)
      const total = binding.direntAlign(entrySize + 24 + name.length)
      if (used + total > limit) break
      const chunk = Buffer.alloc(total)
      const encodedEntry = binding.encodeEntryOut(value.entry, normalized)
      encodedEntry.copy(chunk, 0)
      const at = entrySize
      chunk.writeBigUInt64LE(BigInt.asUintN(64, dirent.ino), at)
      chunk.writeBigUInt64LE(BigInt.asUintN(64, dirent.off), at + 8)
      chunk.writeUInt32LE(name.length >>> 0, at + 16)
      chunk.writeUInt32LE(dirent.type >>> 0, at + 20)
      name.copy(chunk, at + 24)
      chunks.push(chunk)
      used += total
    }
    return { buffer: Buffer.concat(chunks, used), packed: chunks.length }
  }
  binding.unpackDirentsPlus = (value, ctx) => {
    const { body, read } = direntBody(value, "fuse_direntplus")
    const normalized = plusContext(ctx)
    const minor = (normalized ?? binding.DEFAULT_PROTOCOL).minor
    const entrySize = plusEntrySize(normalized)
    const entries = []
    let offset = 0
    while (offset < body.length) {
      const start = offset
      plusEntryTruncation(body, start, entrySize, minor)
      const entry = binding.decodeEntryOut(
        body.subarray(start, start + entrySize),
        normalized,
      )
      offset += entrySize

      const inoAt = offset
      read(inoAt, 8)
      const ino = body.readBigUInt64LE(inoAt)
      offset += 8
      const offAt = offset
      read(offAt, 8)
      const off = body.readBigUInt64LE(offAt)
      offset += 8
      const nameLengthAt = offset
      read(nameLengthAt, 4)
      const namelen = body.readUInt32LE(nameLengthAt)
      offset += 4
      const typeAt = offset
      read(typeAt, 4)
      const type = body.readUInt32LE(typeAt)
      offset += 4
      if (namelen > body.length - offset) {
        throw new ProtocolError(
          `fuse_dirent.namelen is ${namelen} but only ${body.length - offset} byte(s) remain`,
          { offset },
        )
      }
      const name = body.toString("utf8", offset, offset + namelen)
      offset += namelen
      const padded = start + binding.direntAlign(offset - start)
      if (padded > body.length) {
        throw new ProtocolError("fuse_direntplus padding runs past the end of the buffer", {
          offset,
        })
      }
      offset = padded
      entries.push({ entry, dirent: { ino, off, type, name } })
    }
    return entries
  }

  for (const [publicName, nativeName] of [
    ["decodeEntryOut", "fuseDecodeEntryOut"], ["encodeEntryOut", "fuseEncodeEntryOut"],
    ["decodeAttrOut", "fuseDecodeAttrOut"], ["encodeAttrOut", "fuseEncodeAttrOut"],
    ["decodeGetattrIn", "fuseDecodeGetattrIn"], ["encodeGetattrIn", "fuseEncodeGetattrIn"],
    ["decodeGetattrOut", "fuseDecodeGetattrOut"], ["encodeGetattrOut", "fuseEncodeGetattrOut"],
    ["decodeSetattrIn", "fuseDecodeSetattrIn"], ["encodeSetattrIn", "fuseEncodeSetattrIn"],
    ["decodeSetattrOut", "fuseDecodeSetattrOut"], ["encodeSetattrOut", "fuseEncodeSetattrOut"],
    ["decodeOpenOut", "fuseDecodeOpenOut"], ["encodeOpenOut", "fuseEncodeOpenOut"],
    ["decodeReadIn", "fuseDecodeReadIn"], ["encodeReadIn", "fuseEncodeReadIn"],
    ["decodeReadOut", "fuseDecodeReadOut"], ["encodeReadOut", "fuseEncodeReadOut"],
    ["decodeWriteIn", "fuseDecodeWriteIn"], ["encodeWriteIn", "fuseEncodeWriteIn"],
    ["decodeWriteOut", "fuseDecodeWriteOut"], ["encodeWriteOut", "fuseEncodeWriteOut"],
    ["decodeInitIn", "fuseDecodeInitIn"], ["encodeInitIn", "fuseEncodeInitIn"],
    ["decodeInitOut", "fuseDecodeInitOut"], ["encodeInitOut", "fuseEncodeInitOut"],
    ["decodeStatfsOut", "fuseDecodeStatfsOut"], ["encodeStatfsOut", "fuseEncodeStatfsOut"],
    ["decodeGetxattrOut", "fuseDecodeGetxattrOut"], ["encodeGetxattrOut", "fuseEncodeGetxattrOut"],
    ["encodeXattrNames", "fuseEncodeXattrNames"], ["decodeXattrNames", "fuseDecodeXattrNames"],
  ]) {
    binding[publicName] = (...args) => {
      if (args.length > 1) args[1] = context(args[1])
      if (publicName === "encodeWriteIn" && args[0] !== undefined) {
        args[0] = { ...args[0], data: copyBytes(args[0].data) }
      }
      if (publicName === "encodeReadOut" && args[0] !== undefined) {
        args[0] = { ...args[0], data: copyBytes(args[0].data) }
      }
      if (args[0] && args[0].buffer !== undefined && typeof args[0] !== "object") args[0] = copyBytes(args[0])
      try {
        return binding[nativeName](...args)
      } catch (error) {
        throw revive(error)
      }
    }
  }

  // These helpers are pure JS in the oracle and do not touch the native mount.
  binding.translateOpenFlags = (wire, host) => {
    let flags = wire & binding.O_ACCMODE
    if ((wire & binding.O_CREAT) !== 0) flags |= host.O_CREAT
    if ((wire & binding.O_EXCL) !== 0) flags |= host.O_EXCL
    if ((wire & binding.O_TRUNC) !== 0) flags |= host.O_TRUNC
    if ((wire & binding.O_APPEND) !== 0) flags |= host.O_APPEND
    return flags
  }
  binding.driverOpenFlags = (wire, platform = process.platform, host = require("node:fs").constants) =>
    platform === "linux" ? wire : binding.translateOpenFlags(wire, host)
  binding.reopenFlags = (flags) => flags & ~(require("node:fs").constants.O_CREAT | require("node:fs").constants.O_EXCL | require("node:fs").constants.O_TRUNC)
  binding.nameByteLength = (name) => Buffer.byteLength(name)

  Object.defineProperty(binding, "__mountRsFuseCodecInstalled", { value: true })
  return binding
}

module.exports = install
