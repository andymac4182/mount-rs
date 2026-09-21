"use strict"

// The package's ./9p entrypoint is deliberately a shallow facade over the
// normal binding.  Installing the codec aliases on a copy keeps the root
// namespace (and its NFS codec/server names) unchanged while retaining the
// existing P9 server exports.
const root = require("./index.js")
const binding = { ...root }

require("./postlude-p9-codec.cjs")(binding)

module.exports = binding

// Preserve statically discoverable named exports for Node's ESM interop. Keep
// the shallow-copy facade above so existing root/server exports remain
// available, but spell every public codec alias as a direct assignment: Node's
// CommonJS named-export analysis cannot discover names created by a loop.
module.exports.P9Error = binding.P9Error
module.exports.isP9Error = binding.isP9Error
module.exports.P9Reader = binding.P9Reader
module.exports.P9Writer = binding.P9Writer
module.exports.P9FrameAssembler = binding.P9FrameAssembler
module.exports.P9DirentPacker = binding.P9DirentPacker
module.exports.P9_QID_SIZE = binding.P9_QID_SIZE
module.exports.P9_MAX_STRING = binding.P9_MAX_STRING
module.exports.P9_MAX_ITEM = binding.P9_MAX_ITEM
module.exports.P9_DEFAULT_MAX_FRAME = binding.P9_DEFAULT_MAX_FRAME
module.exports.encodeP9 = binding.encodeP9
module.exports.decodeP9 = binding.decodeP9
module.exports.readHeader = binding.readHeader
module.exports.writeHeader = binding.writeHeader
module.exports.readTime = binding.readTime
module.exports.writeTime = binding.writeTime
module.exports.encodeMessage = binding.encodeMessage
module.exports.decodeMessage = binding.decodeMessage
module.exports.decodeMessageAs = binding.decodeMessageAs
module.exports.readEmptyBody = binding.readEmptyBody
module.exports.EMPTY_BODY = binding.EMPTY_BODY
module.exports.stringByteLength = binding.stringByteLength
module.exports.direntSize = binding.direntSize
module.exports.readDirents = binding.readDirents
module.exports.framesFrom = binding.framesFrom

