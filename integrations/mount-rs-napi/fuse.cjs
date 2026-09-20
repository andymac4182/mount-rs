"use strict"

// The FUSE entrypoint is intentionally a codec barrel.  It shares the native
// loader for the Rust wire helpers, but it does not re-export the root mount
// lifecycle or claim a Node-facing FUSE session/device implementation.
const binding = require("./postlude-fuse-codec.cjs")({ ...require("./index.js") })

if (!binding || typeof binding.decodeInHeader !== "function") {
  throw new Error(
    "mount-rs FUSE codec exports are unavailable; rebuild after registering src/fuse_codec.rs",
  )
}

module.exports = binding

// Spell public names directly so Node's CommonJS-to-ESM named-export bridge
// can discover the codec barrel without executing a loop.
module.exports.ProtocolError = binding.ProtocolError
module.exports.isProtocolError = binding.isProtocolError
module.exports.TranscriptError = binding.TranscriptError
module.exports.TranscriptRecorder = binding.TranscriptRecorder
module.exports.InodeTable = binding.InodeTable
module.exports.INODE_GENERATION = binding.INODE_GENERATION
module.exports.FUSE_NAME_MAX = binding.FUSE_NAME_MAX
module.exports.encodeNotify = binding.encodeNotify
module.exports.decodeNotify = binding.decodeNotify
module.exports.encodeNotifyInvalInode = binding.encodeNotifyInvalInode
module.exports.decodeNotifyInvalInode = binding.decodeNotifyInvalInode
module.exports.encodeNotifyInvalEntry = binding.encodeNotifyInvalEntry
module.exports.decodeNotifyInvalEntry = binding.decodeNotifyInvalEntry
module.exports.encodeTranscript = binding.encodeTranscript
module.exports.decodeTranscript = binding.decodeTranscript
module.exports.TRANSCRIPT_MAGIC = binding.TRANSCRIPT_MAGIC
module.exports.TRANSCRIPT_VERSION = binding.TRANSCRIPT_VERSION
module.exports.decodeInHeader = binding.decodeInHeader
module.exports.encodeInHeader = binding.encodeInHeader
module.exports.decodeOutHeader = binding.decodeOutHeader
module.exports.encodeOutHeader = binding.encodeOutHeader
module.exports.writeOutHeaderInto = binding.writeOutHeaderInto
module.exports.fuseErrno = binding.fuseErrno
module.exports.allocReply = binding.allocReply
module.exports.finishReply = binding.finishReply
module.exports.encodeReply = binding.encodeReply
module.exports.encodeErrorReply = binding.encodeErrorReply
module.exports.encodeErrorReplyFor = binding.encodeErrorReplyFor
module.exports.attrSize = binding.attrSize
module.exports.entryOutSize = binding.entryOutSize
module.exports.attrOutSize = binding.attrOutSize
module.exports.kstatfsSize = binding.kstatfsSize
module.exports.initOutSize = binding.initOutSize
module.exports.readWriteInSize = binding.readWriteInSize
module.exports.decodeEntryOut = binding.decodeEntryOut
module.exports.encodeEntryOut = binding.encodeEntryOut
module.exports.decodeLookupIn = binding.decodeLookupIn
module.exports.encodeLookupIn = binding.encodeLookupIn
module.exports.decodeLookupOut = binding.decodeLookupOut
module.exports.encodeLookupOut = binding.encodeLookupOut
module.exports.decodeAttrOut = binding.decodeAttrOut
module.exports.encodeAttrOut = binding.encodeAttrOut
module.exports.decodeGetattrIn = binding.decodeGetattrIn
module.exports.encodeGetattrIn = binding.encodeGetattrIn
module.exports.decodeGetattrOut = binding.decodeGetattrOut
module.exports.encodeGetattrOut = binding.encodeGetattrOut
module.exports.decodeSetattrIn = binding.decodeSetattrIn
module.exports.encodeSetattrIn = binding.encodeSetattrIn
module.exports.decodeSetattrOut = binding.decodeSetattrOut
module.exports.encodeSetattrOut = binding.encodeSetattrOut
module.exports.decodeOpenIn = binding.decodeOpenIn
module.exports.encodeOpenIn = binding.encodeOpenIn
module.exports.decodeOpenOut = binding.decodeOpenOut
module.exports.encodeOpenOut = binding.encodeOpenOut
module.exports.decodeCreateIn = binding.decodeCreateIn
module.exports.encodeCreateIn = binding.encodeCreateIn
module.exports.decodeCreateOut = binding.decodeCreateOut
module.exports.encodeCreateOut = binding.encodeCreateOut
module.exports.decodeReleaseIn = binding.decodeReleaseIn
module.exports.encodeReleaseIn = binding.encodeReleaseIn
module.exports.decodeFlushIn = binding.decodeFlushIn
module.exports.encodeFlushIn = binding.encodeFlushIn
module.exports.decodeFsyncIn = binding.decodeFsyncIn
module.exports.encodeFsyncIn = binding.encodeFsyncIn
module.exports.decodeReadIn = binding.decodeReadIn
module.exports.encodeReadIn = binding.encodeReadIn
module.exports.decodeReadOut = binding.decodeReadOut
module.exports.encodeReadOut = binding.encodeReadOut
module.exports.decodeWriteIn = binding.decodeWriteIn
module.exports.encodeWriteIn = binding.encodeWriteIn
module.exports.decodeWriteOut = binding.decodeWriteOut
module.exports.encodeWriteOut = binding.encodeWriteOut
module.exports.decodeInitIn = binding.decodeInitIn
module.exports.encodeInitIn = binding.encodeInitIn
module.exports.decodeInitOut = binding.decodeInitOut
module.exports.encodeInitOut = binding.encodeInitOut
module.exports.decodeStatfsOut = binding.decodeStatfsOut
module.exports.encodeStatfsOut = binding.encodeStatfsOut
module.exports.decodeGetxattrOut = binding.decodeGetxattrOut
module.exports.encodeGetxattrOut = binding.encodeGetxattrOut
module.exports.encodeXattrNames = binding.encodeXattrNames
module.exports.decodeXattrNames = binding.decodeXattrNames
module.exports.joinInitFlags = binding.joinInitFlags
module.exports.splitInitFlags = binding.splitInitFlags
module.exports.direntAlign = binding.direntAlign
module.exports.direntSize = binding.direntSize
module.exports.direntPlusSize = binding.direntPlusSize
module.exports.direntType = binding.direntType
module.exports.packDirents = binding.packDirents
module.exports.unpackDirents = binding.unpackDirents
module.exports.packDirentsPlus = binding.packDirentsPlus
module.exports.unpackDirentsPlus = binding.unpackDirentsPlus
module.exports.translateOpenFlags = binding.translateOpenFlags
module.exports.driverOpenFlags = binding.driverOpenFlags
module.exports.reopenFlags = binding.reopenFlags
module.exports.nameByteLength = binding.nameByteLength
module.exports.opcodeName = binding.opcodeName
module.exports.OPCODE_NAMES = binding.OPCODE_NAMES
module.exports.SUPPORTED_OPCODES = binding.SUPPORTED_OPCODES
module.exports.UNIMPLEMENTED_OPCODES = binding.UNIMPLEMENTED_OPCODES

