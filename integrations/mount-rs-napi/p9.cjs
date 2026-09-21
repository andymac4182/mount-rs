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
