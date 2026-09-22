"use strict"

// The FUSE entrypoint exposes the Rust wire helpers and mount-free session
// facade. It does not re-export the root mount lifecycle or claim native
// device/mount parity.
const binding = require("./postlude-fuse-codec.cjs")({ ...require("./index.js") })

if (!binding || typeof binding.decodeInHeader !== "function") {
  throw new Error(
    "mount-rs FUSE codec exports are unavailable; rebuild after registering src/fuse_codec.rs",
  )
}

module.exports = binding

// The session is a mount-free Rust dispatcher. Native Linux device and mount
// lifecycle remain owned by the Rust transport; exposing this class here keeps
// the FUSE subpath usable for deterministic protocol/session qualification.
module.exports.FuseSession = binding.FuseSession

// Spell public names directly so Node's CommonJS-to-ESM named-export bridge
// can discover the codec barrel without executing a loop.
module.exports.ProtocolError = binding.ProtocolError
module.exports.isProtocolError = binding.isProtocolError
module.exports.TranscriptError = binding.TranscriptError
module.exports.TranscriptRecorder = binding.TranscriptRecorder
module.exports.InodeTable = binding.InodeTable
module.exports.Filesystem = binding.Filesystem
module.exports.FuseSession = binding.FuseSession
module.exports.createFuseSession = binding.createFuseSession
module.exports.DEFAULT_ATTR_TIMEOUT = binding.DEFAULT_ATTR_TIMEOUT
module.exports.DEFAULT_ENTRY_TIMEOUT = binding.DEFAULT_ENTRY_TIMEOUT
module.exports.DEFAULT_FLUSH_MECHANISM = binding.DEFAULT_FLUSH_MECHANISM
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
module.exports.decodeSymlinkIn = binding.decodeSymlinkIn
module.exports.encodeSymlinkIn = binding.encodeSymlinkIn
module.exports.decodeSymlinkOut = binding.decodeSymlinkOut
module.exports.encodeSymlinkOut = binding.encodeSymlinkOut
module.exports.decodeMknodIn = binding.decodeMknodIn
module.exports.encodeMknodIn = binding.encodeMknodIn
module.exports.decodeMknodOut = binding.decodeMknodOut
module.exports.encodeMknodOut = binding.encodeMknodOut
module.exports.decodeMkdirIn = binding.decodeMkdirIn
module.exports.encodeMkdirIn = binding.encodeMkdirIn
module.exports.decodeMkdirOut = binding.decodeMkdirOut
module.exports.encodeMkdirOut = binding.encodeMkdirOut
module.exports.decodeUnlinkIn = binding.decodeUnlinkIn
module.exports.encodeUnlinkIn = binding.encodeUnlinkIn
module.exports.decodeRmdirIn = binding.decodeRmdirIn
module.exports.encodeRmdirIn = binding.encodeRmdirIn
module.exports.decodeRenameIn = binding.decodeRenameIn
module.exports.encodeRenameIn = binding.encodeRenameIn
module.exports.decodeRename2In = binding.decodeRename2In
module.exports.encodeRename2In = binding.encodeRename2In
module.exports.decodeLinkIn = binding.decodeLinkIn
module.exports.encodeLinkIn = binding.encodeLinkIn
module.exports.decodeLinkOut = binding.decodeLinkOut
module.exports.encodeLinkOut = binding.encodeLinkOut
module.exports.decodeAccessIn = binding.decodeAccessIn
module.exports.encodeAccessIn = binding.encodeAccessIn
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
module.exports.decodeBatchForgetIn = binding.decodeBatchForgetIn
module.exports.encodeBatchForgetIn = binding.encodeBatchForgetIn
module.exports.decodeReadlinkIn = binding.decodeReadlinkIn
module.exports.encodeReadlinkIn = binding.encodeReadlinkIn
module.exports.decodeReadlinkOut = binding.decodeReadlinkOut
module.exports.encodeReadlinkOut = binding.encodeReadlinkOut
module.exports.decodeReleaseIn = binding.decodeReleaseIn
module.exports.encodeReleaseIn = binding.encodeReleaseIn
module.exports.decodeFlushIn = binding.decodeFlushIn
module.exports.encodeFlushIn = binding.encodeFlushIn
module.exports.decodeFsyncIn = binding.decodeFsyncIn
module.exports.encodeFsyncIn = binding.encodeFsyncIn
module.exports.decodeSyncfsIn = binding.decodeSyncfsIn
module.exports.encodeSyncfsIn = binding.encodeSyncfsIn
module.exports.decodeLkIn = binding.decodeLkIn
module.exports.encodeLkIn = binding.encodeLkIn
module.exports.decodeLkOut = binding.decodeLkOut
module.exports.encodeLkOut = binding.encodeLkOut
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
module.exports.decodeStatfsIn = binding.decodeStatfsIn
module.exports.encodeStatfsIn = binding.encodeStatfsIn
module.exports.decodeStatfsOut = binding.decodeStatfsOut
module.exports.encodeStatfsOut = binding.encodeStatfsOut
module.exports.decodeInterruptIn = binding.decodeInterruptIn
module.exports.encodeInterruptIn = binding.encodeInterruptIn
module.exports.decodeIoctlIn = binding.decodeIoctlIn
module.exports.encodeIoctlIn = binding.encodeIoctlIn
module.exports.decodeIoctlOut = binding.decodeIoctlOut
module.exports.encodeIoctlOut = binding.encodeIoctlOut
module.exports.decodePollIn = binding.decodePollIn
module.exports.encodePollIn = binding.encodePollIn
module.exports.decodePollOut = binding.decodePollOut
module.exports.encodePollOut = binding.encodePollOut
module.exports.decodeBmapIn = binding.decodeBmapIn
module.exports.encodeBmapIn = binding.encodeBmapIn
module.exports.decodeBmapOut = binding.decodeBmapOut
module.exports.encodeBmapOut = binding.encodeBmapOut
module.exports.decodeFallocateIn = binding.decodeFallocateIn
module.exports.encodeFallocateIn = binding.encodeFallocateIn
module.exports.decodeLseekIn = binding.decodeLseekIn
module.exports.encodeLseekIn = binding.encodeLseekIn
module.exports.decodeLseekOut = binding.decodeLseekOut
module.exports.encodeLseekOut = binding.encodeLseekOut
module.exports.decodeSetxattrIn = binding.decodeSetxattrIn
module.exports.encodeSetxattrIn = binding.encodeSetxattrIn
module.exports.decodeGetxattrIn = binding.decodeGetxattrIn
module.exports.encodeGetxattrIn = binding.encodeGetxattrIn
module.exports.decodeListxattrIn = binding.decodeListxattrIn
module.exports.encodeListxattrIn = binding.encodeListxattrIn
module.exports.decodeRemovexattrIn = binding.decodeRemovexattrIn
module.exports.encodeRemovexattrIn = binding.encodeRemovexattrIn
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
module.exports.FUSE_FORGET = binding.FUSE_FORGET
module.exports.FUSE_GETATTR = binding.FUSE_GETATTR
module.exports.FUSE_SETATTR = binding.FUSE_SETATTR
module.exports.FUSE_GETATTR_FH = binding.FUSE_GETATTR_FH
module.exports.FUSE_READLINK = binding.FUSE_READLINK
module.exports.FUSE_SYMLINK = binding.FUSE_SYMLINK
module.exports.FUSE_MKNOD = binding.FUSE_MKNOD
module.exports.FUSE_MKDIR = binding.FUSE_MKDIR
module.exports.FUSE_UNLINK = binding.FUSE_UNLINK
module.exports.FUSE_RMDIR = binding.FUSE_RMDIR
module.exports.FUSE_RENAME = binding.FUSE_RENAME
module.exports.FUSE_RENAME2 = binding.FUSE_RENAME2
module.exports.FUSE_LINK = binding.FUSE_LINK
module.exports.FUSE_STATFS = binding.FUSE_STATFS
module.exports.FUSE_SETXATTR = binding.FUSE_SETXATTR
module.exports.FUSE_GETXATTR = binding.FUSE_GETXATTR
module.exports.FUSE_LISTXATTR = binding.FUSE_LISTXATTR
module.exports.FUSE_REMOVEXATTR = binding.FUSE_REMOVEXATTR
module.exports.FUSE_INTERRUPT = binding.FUSE_INTERRUPT
module.exports.FUSE_IOCTL = binding.FUSE_IOCTL
module.exports.FUSE_BATCH_FORGET = binding.FUSE_BATCH_FORGET
module.exports.FUSE_POLL = binding.FUSE_POLL
module.exports.FUSE_BMAP = binding.FUSE_BMAP
module.exports.FUSE_ACCESS = binding.FUSE_ACCESS
module.exports.FUSE_FALLOCATE = binding.FUSE_FALLOCATE
module.exports.FUSE_LSEEK = binding.FUSE_LSEEK
module.exports.FUSE_GETLK = binding.FUSE_GETLK
module.exports.FUSE_SETLK = binding.FUSE_SETLK
module.exports.FUSE_SETLKW = binding.FUSE_SETLKW
module.exports.FUSE_LK_FLOCK = binding.FUSE_LK_FLOCK
module.exports.F_RDLCK = binding.F_RDLCK
module.exports.F_WRLCK = binding.F_WRLCK
module.exports.F_UNLCK = binding.F_UNLCK
module.exports.FUSE_SETXATTR = binding.FUSE_SETXATTR
module.exports.FUSE_GETXATTR = binding.FUSE_GETXATTR
module.exports.FUSE_LISTXATTR = binding.FUSE_LISTXATTR
module.exports.FUSE_REMOVEXATTR = binding.FUSE_REMOVEXATTR
module.exports.FUSE_ASYNC_DIO = binding.FUSE_ASYNC_DIO
module.exports.FUSE_PARALLEL_DIROPS = binding.FUSE_PARALLEL_DIROPS
module.exports.FUSE_SETXATTR_EXT = binding.FUSE_SETXATTR_EXT
module.exports.FUSE_SETXATTR_ACL_KILL_SGID = binding.FUSE_SETXATTR_ACL_KILL_SGID
module.exports.XATTR_CREATE = binding.XATTR_CREATE
module.exports.XATTR_REPLACE = binding.XATTR_REPLACE
module.exports.FUSE_POLL_SCHEDULE_NOTIFY = binding.FUSE_POLL_SCHEDULE_NOTIFY
module.exports.FUSE_SETXATTR_EXT = binding.FUSE_SETXATTR_EXT
module.exports.FUSE_SETXATTR_ACL_KILL_SGID = binding.FUSE_SETXATTR_ACL_KILL_SGID
module.exports.XATTR_CREATE = binding.XATTR_CREATE
module.exports.XATTR_REPLACE = binding.XATTR_REPLACE
module.exports.FUSE_RELEASE = binding.FUSE_RELEASE
module.exports.FUSE_RELEASEDIR = binding.FUSE_RELEASEDIR
module.exports.FUSE_FLUSH = binding.FUSE_FLUSH
module.exports.FUSE_FSYNC = binding.FUSE_FSYNC
module.exports.FUSE_FSYNCDIR = binding.FUSE_FSYNCDIR
module.exports.FUSE_SYNCFS = binding.FUSE_SYNCFS
module.exports.SEEK_SET = binding.SEEK_SET
module.exports.SEEK_CUR = binding.SEEK_CUR
module.exports.SEEK_END = binding.SEEK_END
module.exports.SEEK_DATA = binding.SEEK_DATA
module.exports.SEEK_HOLE = binding.SEEK_HOLE
module.exports.FUSE_RELEASE_FLUSH = binding.FUSE_RELEASE_FLUSH
module.exports.FUSE_RELEASE_FLOCK_UNLOCK = binding.FUSE_RELEASE_FLOCK_UNLOCK
module.exports.FUSE_FSYNC_FDATASYNC = binding.FUSE_FSYNC_FDATASYNC
module.exports.FATTR_MODE = binding.FATTR_MODE
module.exports.FATTR_SIZE = binding.FATTR_SIZE
module.exports.FATTR_ATIME = binding.FATTR_ATIME
module.exports.FATTR_MTIME = binding.FATTR_MTIME
module.exports.FUSE_INIT = binding.FUSE_INIT
module.exports.FUSE_DESTROY = binding.FUSE_DESTROY
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