for (const name of [
  "FUSE_KERNEL_VERSION", "FUSE_KERNEL_MINOR_VERSION", "FUSE_ROOT_ID", "FUSE_MIN_READ_BUFFER",
  "FUSE_PAGE_SIZE", "FUSE_MAX_MAX_PAGES", "FUSE_DEFAULT_MAX_PAGES_PER_REQ", "FUSE_LOOKUP",
  "FUSE_FORGET", "FUSE_GETATTR", "FUSE_SETATTR", "FUSE_READLINK", "FUSE_SYMLINK", "FUSE_MKNOD",
  "FUSE_MKDIR", "FUSE_UNLINK", "FUSE_RMDIR", "FUSE_RENAME", "FUSE_LINK", "FUSE_OPEN", "FUSE_READ",
  "FUSE_WRITE", "FUSE_STATFS", "FUSE_RELEASE", "FUSE_FSYNC", "FUSE_SETXATTR", "FUSE_GETXATTR",
  "FUSE_LISTXATTR", "FUSE_REMOVEXATTR", "FUSE_FLUSH", "FUSE_INIT", "FUSE_OPENDIR", "FUSE_READDIR",
  "FUSE_RELEASEDIR", "FUSE_FSYNCDIR", "FUSE_GETLK", "FUSE_SETLK", "FUSE_SETLKW", "FUSE_ACCESS",
  "FUSE_CREATE", "FUSE_INTERRUPT", "FUSE_BMAP", "FUSE_DESTROY", "FUSE_IOCTL", "FUSE_POLL",
  "FUSE_NOTIFY_REPLY", "FUSE_BATCH_FORGET", "FUSE_FALLOCATE", "FUSE_READDIRPLUS", "FUSE_RENAME2",
  "FUSE_LSEEK", "FUSE_COPY_FILE_RANGE", "FUSE_SETUPMAPPING", "FUSE_REMOVEMAPPING", "FUSE_SYNCFS",
  "FUSE_TMPFILE", "FUSE_STATX", "CUSE_INIT", "FUSE_NOTIFY_POLL", "FUSE_NOTIFY_INVAL_INODE",
  "FUSE_NOTIFY_INVAL_ENTRY", "FUSE_NOTIFY_STORE", "FUSE_NOTIFY_RETRIEVE", "FUSE_NOTIFY_DELETE",
  "FUSE_NOTIFY_RESEND", "FUSE_NOTIFY_UNIQUE", "FATTR_MODE", "FATTR_UID", "FATTR_GID", "FATTR_SIZE",
  "FATTR_ATIME", "FATTR_MTIME", "FATTR_FH", "FATTR_ATIME_NOW", "FATTR_MTIME_NOW", "FATTR_LOCKOWNER",
  "FATTR_CTIME", "FATTR_KILL_SUIDGID", "FOPEN_DIRECT_IO", "FOPEN_KEEP_CACHE", "FOPEN_NONSEEKABLE",
  "FOPEN_CACHE_DIR", "FOPEN_STREAM", "FOPEN_NOFLUSH", "FOPEN_PARALLEL_DIRECT_WRITES",
  "FOPEN_PASSTHROUGH", "FUSE_ASYNC_READ", "FUSE_POSIX_LOCKS", "FUSE_FILE_OPS", "FUSE_ATOMIC_O_TRUNC",
  "FUSE_EXPORT_SUPPORT", "FUSE_BIG_WRITES", "FUSE_DONT_MASK", "FUSE_SPLICE_WRITE", "FUSE_SPLICE_MOVE",
  "FUSE_SPLICE_READ", "FUSE_FLOCK_LOCKS", "FUSE_HAS_IOCTL_DIR", "FUSE_AUTO_INVAL_DATA", "FUSE_DO_READDIRPLUS",
  "FUSE_READDIRPLUS_AUTO", "FUSE_ASYNC_DIO", "FUSE_WRITEBACK_CACHE", "FUSE_NO_OPEN_SUPPORT",
  "FUSE_PARALLEL_DIROPS", "FUSE_HANDLE_KILLPRIV", "FUSE_POSIX_ACL", "FUSE_ABORT_ERROR", "FUSE_MAX_PAGES",
  "FUSE_CACHE_SYMLINKS", "FUSE_NO_OPENDIR_SUPPORT", "FUSE_EXPLICIT_INVAL_DATA", "FUSE_MAP_ALIGNMENT",
  "FUSE_SUBMOUNTS", "FUSE_HANDLE_KILLPRIV_V2", "FUSE_SETXATTR_EXT", "FUSE_INIT_EXT", "FUSE_INIT_RESERVED",
  "FUSE_SECURITY_CTX", "FUSE_HAS_INODE_DAX", "FUSE_CREATE_SUPP_GROUP", "FUSE_HAS_EXPIRE_ONLY",
  "FUSE_DIRECT_IO_ALLOW_MMAP", "FUSE_PASSTHROUGH", "FUSE_NO_EXPORT_SUPPORT", "FUSE_HAS_RESEND",
  "FUSE_ALLOW_IDMAP", "FUSE_RELEASE_FLUSH", "FUSE_RELEASE_FLOCK_UNLOCK", "FUSE_GETATTR_FH", "FUSE_LK_FLOCK",
  "FUSE_WRITE_CACHE", "FUSE_WRITE_LOCKOWNER", "FUSE_WRITE_KILL_SUIDGID", "FUSE_READ_LOCKOWNER",
  "FUSE_POLL_SCHEDULE_NOTIFY", "FUSE_FSYNC_FDATASYNC", "FUSE_ATTR_SUBMOUNT", "FUSE_ATTR_DAX",
  "FUSE_OPEN_KILL_SUIDGID", "FUSE_SETXATTR_ACL_KILL_SGID", "FUSE_EXPIRE_ONLY", "FUSE_UNIQUE_RESEND",
  "FUSE_INVALID_UIDGID", "FUSE_MAX_NR_SECCTX", "FUSE_EXT_GROUPS", "DT_UNKNOWN", "DT_FIFO", "DT_CHR",
  "DT_DIR", "DT_BLK", "DT_REG", "DT_LNK", "DT_SOCK", "O_ACCMODE", "O_RDONLY", "O_WRONLY", "O_RDWR",
  "O_CREAT", "O_EXCL", "O_TRUNC", "O_APPEND", "SEEK_SET", "SEEK_CUR", "SEEK_END", "SEEK_DATA", "SEEK_HOLE",
  "F_RDLCK", "F_WRLCK", "F_UNLCK", "XATTR_CREATE", "XATTR_REPLACE", "FUSE_IN_HEADER_SIZE",
  "FUSE_OUT_HEADER_SIZE", "FUSE_DIRENT_HEADER_SIZE", "FUSE_INIT_OUT_SIZE", "FUSE_COMPAT_INIT_OUT_SIZE",
  "FUSE_COMPAT_22_INIT_OUT_SIZE", "FUSE_COMPAT_ENTRY_OUT_SIZE", "FUSE_COMPAT_ATTR_OUT_SIZE",
  "FUSE_COMPAT_STATFS_SIZE", "FUSE_COMPAT_WRITE_IN_SIZE", "FUSE_COMPAT_MKNOD_IN_SIZE",
  "FUSE_COMPAT_SETXATTR_IN_SIZE", "DEFAULT_WANTED_FLAGS", "DEFAULT_MAX_WRITE", "DEFAULT_PROTOCOL",
]) module.exports[name] = binding[name]

