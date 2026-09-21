/// <reference types="node" />

import type { Buffer } from "node:buffer"
import type {
  NativeFuseAccessIn,
  NativeFuseAttr,
  NativeFuseAttrOut,
  NativeFuseBmapIn,
  NativeFuseBmapOut,
  NativeFuseBatchForgetIn,
  NativeFuseCreateIn,
  NativeFuseCreateOut,
  NativeFuseEmpty,
  NativeFuseEntryOut,
  NativeFuseFallocateIn,
  NativeFuseFlushIn,
  NativeFuseForgetOne,
  NativeFuseFsyncIn,
  NativeFuseFileLock,
  NativeFuseGetattrIn,
  NativeFuseGetxattrIn,
  NativeFuseGetxattrOut,
  NativeFuseInHeader,
  NativeFuseInitIn,
  NativeFuseInitOut,
  NativeFuseInterruptIn,
  NativeFuseIoctlIn,
  NativeFuseIoctlOut,
  NativeFuseListxattrIn,
  NativeFuseLkIn,
  NativeFuseLkOut,
  NativeFuseLinkIn,
  NativeFuseLseekIn,
  NativeFuseLseekOut,
  NativeFuseMkdirIn,
  NativeFuseMknodIn,
  NativeFuseKstatfs,
  NativeFuseNameIn,
  NativeFuseNotification,
  NativeFuseNotifyInvalEntryOut,
  NativeFuseNotifyInvalInodeOut,
  NativeFuseOpenIn,
  NativeFuseOpenOut,
  NativeFuseOutHeader,
  NativeFusePollIn,
  NativeFusePollOut,
  NativeFuseProtocolContext,
  NativeFuseRawData,
  NativeFuseReadIn,
  NativeFuseReadlinkOut,
  NativeFuseReleaseIn,
  NativeFuseRename2In,
  NativeFuseRenameIn,
  NativeFuseSetxattrIn,
  NativeFuseSetattrIn,
  NativeFuseSplitInitFlags,
  NativeFuseSyncfsIn,
  NativeFuseSymlinkIn,
  NativeFuseTranscriptFrame,
  NativeFuseWriteIn,
  NativeFuseWriteOut,
} from "../index.js"

export type {
  NativeFuseAccessIn,
  NativeFuseAttr,
  NativeFuseAttrOut,
  NativeFuseBmapIn,
  NativeFuseBmapOut,
  NativeFuseBatchForgetIn,
  NativeFuseCreateIn,
  NativeFuseCreateOut,
  NativeFuseEmpty,
  NativeFuseEntryOut,
  NativeFuseFallocateIn,
  NativeFuseFlushIn,
  NativeFuseForgetOne,
  NativeFuseFsyncIn,
  NativeFuseFileLock,
  NativeFuseGetattrIn,
  NativeFuseGetxattrIn,
  NativeFuseGetxattrOut,
  NativeFuseInHeader,
  NativeFuseInitIn,
  NativeFuseInitOut,
  NativeFuseInterruptIn,
  NativeFuseIoctlIn,
  NativeFuseIoctlOut,
  NativeFuseListxattrIn,
  NativeFuseLkIn,
  NativeFuseLkOut,
  NativeFuseLinkIn,
  NativeFuseLseekIn,
  NativeFuseLseekOut,
  NativeFuseMkdirIn,
  NativeFuseMknodIn,
  NativeFuseKstatfs,
  NativeFuseNameIn,
  NativeFuseNotification,
  NativeFuseNotifyInvalEntryOut,
  NativeFuseNotifyInvalInodeOut,
  NativeFuseOpenIn,
  NativeFuseOpenOut,
  NativeFuseOutHeader,
  NativeFusePollIn,
  NativeFusePollOut,
  NativeFuseProtocolContext,
  NativeFuseRawData,
  NativeFuseReadIn,
  NativeFuseReadlinkOut,
  NativeFuseReleaseIn,
  NativeFuseRename2In,
  NativeFuseRenameIn,
  NativeFuseSetxattrIn,
  NativeFuseSetattrIn,
  NativeFuseSplitInitFlags,
  NativeFuseSyncfsIn,
  NativeFuseSymlinkIn,
  NativeFuseTranscriptFrame,
  NativeFuseWriteIn,
  NativeFuseWriteOut,
}

