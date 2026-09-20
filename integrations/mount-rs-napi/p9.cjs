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
