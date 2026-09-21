"use strict"

// The package export for ./nfs is intentionally narrower than the root
// facade.  Main owns package.json/postbuild wiring; this wrapper owns only the
// runtime selection for the XDR/RPC codec slice.
const rootBinding = require("./index.js")
// The codec postlude installs public aliases on the object it receives. Keep
// those aliases on this subpath facade instead of adding them to the root
// binding, where names such as stringByteLength can collide with another
// transport's exports.
const binding = require("./postlude-nfs-codec.cjs")({ ...rootBinding })

if (!binding || typeof binding.XdrReader !== "function") {
  throw new Error(
    "mount-rs NFS codec exports are unavailable; rebuild after registering src/nfs_codec.rs",
  )
}

module.exports = {}
module.exports.XdrError = binding.XdrError
module.exports.isXdrError = binding.isXdrError
module.exports.xdrPad = binding.xdrPad
module.exports.xdrAlign = binding.xdrAlign
module.exports.XDR_MAX_ITEM = binding.XDR_MAX_ITEM
module.exports.XdrReader = binding.XdrReader
module.exports.XdrWriter = binding.XdrWriter
module.exports.encodeXdr = binding.encodeXdr
module.exports.decodeXdr = binding.decodeXdr
module.exports.stringByteLength = binding.stringByteLength

module.exports.RPC_VERSION = binding.RPC_VERSION
module.exports.RPC_CALL = binding.RPC_CALL
module.exports.RPC_REPLY = binding.RPC_REPLY
module.exports.MSG_ACCEPTED = binding.MSG_ACCEPTED
module.exports.MSG_DENIED = binding.MSG_DENIED
module.exports.RPC_SUCCESS = binding.RPC_SUCCESS
module.exports.RPC_PROG_UNAVAIL = binding.RPC_PROG_UNAVAIL
module.exports.RPC_PROG_MISMATCH = binding.RPC_PROG_MISMATCH
module.exports.RPC_PROC_UNAVAIL = binding.RPC_PROC_UNAVAIL
module.exports.RPC_GARBAGE_ARGS = binding.RPC_GARBAGE_ARGS
module.exports.RPC_SYSTEM_ERR = binding.RPC_SYSTEM_ERR
module.exports.RPC_MISMATCH = binding.RPC_MISMATCH
module.exports.RPC_AUTH_ERROR = binding.RPC_AUTH_ERROR
module.exports.AUTH_OK = binding.AUTH_OK
module.exports.AUTH_BADCRED = binding.AUTH_BADCRED
module.exports.AUTH_REJECTEDCRED = binding.AUTH_REJECTEDCRED
module.exports.AUTH_BADVERF = binding.AUTH_BADVERF
module.exports.AUTH_REJECTEDVERF = binding.AUTH_REJECTEDVERF
module.exports.AUTH_TOOWEAK = binding.AUTH_TOOWEAK
module.exports.AUTH_INVALIDRESP = binding.AUTH_INVALIDRESP
module.exports.AUTH_FAILED = binding.AUTH_FAILED
module.exports.AUTH_NONE = binding.AUTH_NONE
module.exports.AUTH_SYS = binding.AUTH_SYS
module.exports.AUTH_SHORT = binding.AUTH_SHORT
module.exports.RPC_MAX_AUTH_BYTES = binding.RPC_MAX_AUTH_BYTES
module.exports.RM_LAST_FRAGMENT = binding.RM_LAST_FRAGMENT
module.exports.RM_LENGTH_MASK = binding.RM_LENGTH_MASK
module.exports.ACCEPTED_REPLY_HEADER_SIZE = binding.ACCEPTED_REPLY_HEADER_SIZE
module.exports.DEFAULT_RECORD_LIMIT = binding.DEFAULT_RECORD_LIMIT
module.exports.AUTH_NULL = binding.AUTH_NULL

module.exports.encodeAuthSys = binding.encodeAuthSys
module.exports.decodeAuthSys = binding.decodeAuthSys
module.exports.authSys = binding.authSys
module.exports.credentialsOf = binding.credentialsOf
module.exports.decodeCall = binding.decodeCall
module.exports.encodeCall = binding.encodeCall
module.exports.decodeReply = binding.decodeReply
module.exports.writeAcceptedReplyHeader = binding.writeAcceptedReplyHeader
module.exports.encodeAcceptedReply = binding.encodeAcceptedReply
module.exports.encodeAcceptError = binding.encodeAcceptError
module.exports.encodeAuthError = binding.encodeAuthError
module.exports.encodeRpcMismatch = binding.encodeRpcMismatch
module.exports.recordMark = binding.recordMark
module.exports.frameRecord = binding.frameRecord
module.exports.frameFragments = binding.frameFragments
module.exports.copyBytes = binding.copyBytes
module.exports.RecordAssembler = binding.RecordAssembler

// ./nfs previously exposed the NFS server facade as well as transport
// helpers. Preserve those runtime exports while the declarations below
// re-export their types from the root generated declaration.
module.exports.NfsServer = rootBinding.NfsServer
module.exports.NfsSession = rootBinding.NfsSession
module.exports.Nfs4Session = rootBinding.Nfs4Session
module.exports.NfsConnection = rootBinding.NfsConnection
module.exports.createNfsServer = rootBinding.createNfsServer