export declare class ProtocolError extends Error {
  readonly code: "ERR_FUSE_PROTOCOL"
  readonly offset?: number
}

export declare class TranscriptError extends Error {}

export declare function isProtocolError(error: unknown): error is ProtocolError

export interface Inode {
  readonly nodeid: bigint
  key: string | undefined
  nlookup: bigint
  readonly paths: Set<string>
}

export interface InodeTableOptions {
  useDriverIno?: boolean
}

export interface NativeFuseInitPreferences {
  minor?: number
  maxWrite?: number
  maxReadahead?: number
  maxBackground?: number
  congestionThreshold?: number
  timeGran?: number
  maxStackDepth?: number
  flags?: bigint
  extraFlags?: bigint
  withoutFlags?: bigint
  readdirplus?: boolean
  writebackCache?: boolean
  cacheSymlinks?: boolean
}

export interface NativeFuseSessionOptions {
  maxRequest?: number
  useDriverIno?: boolean
  attrTimeout?: number
  entryTimeout?: number
  negativeTimeout?: number
  keepCache?: boolean
  flushMechanism?: "sync" | "enosys" | "noflush"
  init?: NativeFuseInitPreferences
}

export type FuseFlushMechanism = "sync" | "enosys" | "noflush"

export interface FuseRequest {
  header: NativeFuseInHeader
  payload: Buffer
  extensions: Buffer
  body: unknown
}

export type FuseSessionOptions = Omit<NativeFuseSessionOptions, "flushMechanism"> & {
  flushMechanism?: FuseFlushMechanism
  debug?: boolean
  onError?: (error: unknown, request?: FuseRequest) => void
  onAssertion?: (message: string) => void
}

export interface FuseSessionStats {
  requests: number
  replies: number
  errors: number
  noReply: number
  dropped: number
  assertions: number
}

export interface FuseSessionInode {
  readonly nodeid: bigint
  readonly key: string | undefined
  readonly nlookup: bigint
  readonly paths: Set<string>
}

export interface FuseSessionInodeTable {
  readonly root: FuseSessionInode | undefined
  readonly size: number
  readonly pathCount: number
  get(nodeid: bigint): FuseSessionInode | undefined
  at(path: string): FuseSessionInode | undefined
  require(nodeid: bigint): FuseSessionInode
  pathOf(inode: FuseSessionInode): string
  requirePath(nodeid: bigint): string
}

export interface NativeFuseSessionError {
  code: string
  errno: number
  syscall?: string
  path?: string
  dest?: string
  message: string
}

export interface NativeFuseSessionObservation {
  reply?: Buffer
  error?: NativeFuseSessionError
}

export interface NativeFuseNegotiatedSession {
  major: number
  minor: number
  flags: bigint
  maxWrite: number
  maxPages: number
  maxReadahead: number
  maxBackground: number
  congestionThreshold: number
  timeGran: number
  readdirplus: boolean
  writebackCache: boolean
  atomicOTrunc: boolean
  parallelDirops: boolean
  posixLocks: boolean
  flockLocks: boolean
  cacheSymlinks: boolean
  exportSupport: boolean
  setxattrExt: boolean
  maxStackDepth: number
  protocol: NativeFuseProtocolContext
}

export interface NativeFuseSessionInode {
  nodeid: bigint
  key?: string
  nlookup: bigint
  paths: string[]
}

export interface NativeFuseSessionState {
  destroyed: boolean
  openHandles: number
  negotiated?: NativeFuseNegotiatedSession
  inodes: NativeFuseSessionInode[]
}