module.exports.writeTversion = binding.writeTversion
module.exports.readTversion = binding.readTversion
module.exports.writeRversion = binding.writeRversion
module.exports.readRversion = binding.readRversion
module.exports.writeTauth = binding.writeTauth
module.exports.readTauth = binding.readTauth
module.exports.writeRauth = binding.writeRauth
module.exports.readRauth = binding.readRauth
module.exports.writeTattach = binding.writeTattach
module.exports.readTattach = binding.readTattach
module.exports.writeRattach = binding.writeRattach
module.exports.readRattach = binding.readRattach
module.exports.writeRlerror = binding.writeRlerror
module.exports.readRlerror = binding.readRlerror
module.exports.writeTflush = binding.writeTflush
module.exports.readTflush = binding.readTflush
module.exports.writeTwalk = binding.writeTwalk
module.exports.readTwalk = binding.readTwalk
module.exports.writeRwalk = binding.writeRwalk
module.exports.readRwalk = binding.readRwalk
module.exports.writeTread = binding.writeTread
module.exports.readTread = binding.readTread
module.exports.writeRread = binding.writeRread
module.exports.readRread = binding.readRread
module.exports.writeTwrite = binding.writeTwrite
module.exports.readTwrite = binding.readTwrite
module.exports.writeRwrite = binding.writeRwrite
module.exports.readRwrite = binding.readRwrite
module.exports.writeFidRequest = binding.writeFidRequest
module.exports.readFidRequest = binding.readFidRequest
module.exports.writeRstatfs = binding.writeRstatfs
module.exports.readRstatfs = binding.readRstatfs
module.exports.writeTlopen = binding.writeTlopen
module.exports.readTlopen = binding.readTlopen
module.exports.writeRlopen = binding.writeRlopen
module.exports.readRlopen = binding.readRlopen
module.exports.writeTlcreate = binding.writeTlcreate
module.exports.readTlcreate = binding.readTlcreate
module.exports.writeTsymlink = binding.writeTsymlink
module.exports.readTsymlink = binding.readTsymlink
module.exports.writeQidReply = binding.writeQidReply
module.exports.readQidReply = binding.readQidReply
module.exports.writeTmknod = binding.writeTmknod
module.exports.readTmknod = binding.readTmknod
module.exports.writeTmkdir = binding.writeTmkdir
module.exports.readTmkdir = binding.readTmkdir
module.exports.writeTrename = binding.writeTrename
module.exports.readTrename = binding.readTrename
module.exports.writeTrenameat = binding.writeTrenameat
module.exports.readTrenameat = binding.readTrenameat
module.exports.writeTunlinkat = binding.writeTunlinkat
module.exports.readTunlinkat = binding.readTunlinkat
module.exports.writeTlink = binding.writeTlink
module.exports.readTlink = binding.readTlink
module.exports.writeRreadlink = binding.writeRreadlink
module.exports.readRreadlink = binding.readRreadlink
module.exports.writeTgetattr = binding.writeTgetattr
module.exports.readTgetattr = binding.readTgetattr
module.exports.writeRgetattr = binding.writeRgetattr
module.exports.readRgetattr = binding.readRgetattr
module.exports.writeTsetattr = binding.writeTsetattr
module.exports.readTsetattr = binding.readTsetattr
module.exports.writeTxattrwalk = binding.writeTxattrwalk
module.exports.readTxattrwalk = binding.readTxattrwalk
module.exports.writeRxattrwalk = binding.writeRxattrwalk
module.exports.readRxattrwalk = binding.readRxattrwalk
module.exports.writeTxattrcreate = binding.writeTxattrcreate
module.exports.readTxattrcreate = binding.readTxattrcreate
module.exports.writeTreaddir = binding.writeTreaddir
module.exports.readTreaddir = binding.readTreaddir
module.exports.writeRreaddir = binding.writeRreaddir
module.exports.readRreaddir = binding.readRreaddir
module.exports.writeDirent = binding.writeDirent
module.exports.readDirent = binding.readDirent
module.exports.writeTfsync = binding.writeTfsync
module.exports.readTfsync = binding.readTfsync
module.exports.writeTlock = binding.writeTlock
module.exports.readTlock = binding.readTlock
module.exports.writeRlock = binding.writeRlock
module.exports.readRlock = binding.readRlock
module.exports.writeTgetlock = binding.writeTgetlock
module.exports.readTgetlock = binding.readTgetlock
module.exports.writeRgetlock = binding.writeRgetlock
module.exports.readRgetlock = binding.readRgetlock
module.exports.writeTclunk = binding.writeTclunk
module.exports.readTclunk = binding.readTclunk
module.exports.writeTremove = binding.writeTremove
module.exports.readTremove = binding.readTremove
module.exports.writeTstatfs = binding.writeTstatfs
module.exports.readTstatfs = binding.readTstatfs
module.exports.writeTreadlink = binding.writeTreadlink
module.exports.readTreadlink = binding.readTreadlink
module.exports.writeRsymlink = binding.writeRsymlink
module.exports.readRsymlink = binding.readRsymlink
module.exports.writeRmknod = binding.writeRmknod
module.exports.readRmknod = binding.readRmknod
module.exports.writeRmkdir = binding.writeRmkdir
module.exports.readRmkdir = binding.readRmkdir
module.exports.writeRlcreate = binding.writeRlcreate
module.exports.readRlcreate = binding.readRlcreate