// Keep the complete wire-constant surface statically discoverable too. The
// runtime loop above is useful for CommonJS consumers, but Node's ESM bridge
// cannot infer named exports that are assigned only from inside a loop.
module.exports.FUSE_MIN_READ_BUFFER = binding.FUSE_MIN_READ_BUFFER
module.exports.FUSE_PAGE_SIZE = binding.FUSE_PAGE_SIZE
module.exports.FUSE_MAX_MAX_PAGES = binding.FUSE_MAX_MAX_PAGES
module.exports.FUSE_DEFAULT_MAX_PAGES_PER_REQ = binding.FUSE_DEFAULT_MAX_PAGES_PER_REQ
module.exports.FUSE_MKNOD = binding.FUSE_MKNOD
module.exports.FUSE_MKDIR = binding.FUSE_MKDIR
module.exports.FUSE_UNLINK = binding.FUSE_UNLINK
module.exports.FUSE_RMDIR = binding.FUSE_RMDIR
module.exports.FUSE_RENAME = binding.FUSE_RENAME
module.exports.FUSE_LINK = binding.FUSE_LINK
module.exports.FUSE_OPEN = binding.FUSE_OPEN
module.exports.FUSE_READ = binding.FUSE_READ
module.exports.FUSE_WRITE = binding.FUSE_WRITE
module.exports.FUSE_INIT = binding.FUSE_INIT
module.exports.FUSE_OPENDIR = binding.FUSE_OPENDIR
module.exports.FUSE_READDIR = binding.FUSE_READDIR
module.exports.FUSE_CREATE = binding.FUSE_CREATE
module.exports.FUSE_DESTROY = binding.FUSE_DESTROY
module.exports.FUSE_NOTIFY_REPLY = binding.FUSE_NOTIFY_REPLY
module.exports.FUSE_FALLOCATE = binding.FUSE_FALLOCATE
module.exports.FUSE_READDIRPLUS = binding.FUSE_READDIRPLUS
module.exports.FUSE_COPY_FILE_RANGE = binding.FUSE_COPY_FILE_RANGE
module.exports.FUSE_SETUPMAPPING = binding.FUSE_SETUPMAPPING
module.exports.FUSE_REMOVEMAPPING = binding.FUSE_REMOVEMAPPING
module.exports.FUSE_TMPFILE = binding.FUSE_TMPFILE
module.exports.FUSE_STATX = binding.FUSE_STATX
module.exports.CUSE_INIT = binding.CUSE_INIT
module.exports.FUSE_NOTIFY_POLL = binding.FUSE_NOTIFY_POLL
module.exports.FUSE_NOTIFY_INVAL_INODE = binding.FUSE_NOTIFY_INVAL_INODE
module.exports.FUSE_NOTIFY_INVAL_ENTRY = binding.FUSE_NOTIFY_INVAL_ENTRY
module.exports.FUSE_NOTIFY_STORE = binding.FUSE_NOTIFY_STORE
module.exports.FUSE_NOTIFY_RETRIEVE = binding.FUSE_NOTIFY_RETRIEVE
module.exports.FUSE_NOTIFY_DELETE = binding.FUSE_NOTIFY_DELETE
module.exports.FUSE_NOTIFY_RESEND = binding.FUSE_NOTIFY_RESEND
module.exports.FUSE_NOTIFY_UNIQUE = binding.FUSE_NOTIFY_UNIQUE
module.exports.FATTR_UID = binding.FATTR_UID
module.exports.FATTR_GID = binding.FATTR_GID
module.exports.FATTR_FH = binding.FATTR_FH
module.exports.FATTR_ATIME_NOW = binding.FATTR_ATIME_NOW
module.exports.FATTR_MTIME_NOW = binding.FATTR_MTIME_NOW
module.exports.FATTR_LOCKOWNER = binding.FATTR_LOCKOWNER
module.exports.FATTR_CTIME = binding.FATTR_CTIME
module.exports.FATTR_KILL_SUIDGID = binding.FATTR_KILL_SUIDGID
module.exports.FOPEN_DIRECT_IO = binding.FOPEN_DIRECT_IO
module.exports.FOPEN_KEEP_CACHE = binding.FOPEN_KEEP_CACHE
module.exports.FOPEN_NONSEEKABLE = binding.FOPEN_NONSEEKABLE
module.exports.FOPEN_CACHE_DIR = binding.FOPEN_CACHE_DIR
module.exports.FOPEN_STREAM = binding.FOPEN_STREAM
module.exports.FOPEN_NOFLUSH = binding.FOPEN_NOFLUSH
module.exports.FOPEN_PARALLEL_DIRECT_WRITES = binding.FOPEN_PARALLEL_DIRECT_WRITES
module.exports.FOPEN_PASSTHROUGH = binding.FOPEN_PASSTHROUGH
module.exports.FUSE_ASYNC_READ = binding.FUSE_ASYNC_READ
module.exports.FUSE_POSIX_LOCKS = binding.FUSE_POSIX_LOCKS
module.exports.FUSE_FILE_OPS = binding.FUSE_FILE_OPS
module.exports.FUSE_ATOMIC_O_TRUNC = binding.FUSE_ATOMIC_O_TRUNC
module.exports.FUSE_EXPORT_SUPPORT = binding.FUSE_EXPORT_SUPPORT
module.exports.FUSE_BIG_WRITES = binding.FUSE_BIG_WRITES
module.exports.FUSE_DONT_MASK = binding.FUSE_DONT_MASK
module.exports.FUSE_SPLICE_WRITE = binding.FUSE_SPLICE_WRITE
module.exports.FUSE_SPLICE_MOVE = binding.FUSE_SPLICE_MOVE
module.exports.FUSE_SPLICE_READ = binding.FUSE_SPLICE_READ
module.exports.FUSE_FLOCK_LOCKS = binding.FUSE_FLOCK_LOCKS
module.exports.FUSE_HAS_IOCTL_DIR = binding.FUSE_HAS_IOCTL_DIR
module.exports.FUSE_AUTO_INVAL_DATA = binding.FUSE_AUTO_INVAL_DATA
module.exports.FUSE_DO_READDIRPLUS = binding.FUSE_DO_READDIRPLUS
module.exports.FUSE_READDIRPLUS_AUTO = binding.FUSE_READDIRPLUS_AUTO
module.exports.FUSE_WRITEBACK_CACHE = binding.FUSE_WRITEBACK_CACHE
module.exports.FUSE_NO_OPEN_SUPPORT = binding.FUSE_NO_OPEN_SUPPORT
module.exports.FUSE_HANDLE_KILLPRIV = binding.FUSE_HANDLE_KILLPRIV
module.exports.FUSE_POSIX_ACL = binding.FUSE_POSIX_ACL
module.exports.FUSE_ABORT_ERROR = binding.FUSE_ABORT_ERROR
module.exports.FUSE_MAX_PAGES = binding.FUSE_MAX_PAGES
module.exports.FUSE_CACHE_SYMLINKS = binding.FUSE_CACHE_SYMLINKS
module.exports.FUSE_NO_OPENDIR_SUPPORT = binding.FUSE_NO_OPENDIR_SUPPORT
module.exports.FUSE_EXPLICIT_INVAL_DATA = binding.FUSE_EXPLICIT_INVAL_DATA
module.exports.FUSE_MAP_ALIGNMENT = binding.FUSE_MAP_ALIGNMENT
module.exports.FUSE_SUBMOUNTS = binding.FUSE_SUBMOUNTS
module.exports.FUSE_HANDLE_KILLPRIV_V2 = binding.FUSE_HANDLE_KILLPRIV_V2
module.exports.FUSE_INIT_EXT = binding.FUSE_INIT_EXT
module.exports.FUSE_INIT_RESERVED = binding.FUSE_INIT_RESERVED
module.exports.FUSE_SECURITY_CTX = binding.FUSE_SECURITY_CTX
module.exports.FUSE_HAS_INODE_DAX = binding.FUSE_HAS_INODE_DAX
module.exports.FUSE_CREATE_SUPP_GROUP = binding.FUSE_CREATE_SUPP_GROUP
module.exports.FUSE_HAS_EXPIRE_ONLY = binding.FUSE_HAS_EXPIRE_ONLY
module.exports.FUSE_DIRECT_IO_ALLOW_MMAP = binding.FUSE_DIRECT_IO_ALLOW_MMAP
module.exports.FUSE_PASSTHROUGH = binding.FUSE_PASSTHROUGH
module.exports.FUSE_NO_EXPORT_SUPPORT = binding.FUSE_NO_EXPORT_SUPPORT
module.exports.FUSE_HAS_RESEND = binding.FUSE_HAS_RESEND
module.exports.FUSE_ALLOW_IDMAP = binding.FUSE_ALLOW_IDMAP
module.exports.FUSE_WRITE_CACHE = binding.FUSE_WRITE_CACHE
module.exports.FUSE_WRITE_LOCKOWNER = binding.FUSE_WRITE_LOCKOWNER
module.exports.FUSE_WRITE_KILL_SUIDGID = binding.FUSE_WRITE_KILL_SUIDGID
module.exports.FUSE_READ_LOCKOWNER = binding.FUSE_READ_LOCKOWNER
module.exports.FUSE_ATTR_SUBMOUNT = binding.FUSE_ATTR_SUBMOUNT
module.exports.FUSE_ATTR_DAX = binding.FUSE_ATTR_DAX
module.exports.FUSE_OPEN_KILL_SUIDGID = binding.FUSE_OPEN_KILL_SUIDGID
module.exports.FUSE_EXPIRE_ONLY = binding.FUSE_EXPIRE_ONLY
module.exports.FUSE_UNIQUE_RESEND = binding.FUSE_UNIQUE_RESEND
module.exports.FUSE_INVALID_UIDGID = binding.FUSE_INVALID_UIDGID
module.exports.FUSE_MAX_NR_SECCTX = binding.FUSE_MAX_NR_SECCTX
module.exports.FUSE_EXT_GROUPS = binding.FUSE_EXT_GROUPS
module.exports.DT_UNKNOWN = binding.DT_UNKNOWN
module.exports.DT_FIFO = binding.DT_FIFO
module.exports.DT_CHR = binding.DT_CHR
module.exports.DT_DIR = binding.DT_DIR
module.exports.DT_BLK = binding.DT_BLK
module.exports.DT_REG = binding.DT_REG
module.exports.DT_LNK = binding.DT_LNK
module.exports.DT_SOCK = binding.DT_SOCK
module.exports.O_ACCMODE = binding.O_ACCMODE
module.exports.O_RDONLY = binding.O_RDONLY
module.exports.O_WRONLY = binding.O_WRONLY
module.exports.O_RDWR = binding.O_RDWR
module.exports.O_CREAT = binding.O_CREAT
module.exports.O_EXCL = binding.O_EXCL
module.exports.O_TRUNC = binding.O_TRUNC
module.exports.O_APPEND = binding.O_APPEND
module.exports.FUSE_DIRENT_HEADER_SIZE = binding.FUSE_DIRENT_HEADER_SIZE
module.exports.FUSE_INIT_OUT_SIZE = binding.FUSE_INIT_OUT_SIZE
module.exports.FUSE_COMPAT_INIT_OUT_SIZE = binding.FUSE_COMPAT_INIT_OUT_SIZE
module.exports.FUSE_COMPAT_22_INIT_OUT_SIZE = binding.FUSE_COMPAT_22_INIT_OUT_SIZE
module.exports.FUSE_COMPAT_ENTRY_OUT_SIZE = binding.FUSE_COMPAT_ENTRY_OUT_SIZE
module.exports.FUSE_COMPAT_ATTR_OUT_SIZE = binding.FUSE_COMPAT_ATTR_OUT_SIZE
module.exports.FUSE_COMPAT_STATFS_SIZE = binding.FUSE_COMPAT_STATFS_SIZE
module.exports.FUSE_COMPAT_WRITE_IN_SIZE = binding.FUSE_COMPAT_WRITE_IN_SIZE
module.exports.FUSE_COMPAT_MKNOD_IN_SIZE = binding.FUSE_COMPAT_MKNOD_IN_SIZE
module.exports.FUSE_COMPAT_SETXATTR_IN_SIZE = binding.FUSE_COMPAT_SETXATTR_IN_SIZE
module.exports.OPCODES = binding.OPCODES
module.exports.decodeRequestBody = binding.decodeRequestBody
module.exports.encodeRequestBody = binding.encodeRequestBody
module.exports.decodeReplyBody = binding.decodeReplyBody
module.exports.encodeReplyBody = binding.encodeReplyBody
module.exports.decodeRequest = binding.decodeRequest
module.exports.encodeRequest = binding.encodeRequest
module.exports.decodeReply = binding.decodeReply
module.exports.encodeReplyFor = binding.encodeReplyFor