/** Serialized Rust-backed FUSE requests; it never opens a native device. */
export declare class FuseSession {
  constructor(filesystem: import("../index.js").Filesystem, options?: FuseSessionOptions | null)
  readonly options: FuseSessionOptions
  readonly stats: FuseSessionStats
  readonly assertions: string[]
  readonly negotiated: NativeFuseNegotiatedSession | undefined
  readonly protocol: NativeFuseProtocolContext | undefined
  readonly destroyed: boolean
  readonly openHandles: number
  readonly inodes: FuseSessionInodeTable
  handle(bytes: Uint8Array): Promise<Buffer | null>
  handleMessage(bytes: Uint8Array): Promise<Buffer | null>
  destroy(): Promise<void>
  notifyInvalInode(ino: bigint, off?: bigint, len?: bigint): Buffer
  notifyInvalEntry(parent: bigint, name: string, flags?: number): Buffer
}

export declare const DEFAULT_ATTR_TIMEOUT: 10
export declare const DEFAULT_ENTRY_TIMEOUT: 10
export declare const DEFAULT_FLUSH_MECHANISM: FuseFlushMechanism
export declare function createFuseSession(filesystem: import("../index.js").Filesystem, options?: FuseSessionOptions | null): FuseSession

export declare const INODE_GENERATION: bigint

/** Rust-backed path/nodeid state used by the FUSE session. */
export declare class InodeTable {
  constructor(options?: InodeTableOptions)
  readonly root: Inode
  readonly size: number
  readonly pathCount: number
  get(nodeid: bigint): Inode | undefined
  at(path: string): Inode | undefined
  require(nodeid: bigint): Inode
  pathOf(inode: Inode): string
  requirePath(nodeid: bigint): string
  bind(path: string, stats: { dev: number; ino: number }): Inode
  acquire(inode: Inode): Inode
  forget(nodeid: bigint, count: bigint): boolean
  unbind(path: string): Inode | undefined
  remap(from: string, to: string): void
  nodeids(): bigint[]
  clear(): void
}

export declare class TranscriptRecorder {
  constructor(options?: { limit?: number; now?: () => bigint })
  readonly frames: Array<NativeFuseTranscriptFrame>
  readonly truncated: boolean
  readonly bytes: number
  tap(direction: "in" | "out", bytes: Uint8Array): void
  encode(): Buffer
}

export declare const FUSE_NAME_MAX: number
export declare const FUSE_KERNEL_VERSION: number
export declare const FUSE_KERNEL_MINOR_VERSION: number
export declare const FUSE_ROOT_ID: bigint
export declare const FUSE_RELEASE: number
export declare const FUSE_RELEASEDIR: number
export declare const FUSE_FLUSH: number
export declare const FUSE_FSYNC: number
export declare const FUSE_FSYNCDIR: number
export declare const FUSE_RELEASE_FLUSH: number
export declare const FUSE_RELEASE_FLOCK_UNLOCK: number
export declare const FUSE_FSYNC_FDATASYNC: number
export declare const FUSE_GETATTR: number
export declare const FUSE_SETATTR: number
export declare const FUSE_GETATTR_FH: number
export declare const FUSE_READLINK: number
export declare const FUSE_STATFS: number
export declare const FUSE_INTERRUPT: number
export declare const FUSE_IOCTL: number
export declare const FUSE_BATCH_FORGET: number
export declare const FUSE_POLL: number
export declare const FUSE_BMAP: number
export declare const FUSE_SETXATTR: number
export declare const FUSE_GETXATTR: number
export declare const FUSE_LISTXATTR: number
export declare const FUSE_REMOVEXATTR: number
export declare const FUSE_ASYNC_DIO: bigint
export declare const FUSE_PARALLEL_DIROPS: bigint
export declare const FUSE_GETLK: number
export declare const FUSE_SETLK: number
export declare const FUSE_SETLKW: number
export declare const FUSE_LK_FLOCK: number
export declare const F_RDLCK: number
export declare const F_WRLCK: number
export declare const F_UNLCK: number
export declare const FUSE_SETXATTR_EXT: bigint
export declare const FUSE_SETXATTR_ACL_KILL_SGID: number
export declare const XATTR_CREATE: number
export declare const XATTR_REPLACE: number
export declare const FATTR_MODE: number
export declare const FATTR_SIZE: number
export declare const FATTR_ATIME: number
export declare const FATTR_MTIME: number
export declare const FUSE_IN_HEADER_SIZE: number
export declare const FUSE_OUT_HEADER_SIZE: number
export declare const DEFAULT_MAX_WRITE: number
export declare const DEFAULT_PROTOCOL: Readonly<NativeFuseProtocolContext>
export declare const OPCODE_NAMES: Readonly<Record<number, string>>
export declare const SUPPORTED_OPCODES: ReadonlyArray<number>
export declare const UNIMPLEMENTED_OPCODES: ReadonlyArray<number>