module.exports.createP9Server = binding.createP9Server
module.exports.P9Server = binding.P9Server
module.exports.P9Connection = binding.P9Connection
module.exports.P9Session = binding.P9Session
module.exports.P9LockClient = binding.P9LockClient
module.exports.P9LockTable = binding.P9LockTable
module.exports.P9_LOCK_TYPE_RDLCK = 0
module.exports.P9_LOCK_TYPE_WRLCK = 1
module.exports.P9_LOCK_TYPE_UNLCK = 2
module.exports.P9_LOCK_SUCCESS = 0
module.exports.P9_LOCK_BLOCKED = 1
module.exports.P9_LOCK_ERROR = 2
module.exports.P9_LOCK_GRACE = 3
module.exports.P9_LOCK_FLAGS_BLOCK = 1
module.exports.P9_LOCK_FLAGS_RECLAIM = 2

// Fid-table classes are exported under both their native names and the
// upstream 9P facade names. Direct assignments keep them visible to CommonJS
// and ESM named-export discovery.
module.exports.P9Fid = binding.P9Fid
module.exports.P9FidOpenState = binding.P9FidOpenState
module.exports.P9FidTable = binding.P9FidTable
module.exports.P9OpenHandle = binding.P9OpenHandle
module.exports.FidTable = binding.P9FidTable

module.exports.FIRST_QID_PATH = 1n

function p9WalkError(message) {
  if (typeof binding.fsError === "function") {
    return binding.fsError("EINVAL", { message })
  }
  const error = new Error(message)
  error.code = "EINVAL"
  return error
}

function p9NormalizePath(value) {
  const parts = []
  for (const segment of String(value).split("/")) {
    if (!segment || segment === ".") continue
    if (segment === "..") {
      if (parts.length) parts.pop()
      continue
    }
    parts.push(segment)
  }
  return `/${parts.join("/")}`
}

function p9QidType(mode) {
  switch (Number(mode) & 0o170000) {
    case 0o040000:
      return module.exports.P9_QTDIR
    case 0o120000:
      return module.exports.P9_QTSYMLINK
    default:
      return module.exports.P9_QTFILE
  }
}

function p9QidVersion(stats) {
  const mtimeMs = Number(stats?.mtimeMs)
  if (!Number.isFinite(mtimeMs) || mtimeMs <= 0) return 0
  return Math.trunc(mtimeMs) >>> 0
}

function p9WalkStep(path, name) {
  const element = String(name)
  if (!element) throw p9WalkError("EINVAL: walk element is empty")
  if (element.includes("/")) {
    throw p9WalkError(`EINVAL: walk element ${JSON.stringify(element)} contains a separator`)
  }
  if (element.includes("\0")) throw p9WalkError("EINVAL: walk element contains a NUL")
  return p9NormalizePath(`${p9NormalizePath(path)}/${element}`)
}

module.exports.qidType = p9QidType
module.exports.qidVersion = p9QidVersion
module.exports.walkStep = p9WalkStep

function p9DecodeErrorField(value) {
  if (value === "-") return undefined
  try {
    return Buffer.from(value, "hex").toString("utf8")
  } catch {
    return value
  }
}

function p9ReviveError(error) {
  const message = String(error?.message ?? error)
  const fields = message.split("|")
  if (fields[0] !== "__mount_rs_error_v1__" || fields.length !== 7) return error
  const [, code, errno, syscall, path, dest, encodedMessage] = fields
  const options = { message: p9DecodeErrorField(encodedMessage) }
  const decodedSyscall = p9DecodeErrorField(syscall)
  const decodedPath = p9DecodeErrorField(path)
  const decodedDest = p9DecodeErrorField(dest)
  if (decodedSyscall !== undefined) options.syscall = decodedSyscall
  if (decodedPath !== undefined) options.path = decodedPath
  if (decodedDest !== undefined) options.dest = decodedDest
  if (typeof binding.fsError === "function") return binding.fsError(code, options)
  const revived = new Error(options.message)
  revived.code = code
  if (errno !== "-") revived.errno = Number(errno)
  return revived
}

function p9Invoke(native, receiver, args) {
  try {
    return native.apply(receiver, args)
  } catch (error) {
    throw p9ReviveError(error)
  }
}