// Keep the most commonly imported protocol constants statically discoverable
// for Node's CommonJS-to-ESM named-export bridge. The loop above remains the
// complete CommonJS surface; these assignments make `import { ... }` work for
// the core constants as well.
module.exports.FUSE_KERNEL_VERSION = binding.FUSE_KERNEL_VERSION
module.exports.FUSE_KERNEL_MINOR_VERSION = binding.FUSE_KERNEL_MINOR_VERSION
module.exports.FUSE_ROOT_ID = binding.FUSE_ROOT_ID
module.exports.FUSE_LOOKUP = binding.FUSE_LOOKUP
module.exports.FUSE_GETATTR = binding.FUSE_GETATTR
module.exports.FUSE_SETATTR = binding.FUSE_SETATTR
module.exports.FUSE_GETATTR_FH = binding.FUSE_GETATTR_FH
module.exports.FUSE_RELEASE = binding.FUSE_RELEASE
module.exports.FUSE_RELEASEDIR = binding.FUSE_RELEASEDIR
module.exports.FUSE_FLUSH = binding.FUSE_FLUSH
module.exports.FUSE_FSYNC = binding.FUSE_FSYNC
module.exports.FUSE_FSYNCDIR = binding.FUSE_FSYNCDIR
module.exports.FUSE_RELEASE_FLUSH = binding.FUSE_RELEASE_FLUSH
module.exports.FUSE_RELEASE_FLOCK_UNLOCK = binding.FUSE_RELEASE_FLOCK_UNLOCK
module.exports.FUSE_FSYNC_FDATASYNC = binding.FUSE_FSYNC_FDATASYNC
module.exports.FATTR_MODE = binding.FATTR_MODE
module.exports.FATTR_SIZE = binding.FATTR_SIZE
module.exports.FATTR_ATIME = binding.FATTR_ATIME
module.exports.FATTR_MTIME = binding.FATTR_MTIME
module.exports.FUSE_INIT = binding.FUSE_INIT
module.exports.FUSE_OPEN = binding.FUSE_OPEN
module.exports.FUSE_CREATE = binding.FUSE_CREATE
module.exports.FUSE_OPENDIR = binding.FUSE_OPENDIR
module.exports.FUSE_READ = binding.FUSE_READ
module.exports.FUSE_WRITE = binding.FUSE_WRITE
module.exports.FUSE_IN_HEADER_SIZE = binding.FUSE_IN_HEADER_SIZE
module.exports.FUSE_OUT_HEADER_SIZE = binding.FUSE_OUT_HEADER_SIZE
module.exports.DEFAULT_MAX_WRITE = binding.DEFAULT_MAX_WRITE
module.exports.DEFAULT_PROTOCOL = binding.DEFAULT_PROTOCOL
module.exports.OPCODE_NAMES = binding.OPCODE_NAMES
module.exports.SUPPORTED_OPCODES = binding.SUPPORTED_OPCODES
module.exports.UNIMPLEMENTED_OPCODES = binding.UNIMPLEMENTED_OPCODES