export declare function encodeNotify(code: number, body: Uint8Array): Buffer
export declare function decodeNotify(message: Uint8Array): NativeFuseNotification
export declare function encodeNotifyInvalInode(value: NativeFuseNotifyInvalInodeOut): Buffer
export declare function decodeNotifyInvalInode(body: Uint8Array): NativeFuseNotifyInvalInodeOut
export declare function encodeNotifyInvalEntry(value: NativeFuseNotifyInvalEntryOut): Buffer
export declare function decodeNotifyInvalEntry(body: Uint8Array): NativeFuseNotifyInvalEntryOut
export declare function encodeTranscript(frames: Array<NativeFuseTranscriptFrame>): Buffer
export declare function decodeTranscript(bytes: Uint8Array): Array<NativeFuseTranscriptFrame>
export declare function decodeInHeader(bytes: Uint8Array): NativeFuseInHeader
export declare function encodeInHeader(value: NativeFuseInHeader): Buffer
export declare function decodeOutHeader(bytes: Uint8Array): NativeFuseOutHeader
export declare function encodeOutHeader(value: NativeFuseOutHeader): Buffer
export declare function writeOutHeaderInto(target: Uint8Array, value: NativeFuseOutHeader): void
export declare function fuseErrno(code: number | string): number
export declare function allocReply(size: number): { message: Buffer; body: Buffer }
export declare function finishReply(reply: { message: Buffer; body: Buffer }, unique: bigint, bytesUsed?: number): Buffer
export declare function encodeReply(unique: bigint, body?: Uint8Array): Buffer
export declare function encodeErrorReply(unique: bigint, code: number | string): Buffer
export declare function encodeErrorReplyFor(unique: bigint, error: { errno?: number }): Buffer
export declare function attrSize(minor: number): number
export declare function entryOutSize(minor: number): number
export declare function attrOutSize(minor: number): number
export declare function kstatfsSize(minor: number): number
export declare function initOutSize(minor: number): number
export declare function readWriteInSize(minor: number): number
export declare function decodeEntryOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseEntryOut
export declare function encodeEntryOut(value: NativeFuseEntryOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeLookupIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseNameIn
export declare function encodeLookupIn(value: NativeFuseNameIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeLookupOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseEntryOut
export declare function encodeLookupOut(value: NativeFuseEntryOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeSymlinkIn(body: Uint8Array): NativeFuseSymlinkIn
export declare function encodeSymlinkIn(value: NativeFuseSymlinkIn): Buffer
export declare function decodeSymlinkOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseEntryOut
export declare function encodeSymlinkOut(value: NativeFuseEntryOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeMknodIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseMknodIn
export declare function encodeMknodIn(value: NativeFuseMknodIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeMknodOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseEntryOut
export declare function encodeMknodOut(value: NativeFuseEntryOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeMkdirIn(body: Uint8Array): NativeFuseMkdirIn
export declare function encodeMkdirIn(value: NativeFuseMkdirIn): Buffer
export declare function decodeMkdirOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseEntryOut
export declare function encodeMkdirOut(value: NativeFuseEntryOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeUnlinkIn(body: Uint8Array): NativeFuseNameIn
export declare function encodeUnlinkIn(value: NativeFuseNameIn): Buffer
export declare function decodeRmdirIn(body: Uint8Array): NativeFuseNameIn
export declare function encodeRmdirIn(value: NativeFuseNameIn): Buffer
export declare function decodeRenameIn(body: Uint8Array): NativeFuseRenameIn
export declare function encodeRenameIn(value: NativeFuseRenameIn): Buffer
export declare function decodeRename2In(body: Uint8Array): NativeFuseRename2In
export declare function encodeRename2In(value: NativeFuseRename2In): Buffer
export declare function decodeLinkIn(body: Uint8Array): NativeFuseLinkIn
export declare function encodeLinkIn(value: NativeFuseLinkIn): Buffer
export declare function decodeLinkOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseEntryOut
export declare function encodeLinkOut(value: NativeFuseEntryOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeAccessIn(body: Uint8Array): NativeFuseAccessIn
export declare function encodeAccessIn(value: NativeFuseAccessIn): Buffer
export declare function decodeAttrOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseAttrOut
export declare function encodeAttrOut(value: NativeFuseAttrOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeGetattrIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseGetattrIn
export declare function encodeGetattrIn(value: NativeFuseGetattrIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeGetattrOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseAttrOut
export declare function encodeGetattrOut(value: NativeFuseAttrOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeSetattrIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseSetattrIn
export declare function encodeSetattrIn(value: NativeFuseSetattrIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeSetattrOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseAttrOut
export declare function encodeSetattrOut(value: NativeFuseAttrOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeOpenIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseOpenIn
export declare function encodeOpenIn(value: NativeFuseOpenIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeOpenOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseOpenOut
export declare function encodeOpenOut(value: NativeFuseOpenOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeCreateIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseCreateIn
export declare function encodeCreateIn(value: NativeFuseCreateIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeCreateOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseCreateOut
export declare function encodeCreateOut(value: NativeFuseCreateOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeBatchForgetIn(body: Uint8Array): NativeFuseBatchForgetIn
export declare function encodeBatchForgetIn(value: NativeFuseBatchForgetIn): Buffer
export declare function decodeReadlinkIn(body: Uint8Array): NativeFuseEmpty
export declare function encodeReadlinkIn(value: NativeFuseEmpty): Buffer
export declare function decodeReadlinkOut(body: Uint8Array): NativeFuseReadlinkOut
export declare function encodeReadlinkOut(value: NativeFuseReadlinkOut): Buffer
export declare function decodeReleaseIn(body: Uint8Array): NativeFuseReleaseIn
export declare function encodeReleaseIn(value: NativeFuseReleaseIn): Buffer
export declare function decodeFlushIn(body: Uint8Array): NativeFuseFlushIn
export declare function encodeFlushIn(value: NativeFuseFlushIn): Buffer
export declare function decodeFsyncIn(body: Uint8Array): NativeFuseFsyncIn
export declare function encodeFsyncIn(value: NativeFuseFsyncIn): Buffer
export declare function decodeSyncfsIn(body: Uint8Array): NativeFuseSyncfsIn
export declare function encodeSyncfsIn(value: NativeFuseSyncfsIn): Buffer
export declare function decodeLkIn(body: Uint8Array): NativeFuseLkIn
export declare function encodeLkIn(value: NativeFuseLkIn): Buffer
export declare function decodeLkOut(body: Uint8Array): NativeFuseLkOut
export declare function encodeLkOut(value: NativeFuseLkOut): Buffer
export declare function decodeSetxattrIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseSetxattrIn
export declare function encodeSetxattrIn(value: NativeFuseSetxattrIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeGetxattrIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseGetxattrIn
export declare function encodeGetxattrIn(value: NativeFuseGetxattrIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeListxattrIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseListxattrIn
export declare function encodeListxattrIn(value: NativeFuseListxattrIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeRemovexattrIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseNameIn
export declare function encodeRemovexattrIn(value: NativeFuseNameIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeReadIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseReadIn
export declare function encodeReadIn(value: NativeFuseReadIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeReadOut(body: Uint8Array): NativeFuseRawData
export declare function encodeReadOut(value: NativeFuseRawData): Buffer
export declare function decodeWriteIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseWriteIn
export declare function encodeWriteIn(value: NativeFuseWriteIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeWriteOut(body: Uint8Array): NativeFuseWriteOut
export declare function encodeWriteOut(value: NativeFuseWriteOut): Buffer
export declare function decodeInitIn(body: Uint8Array): NativeFuseInitIn
export declare function encodeInitIn(value: NativeFuseInitIn): Buffer
export declare function decodeInitOut(body: Uint8Array): NativeFuseInitOut
export declare function encodeInitOut(value: NativeFuseInitOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeStatfsIn(body: Uint8Array): NativeFuseEmpty
export declare function encodeStatfsIn(value: NativeFuseEmpty): Buffer
export declare function decodeStatfsOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseKstatfs
export declare function encodeStatfsOut(value: NativeFuseKstatfs, context?: NativeFuseProtocolContext): Buffer
export declare function decodeInterruptIn(body: Uint8Array): NativeFuseInterruptIn
export declare function encodeInterruptIn(value: NativeFuseInterruptIn): Buffer
export declare function decodeIoctlIn(body: Uint8Array): NativeFuseIoctlIn
export declare function encodeIoctlIn(value: NativeFuseIoctlIn): Buffer
export declare function decodeIoctlOut(body: Uint8Array): NativeFuseIoctlOut
export declare function encodeIoctlOut(value: NativeFuseIoctlOut): Buffer
export declare function decodePollIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFusePollIn
export declare function encodePollIn(value: NativeFusePollIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodePollOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFusePollOut
export declare function encodePollOut(value: NativeFusePollOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeBmapIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseBmapIn
export declare function encodeBmapIn(value: NativeFuseBmapIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodeBmapOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFuseBmapOut
export declare function encodeBmapOut(value: NativeFuseBmapOut, context?: NativeFuseProtocolContext): Buffer
export declare function decodeFallocateIn(body: Uint8Array): NativeFuseFallocateIn
export declare function encodeFallocateIn(value: NativeFuseFallocateIn): Buffer
export declare function decodeLseekIn(body: Uint8Array): NativeFuseLseekIn
export declare function encodeLseekIn(value: NativeFuseLseekIn): Buffer
export declare function decodeLseekOut(body: Uint8Array): NativeFuseLseekOut
export declare function encodeLseekOut(value: NativeFuseLseekOut): Buffer
export declare function decodeGetxattrOut(body: Uint8Array): NativeFuseGetxattrOut
export declare function encodeGetxattrOut(value: NativeFuseGetxattrOut): Buffer
export declare function encodeXattrNames(names: Array<string>): Buffer
export declare function decodeXattrNames(body: Uint8Array): Array<string>
export declare function joinInitFlags(flags: number, flags2: number): bigint
export declare function splitInitFlags(flags: bigint): NativeFuseSplitInitFlags
export declare function direntAlign(size: number): number
export declare function direntSize(nameByteLength: number): number
export declare function direntPlusSize(nameByteLength: number, context?: NativeFuseProtocolContext): number
export declare function direntType(mode: number): number
export declare function translateOpenFlags(wire: number, host: { O_CREAT: number; O_EXCL: number; O_TRUNC: number; O_APPEND: number }): number
export declare function driverOpenFlags(wire: number, platform?: string, host?: object): number
export declare function reopenFlags(flags: number): number
export declare function nameByteLength(name: string): number
export declare function opcodeName(opcode: number): string

export * from "../index.js"