// N-API uses `null` for Rust `Option<T>`. The upstream 9P objects use
// `undefined` for absent optional fields, so normalize the fid facade at this
// boundary while leaving the native binding's generated surface intact.
for (const [ctor, properties] of [
  [binding.P9Fid, ["open", "cursor"]],
  [binding.P9FidOpenState, ["handle", "qid"]],
]) {
  if (!ctor?.prototype) continue
  for (const property of properties) {
    const descriptor = Object.getOwnPropertyDescriptor(ctor.prototype, property)
    if (!descriptor?.get) continue
    const native = descriptor.get
    Object.defineProperty(ctor.prototype, property, {
      ...descriptor,
      get() {
        const value = p9Invoke(native, this, [])
        return value === null ? undefined : value
      },
      ...(descriptor.set
        ? {
            set(value) {
              if (
                ctor === binding.P9Fid &&
                property === "open" &&
                value !== undefined &&
                value !== null &&
                !(value instanceof binding.P9FidOpenState)
              ) {
                value = new binding.P9FidOpenState(
                  value.flags,
                  value.handle,
                  value.directory,
                  value.qid,
                )
              }
              return p9Invoke(descriptor.set, this, [value])
            },
          }
        : {}),
    })
  }
}

const nativeFidGet = binding.P9FidTable?.prototype?.get
if (typeof nativeFidGet === "function") {
  binding.P9FidTable.prototype.get = function getFid(fid) {
    const value = p9Invoke(nativeFidGet, this, [fid])
    return value === null ? undefined : value
  }
}

const nativeFidResume = binding.P9FidTable?.prototype?.resume
if (typeof nativeFidResume === "function") {
  binding.P9FidTable.prototype.resume = function resumeFid(entry, offset) {
    const value = p9Invoke(nativeFidResume, this, [entry, offset])
    return value === null ? undefined : value
  }
}

for (const name of [
  "require",
  "create",
  "clone",
  "clunk",
  "snapshot",
  "noteOffset",
  "qidFor",
  "qidPathFor",
  "release",
  "remap",
  "fids",
  "entries",
  "openHandles",
  "clear",
]) {
  const native = binding.P9FidTable?.prototype?.[name]
  if (typeof native !== "function") continue
  Object.defineProperty(binding.P9FidTable.prototype, name, {
    configurable: true,
    enumerable: false,
    writable: true,
    value(...args) {
      return p9Invoke(native, this, args)
    },
  })
}

