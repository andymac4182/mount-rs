/// <reference types="node" />

import type { Buffer } from "node:buffer"
import type {
  NativeFuseAttr,
  NativeFuseAttrOut,
  NativeFuseBatchForgetIn,
  NativeFuseCreateIn,
  NativeFuseCreateOut,
  NativeFuseEmpty,
  NativeFuseEntryOut,
  NativeFuseFlushIn,
  NativeFuseForgetOne,
  NativeFuseFsyncIn,
  NativeFuseGetattrIn,
  NativeFuseGetxattrOut,
  NativeFuseInHeader,
  NativeFuseInitIn,
  NativeFuseInitOut,
  NativeFuseInterruptIn,
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
  NativeFuseSetattrIn,
  NativeFuseSplitInitFlags,
  NativeFuseTranscriptFrame,
  NativeFuseWriteIn,
  NativeFuseWriteOut,
} from "../index.js"

export type {
  NativeFuseAttr,
  NativeFuseAttrOut,
  NativeFuseBatchForgetIn,
  NativeFuseCreateIn,
  NativeFuseCreateOut,
  NativeFuseEmpty,
  NativeFuseEntryOut,
  NativeFuseFlushIn,
  NativeFuseForgetOne,
  NativeFuseFsyncIn,
  NativeFuseGetattrIn,
  NativeFuseGetxattrOut,
  NativeFuseInHeader,
  NativeFuseInitIn,
  NativeFuseInitOut,
  NativeFuseInterruptIn,
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
  NativeFuseSetattrIn,
  NativeFuseSplitInitFlags,
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
export declare const FUSE_BATCH_FORGET: number
export declare const FUSE_POLL: number
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
export declare function decodePollIn(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFusePollIn
export declare function encodePollIn(value: NativeFusePollIn, context?: NativeFuseProtocolContext): Buffer
export declare function decodePollOut(body: Uint8Array, context?: NativeFuseProtocolContext): NativeFusePollOut
export declare function encodePollOut(value: NativeFusePollOut, context?: NativeFuseProtocolContext): Buffer
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