module.exports.P9_TLERROR = 6
module.exports.P9_RLERROR = 7
module.exports.P9_TSTATFS = 8
module.exports.P9_RSTATFS = 9
module.exports.P9_TLOPEN = 12
module.exports.P9_RLOPEN = 13
module.exports.P9_TLCREATE = 14
module.exports.P9_RLCREATE = 15
module.exports.P9_TSYMLINK = 16
module.exports.P9_RSYMLINK = 17
module.exports.P9_TMKNOD = 18
module.exports.P9_RMKNOD = 19
module.exports.P9_TRENAME = 20
module.exports.P9_RRENAME = 21
module.exports.P9_TREADLINK = 22
module.exports.P9_RREADLINK = 23
module.exports.P9_TGETATTR = 24
module.exports.P9_RGETATTR = 25
module.exports.P9_TSETATTR = 26
module.exports.P9_RSETATTR = 27
module.exports.P9_TXATTRWALK = 30
module.exports.P9_RXATTRWALK = 31
module.exports.P9_TXATTRCREATE = 32
module.exports.P9_RXATTRCREATE = 33
module.exports.P9_TREADDIR = 40
module.exports.P9_RREADDIR = 41
module.exports.P9_TFSYNC = 50
module.exports.P9_RFSYNC = 51
module.exports.P9_TLOCK = 52
module.exports.P9_RLOCK = 53
module.exports.P9_TGETLOCK = 54
module.exports.P9_RGETLOCK = 55
module.exports.P9_TLINK = 70
module.exports.P9_RLINK = 71
module.exports.P9_TMKDIR = 72
module.exports.P9_RMKDIR = 73
module.exports.P9_TRENAMEAT = 74
module.exports.P9_RRENAMEAT = 75
module.exports.P9_TUNLINKAT = 76
module.exports.P9_RUNLINKAT = 77
module.exports.P9_TVERSION = 100
module.exports.P9_RVERSION = 101
module.exports.P9_TAUTH = 102
module.exports.P9_RAUTH = 103
module.exports.P9_TATTACH = 104
module.exports.P9_RATTACH = 105
module.exports.P9_TERROR = 106
module.exports.P9_RERROR = 107
module.exports.P9_TFLUSH = 108
module.exports.P9_RFLUSH = 109
module.exports.P9_TWALK = 110
module.exports.P9_RWALK = 111
module.exports.P9_TOPEN = 112
module.exports.P9_ROPEN = 113
module.exports.P9_TCREATE = 114
module.exports.P9_RCREATE = 115
module.exports.P9_TREAD = 116
module.exports.P9_RREAD = 117
module.exports.P9_TWRITE = 118
module.exports.P9_RWRITE = 119
module.exports.P9_TCLUNK = 120
module.exports.P9_RCLUNK = 121
module.exports.P9_TREMOVE = 122
module.exports.P9_RREMOVE = 123
module.exports.P9_TSTAT = 124
module.exports.P9_RSTAT = 125
module.exports.P9_TWSTAT = 126
module.exports.P9_RWSTAT = 127

module.exports.MESSAGE_NAMES = Object.freeze({
  6: "Tlerror",
  7: "Rlerror",
  8: "Tstatfs",
  9: "Rstatfs",
  12: "Tlopen",
  13: "Rlopen",
  14: "Tlcreate",
  15: "Rlcreate",
  16: "Tsymlink",
  17: "Rsymlink",
  18: "Tmknod",
  19: "Rmknod",
  20: "Trename",
  21: "Rrename",
  22: "Treadlink",
  23: "Rreadlink",
  24: "Tgetattr",
  25: "Rgetattr",
  26: "Tsetattr",
  27: "Rsetattr",
  30: "Txattrwalk",
  31: "Rxattrwalk",
  32: "Txattrcreate",
  33: "Rxattrcreate",
  40: "Treaddir",
  41: "Rreaddir",
  50: "Tfsync",
  51: "Rfsync",
  52: "Tlock",
  53: "Rlock",
  54: "Tgetlock",
  55: "Rgetlock",
  70: "Tlink",
  71: "Rlink",
  72: "Tmkdir",
  73: "Rmkdir",
  74: "Trenameat",
  75: "Rrenameat",
  76: "Tunlinkat",
  77: "Runlinkat",
  100: "Tversion",
  101: "Rversion",
  102: "Tauth",
  103: "Rauth",
  104: "Tattach",
  105: "Rattach",
  106: "Terror",
  107: "Rerror",
  108: "Tflush",
  109: "Rflush",
  110: "Twalk",
  111: "Rwalk",
  112: "Topen",
  113: "Ropen",
  114: "Tcreate",
  115: "Rcreate",
  116: "Tread",
  117: "Rread",
  118: "Twrite",
  119: "Rwrite",
  120: "Tclunk",
  121: "Rclunk",
  122: "Tremove",
  123: "Rremove",
  124: "Tstat",
  125: "Rstat",
  126: "Twstat",
  127: "Rwstat",
})
function p9MessageName(type) {
  return module.exports.MESSAGE_NAMES[type] ?? `UNKNOWN(${type})`
}
module.exports.messageName = p9MessageName

module.exports.P9_GETATTR_MODE = 0x00000001n
module.exports.P9_GETATTR_NLINK = 0x00000002n
module.exports.P9_GETATTR_UID = 0x00000004n
module.exports.P9_GETATTR_GID = 0x00000008n
module.exports.P9_GETATTR_RDEV = 0x00000010n
module.exports.P9_GETATTR_ATIME = 0x00000020n
module.exports.P9_GETATTR_MTIME = 0x00000040n
module.exports.P9_GETATTR_CTIME = 0x00000080n
module.exports.P9_GETATTR_INO = 0x00000100n
module.exports.P9_GETATTR_SIZE = 0x00000200n
module.exports.P9_GETATTR_BLOCKS = 0x00000400n
module.exports.P9_GETATTR_BTIME = 0x00000800n
module.exports.P9_GETATTR_GEN = 0x00001000n
module.exports.P9_GETATTR_DATA_VERSION = 0x00002000n
module.exports.P9_GETATTR_BASIC = 0x000007ffn
module.exports.P9_GETATTR_ALL = 0x00003fffn

module.exports.P9_SETATTR_MODE = 1 << 0
module.exports.P9_SETATTR_UID = 1 << 1
module.exports.P9_SETATTR_GID = 1 << 2
module.exports.P9_SETATTR_SIZE = 1 << 3
module.exports.P9_SETATTR_ATIME = 1 << 4
module.exports.P9_SETATTR_MTIME = 1 << 5
module.exports.P9_SETATTR_CTIME = 1 << 6
module.exports.P9_SETATTR_ATIME_SET = 1 << 7
module.exports.P9_SETATTR_MTIME_SET = 1 << 8

module.exports.P9_QTDIR = 0x80
module.exports.P9_QTAPPEND = 0x40
module.exports.P9_QTEXCL = 0x20
module.exports.P9_QTMOUNT = 0x10
module.exports.P9_QTAUTH = 0x08
module.exports.P9_QTTMP = 0x04
module.exports.P9_QTSYMLINK = 0x02
module.exports.P9_QTLINK = 0x01
module.exports.P9_QTFILE = 0x00

module.exports.P9_NOTAG = 0xffff
module.exports.P9_NOFID = 0xffffffff
module.exports.P9_MAXWELEM = 16
module.exports.P9_HDRSZ = 7
module.exports.P9_IOHDRSZ = 24
module.exports.P9_READDIRHDRSZ = 24
module.exports.P9_DOTL_AT_REMOVEDIR = 0x200
module.exports.P9_VERSION_DOTL = "9P2000.L"
module.exports.P9_VERSION_UNKNOWN = "unknown"
module.exports.P9_MIN_MSIZE = 4096
module.exports.V9FS_MAGIC = 0x01021997

// The native binding owns the host probe; the option-string and refusal
// helpers stay in this shallow facade so they remain usable and testable on
// non-Linux hosts without attempting a mount.
const nativeP9ClientProbe = binding.p9ClientProbe
const nativeP9Platform = binding.p9Platform
module.exports.p9ClientProbe = () => {
  const probe = nativeP9ClientProbe()
  if (probe.platform === null) probe.platform = undefined
  if (probe.reason === null) probe.reason = undefined
  return probe
}
module.exports.p9Platform = () => nativeP9Platform() ?? undefined
module.exports.P9_DEFAULT_MOUNT_MSIZE = 128 * 1024 + module.exports.P9_IOHDRSZ
module.exports.P9_MAX_MOUNT_MSIZE = 1024 * 1024
module.exports.P9_UNIX_PATH_MAX = 108

function socketPathRefusal(path) {
  const length = Buffer.byteLength(String(path), "utf8")
  if (length < module.exports.P9_UNIX_PATH_MAX) return undefined
  return `the 9P Unix socket path is ${length} bytes; Linux sockaddr_un allows at most ${module.exports.P9_UNIX_PATH_MAX - 1}`
}

function tcpSourceRefusal(host) {
  const value = String(host)
  const match = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(value)
  if (match && match.slice(1).every((octet) => Number(octet) <= 255)) return undefined
  return `a trans=tcp 9P mount needs a dotted-quad IPv4 source, got ${JSON.stringify(value)}`
}

function checkP9OptionValue(name, value) {
  const text = String(value)
  if (/[\s,]/.test(text)) {
    throw new TypeError(`9P mount option ${name} may not contain comma or whitespace`)
  }
  return text
}

function p9Msize(value) {
  if (value === undefined || Number.isNaN(value)) return module.exports.P9_DEFAULT_MOUNT_MSIZE
  return Math.min(Math.max(Math.trunc(Number(value)), module.exports.P9_MIN_MSIZE), module.exports.P9_MAX_MOUNT_MSIZE)
}

function p9Port(value) {
  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    throw new RangeError("TCP 9P mounts require an integer port in [1, 65535]")
  }
  return value
}

function p9MountOptions(target, options = {}) {
  const trans = target?.trans
  if (trans !== "unix" && trans !== "tcp") {
    throw new TypeError("9P mount target trans must be unix or tcp")
  }
  const parts = [`trans=${trans}`]
  if (trans === "tcp") parts.push(`port=${p9Port(target.port)}`)
  parts.push(
    "version=9p2000.L",
    `msize=${p9Msize(options.mountMsize)}`,
    `access=${checkP9OptionValue("access", options.access ?? "client")}`,
    `cache=${checkP9OptionValue("cache", options.cache ?? "none")}`,
    `uname=${checkP9OptionValue("uname", options.uname ?? "nobody")}`,
    `aname=${checkP9OptionValue("aname", options.aname ?? "/")}`,
  )
  if (options.readOnly === true) parts.push("ro")
  parts.push(...(options.mountOptions ?? []))
  return parts.join(",")
}

async function mount9p(driver, mountpoint, options = {}) {
  const server = options.server
  if (server !== undefined &&
      (server === null || typeof server.listen !== "function")) {
    throw new TypeError("9P mount server must be a P9Server")
  }
  // Avoid starting a user-supplied listener on hosts where the native client
  // cannot mount 9P, while still making the common configured-server sequence
  // adopt the exact bound listener.
  if (server !== undefined && module.exports.p9ClientProbe().usable) await server.listen()
  const trans = options.transport ?? ((options.port !== undefined || options.host !== undefined) ? "tcp" : "unix")
  const p9 = {
    transport: trans,
    host: options.host,
    port: options.port,
    path: options.path,
    allowRemote: options.allowRemote,
    socketMode: options.socketMode,
    allowSharedDirectory: options.allowSharedDirectory,
    maxFrame: options.maxFrame,
    maxInFlight: options.maxInFlight,
    msize: options.msize,
    mountMsize: options.mountMsize,
    access: options.access,
    cache: options.cache,
    uname: options.uname,
    aname: options.aname,
    readOnly: options.readOnly,
    useDriverIno: options.useDriverIno,
    claimOwnership: options.claimOwnership,
    debug: options.debug,
    locks: options.locks,
    mountOptions: options.mountOptions,
    unmountTimeoutMs: options.unmountTimeout,
    server,
  }
  return binding.mount(driver, mountpoint, {
    transport: "9p",
    readOnly: options.readOnly,
    unmountTimeoutMs: options.unmountTimeout,
    onTransportError: options.onTransportError,
    p9,
  })
}

async function live9pMounts() {
  return (await binding.liveMounts()).filter((mount) => mount.transport === "9p")
}

async function unmountAll9p() {
  return (await binding.unmountAll()).filter((failure) => failure.transport === "9p")
}

module.exports.socketPathRefusal = socketPathRefusal
module.exports.tcpSourceRefusal = tcpSourceRefusal
module.exports.p9MountOptions = p9MountOptions
module.exports.mount9p = mount9p
module.exports.live9pMounts = live9pMounts
module.exports.unmountAll9p = unmountAll9p
