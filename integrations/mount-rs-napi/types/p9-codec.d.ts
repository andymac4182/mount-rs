/// <reference lib="esnext.disposable" />

import type {
  FileHandle,
  Filesystem,
  FsDriver,
  Mounted,
  P9Connection,
  P9FidTable,
  P9LockTable,
  P9Server,
  NativeP9Header,
} from "../index.js"

// The 9P entrypoint retains the existing package exports (including the P9
// server) and adds the Rust-backed 9P2000.L codec surface below.
export * from "../index.js"

export declare class P9Error extends Error {
  readonly code: "ERR_9P_WIRE"
  readonly offset: number | undefined
  constructor(message: string, options?: { offset?: number; cause?: unknown })
}

export declare function isP9Error(error: unknown): error is P9Error

export interface P9Qid {
  type: number
  version: number
  path: bigint
}

export interface P9Time {
  sec: bigint
  nsec: bigint
}

export interface P9Header {
  size: number
  type: number
  tag: number
}

export interface P9Message extends P9Header {
  body: P9Reader
}

/** The first qid.path allocated by a fid table. */
export declare const FIRST_QID_PATH: 1n

export interface FidOpenState {
  flags: number
  handle: FileHandle | undefined
  directory: boolean
  qid?: P9Qid
}

export interface DirCursor<TDirent = string> {
  readonly entries: readonly TDirent[]
  readonly offsets: ReadonlyMap<bigint, number>
}

export interface DirResume<TDirent = string> {
  readonly entries: readonly TDirent[]
  readonly index: number
}

export interface Fid<TDirent = string> {
  readonly fid: number
  path: string
  open: FidOpenState | undefined
  readonly iounit: number
  readonly cursor: DirCursor<TDirent> | undefined
}

export interface FidTableOptions {
  useDriverIno?: boolean
}

/** Runtime alias for the Rust-backed fid table class. */
export declare const FidTable: typeof P9FidTable
export type FidTable<TDirent = string> = P9FidTable

export declare function qidType(mode: number): number
export declare function qidVersion(stats: { mtimeMs: number }): number
export declare function walkStep(path: string, name: string): string

export declare const P9_QID_SIZE: 13
export declare const P9_MAX_STRING: 65535
export declare const P9_MAX_ITEM: 16777216
export declare const P9_DEFAULT_MAX_FRAME: 1048576
export declare const P9_LOCK_TYPE_RDLCK: 0
export declare const P9_LOCK_TYPE_WRLCK: 1
export declare const P9_LOCK_TYPE_UNLCK: 2
export declare const P9_LOCK_SUCCESS: 0
export declare const P9_LOCK_BLOCKED: 1
export declare const P9_LOCK_ERROR: 2
export declare const P9_LOCK_GRACE: 3
export declare const P9_LOCK_FLAGS_BLOCK: 1
export declare const P9_LOCK_FLAGS_RECLAIM: 2
export declare const P9_TLERROR: 6
export declare const P9_RLERROR: 7
export declare const P9_TSTATFS: 8
export declare const P9_RSTATFS: 9
export declare const P9_TLOPEN: 12
export declare const P9_RLOPEN: 13
export declare const P9_TLCREATE: 14
export declare const P9_RLCREATE: 15
export declare const P9_TSYMLINK: 16
export declare const P9_RSYMLINK: 17
export declare const P9_TMKNOD: 18
export declare const P9_RMKNOD: 19
export declare const P9_TRENAME: 20
export declare const P9_RRENAME: 21
export declare const P9_TREADLINK: 22
export declare const P9_RREADLINK: 23
export declare const P9_TGETATTR: 24
export declare const P9_RGETATTR: 25
export declare const P9_TSETATTR: 26
export declare const P9_RSETATTR: 27
export declare const P9_TXATTRWALK: 30
export declare const P9_RXATTRWALK: 31
export declare const P9_TXATTRCREATE: 32
export declare const P9_RXATTRCREATE: 33
export declare const P9_TREADDIR: 40
export declare const P9_RREADDIR: 41
export declare const P9_TFSYNC: 50
export declare const P9_RFSYNC: 51
export declare const P9_TLOCK: 52
export declare const P9_RLOCK: 53
export declare const P9_TGETLOCK: 54
export declare const P9_RGETLOCK: 55
export declare const P9_TLINK: 70
export declare const P9_RLINK: 71
export declare const P9_TMKDIR: 72
export declare const P9_RMKDIR: 73
export declare const P9_TRENAMEAT: 74
export declare const P9_RRENAMEAT: 75
export declare const P9_TUNLINKAT: 76
export declare const P9_RUNLINKAT: 77
export declare const P9_TVERSION: 100
export declare const P9_RVERSION: 101
export declare const P9_TAUTH: 102
export declare const P9_RAUTH: 103
export declare const P9_TATTACH: 104
export declare const P9_RATTACH: 105
export declare const P9_TERROR: 106
export declare const P9_RERROR: 107
export declare const P9_TFLUSH: 108
export declare const P9_RFLUSH: 109
export declare const P9_TWALK: 110
export declare const P9_RWALK: 111
export declare const P9_TOPEN: 112
export declare const P9_ROPEN: 113
export declare const P9_TCREATE: 114
export declare const P9_RCREATE: 115
export declare const P9_TREAD: 116
export declare const P9_RREAD: 117
export declare const P9_TWRITE: 118
export declare const P9_RWRITE: 119
export declare const P9_TCLUNK: 120
export declare const P9_RCLUNK: 121
export declare const P9_TREMOVE: 122
export declare const P9_RREMOVE: 123
export declare const P9_TSTAT: 124
export declare const P9_RSTAT: 125
export declare const P9_TWSTAT: 126
export declare const P9_RWSTAT: 127
export declare const MESSAGE_NAMES: Readonly<Record<number, string>>
export declare function messageName(type: number): string
export declare const P9_GETATTR_MODE: 0x00000001n
export declare const P9_GETATTR_NLINK: 0x00000002n
export declare const P9_GETATTR_UID: 0x00000004n
export declare const P9_GETATTR_GID: 0x00000008n
export declare const P9_GETATTR_RDEV: 0x00000010n
export declare const P9_GETATTR_ATIME: 0x00000020n
export declare const P9_GETATTR_MTIME: 0x00000040n
export declare const P9_GETATTR_CTIME: 0x00000080n
export declare const P9_GETATTR_INO: 0x00000100n
export declare const P9_GETATTR_SIZE: 0x00000200n
export declare const P9_GETATTR_BLOCKS: 0x00000400n
export declare const P9_GETATTR_BTIME: 0x00000800n
export declare const P9_GETATTR_GEN: 0x00001000n
export declare const P9_GETATTR_DATA_VERSION: 0x00002000n
export declare const P9_GETATTR_BASIC: 0x000007ffn
export declare const P9_GETATTR_ALL: 0x00003fffn
export declare const P9_SETATTR_MODE: 1
export declare const P9_SETATTR_UID: 2
export declare const P9_SETATTR_GID: 4
export declare const P9_SETATTR_SIZE: 8
export declare const P9_SETATTR_ATIME: 16
export declare const P9_SETATTR_MTIME: 32
export declare const P9_SETATTR_CTIME: 64
export declare const P9_SETATTR_ATIME_SET: 128
export declare const P9_SETATTR_MTIME_SET: 256
export declare const P9_QTDIR: 128
export declare const P9_QTAPPEND: 64
export declare const P9_QTEXCL: 32
export declare const P9_QTMOUNT: 16
export declare const P9_QTAUTH: 8
export declare const P9_QTTMP: 4
export declare const P9_QTSYMLINK: 2
export declare const P9_QTLINK: 1
export declare const P9_QTFILE: 0
export declare const P9_NOTAG: 65535
export declare const P9_NOFID: 4294967295
export declare const P9_MAXWELEM: 16
export declare const P9_HDRSZ: 7
export declare const P9_IOHDRSZ: 24
export declare const P9_READDIRHDRSZ: 24
export declare const P9_DOTL_AT_REMOVEDIR: 512
export declare const P9_VERSION_DOTL: "9P2000.L"
export declare const P9_VERSION_UNKNOWN: "unknown"
export declare const P9_MIN_MSIZE: 4096
export declare const V9FS_MAGIC: 0x01021997
export declare const DEFAULT_P9_PORT: 564
export declare const DEFAULT_SOCKET_MODE: 384
export declare const DEFAULT_MAX_IN_FLIGHT: 16
export declare const DEFAULT_MSIZE: 1048576
export declare const P9_LOCK_EOF_END: 0x10000000000000000n
export declare const DEFAULT_MAX_LOCKS_PER_FILE: 1024

export declare const P9_DEFAULT_MOUNT_MSIZE: 131096
export declare const P9_MAX_MOUNT_MSIZE: 1048576
export declare const P9_UNIX_PATH_MAX: 108

export interface P9MountTarget {
  trans: "unix" | "tcp"
  port?: number
}

/** The N-API 9P mount-helper option subset supported by this package. */
export interface MountP9Options {
  /** Reuse a configured native server and its policy/lock table. */
  server?: P9Server
  transport?: "unix" | "tcp"
  host?: string
  port?: number
  path?: string
  /** Scalar policy for a server created by this mount. */
  allowRemote?: boolean
  socketMode?: number
  allowSharedDirectory?: boolean
  maxFrame?: number
  maxInFlight?: number
  msize?: number
  mountMsize?: number
  access?: string
  cache?: string
  uname?: string
  aname?: string
  readOnly?: boolean
  useDriverIno?: boolean
  claimOwnership?: boolean
  debug?: boolean
  locks?: P9LockTable
  onError?: (error: unknown, header: NativeP9Header | undefined) => void
  onAssertion?: (message: string) => void
  /** Unmount on SIGINT/SIGTERM. Default: true for direct ./9p mounts. */
  signals?: boolean
  mountOptions?: readonly string[]
  unmountTimeout?: number
  onTransportError?: (error: unknown, peer: string | undefined) => void
}

export interface P9Mount extends Mounted {
  readonly transport: "9p"
  readonly source: string
  readonly trans: "unix" | "tcp"
  readonly server: P9Server
  readonly connection: P9Connection
  readonly closed: Promise<void>
  waitClosed(): Promise<void>
}

export interface P9ClientProbe {
  usable: boolean
  platform?: "linux"
  kernel: boolean
  transport: boolean
  modules: boolean
  root: boolean
  reason?: string
}

export declare function p9ClientProbe(platform?: NodeJS.Platform): P9ClientProbe
export declare function p9Platform(platform?: NodeJS.Platform): "linux" | undefined
export declare function socketPathRefusal(path: string): string | undefined
export declare function tcpSourceRefusal(host: string): string | undefined
export declare function p9MountOptions(target: P9MountTarget, options?: MountP9Options): string
export declare function mount9p(
  driver: Filesystem | FsDriver,
  mountpoint: string,
  options?: MountP9Options,
): Promise<P9Mount>
export declare function live9pMounts(): Promise<Array<P9Mount>>
export declare function unmountAll9p(): Promise<Array<{ transport?: string; message: string }>>

export declare class P9Reader {
  constructor(bytes: Uint8Array, offset?: number)
  readonly bytes: Uint8Array
  readonly offset: number
  readonly remaining: number
  readonly atEnd: boolean
  u8(what?: string): number
  u16(what?: string): number
  u32(what?: string): number
  u64(what?: string): bigint
  string(max?: number, what?: string): string
  qid(what?: string): P9Qid
  blob(max?: number, what?: string): Uint8Array
  raw(count: number, what?: string): Uint8Array
  rest(): Uint8Array
  end(what?: string): void
  readHeader(): P9Header
  readTime(what?: string): P9Time
  readTversion(): Tversion
  readRversion(): Rversion
  readTauth(): Tauth
  readRauth(): Rauth
  readTattach(): Tattach
  readRattach(): Rattach
  readRlerror(): Rlerror
  readTflush(): Tflush
  readTwalk(): Twalk
  readRwalk(): Rwalk
  readTread(): Tread
  readRread(): Rread
  readTwrite(): Twrite
  readRwrite(): Rwrite
  readFidRequest(): FidRequest
  readRstatfs(): Rstatfs
  readTlopen(): Tlopen
  readRlopen(): Rlopen
  readTlcreate(): Tlcreate
  readTsymlink(): Tsymlink
  readQidReply(): QidReply
  readTmknod(): Tmknod
  readTmkdir(): Tmkdir
  readTrename(): Trename
  readTrenameat(): Trenameat
  readTunlinkat(): Tunlinkat
  readTlink(): Tlink
  readRreadlink(): Rreadlink
  readTgetattr(): Tgetattr
  readRgetattr(): Rgetattr
  readTsetattr(): Tsetattr
  readTxattrwalk(): Txattrwalk
  readRxattrwalk(): Rxattrwalk
  readTxattrcreate(): Txattrcreate
  readTreaddir(): Treaddir
  readRreaddir(): Rreaddir
  readDirent(): P9Dirent
  readDirents(): P9Dirent[]
  readTfsync(): Tfsync
  readTlock(): Tlock
  readRlock(): Rlock
  readTgetlock(): Tgetlock
  readRgetlock(): Rgetlock
}

export declare class P9Writer {
  constructor(capacity?: number)
  readonly length: number
  u8(value: number): this
  u16(value: number): this
  u32(value: number): this
  u64(value: bigint): this
  string(value: string): this
  qid(value: P9Qid): this
  blob(value: Uint8Array): this
  raw(value: Uint8Array): this
  patchU32(at: number, value: number): this
  bytes(): Uint8Array
  writeHeader(header: P9Header): void
  writeTime(value: P9Time): void
  writeTversion(value: Tversion): void
  writeRversion(value: Rversion): void
  writeTauth(value: Tauth): void
  writeRauth(value: Rauth): void
  writeTattach(value: Tattach): void
  writeRattach(value: Rattach): void
  writeRlerror(value: Rlerror): void
  writeTflush(value: Tflush): void
  writeTwalk(value: Twalk): void
  writeRwalk(value: Rwalk): void
  writeTread(value: Tread): void
  writeRread(value: Rread): void
  writeTwrite(value: Twrite): void
  writeRwrite(value: Rwrite): void
  writeFidRequest(value: FidRequest): void
  writeRstatfs(value: Rstatfs): void
  writeTlopen(value: Tlopen): void
  writeRlopen(value: Rlopen): void
  writeTlcreate(value: Tlcreate): void
  writeTsymlink(value: Tsymlink): void
  writeQidReply(value: QidReply): void
  writeTmknod(value: Tmknod): void
  writeTmkdir(value: Tmkdir): void
  writeTrename(value: Trename): void
  writeTrenameat(value: Trenameat): void
  writeTunlinkat(value: Tunlinkat): void
  writeTlink(value: Tlink): void
  writeRreadlink(value: Rreadlink): void
  writeTgetattr(value: Tgetattr): void
  writeRgetattr(value: Rgetattr): void
  writeTsetattr(value: Tsetattr): void
  writeTxattrwalk(value: Txattrwalk): void
  writeRxattrwalk(value: Rxattrwalk): void
  writeTxattrcreate(value: Txattrcreate): void
  writeTreaddir(value: Treaddir): void
  writeRreaddir(value: Rreaddir): void
  writeDirent(value: P9Dirent): void
  writeTfsync(value: Tfsync): void
  writeTlock(value: Tlock): void
  writeRlock(value: Rlock): void
  writeTgetlock(value: Tgetlock): void
  writeRgetlock(value: Rgetlock): void
}

export declare class P9FrameAssembler {
  constructor(limit?: number)
  limit: number
  readonly pending: number
  readonly failed: boolean
  reset(): void
  push(chunk: Uint8Array): Uint8Array[]
}

export declare class P9DirentPacker {
  constructor(maxSize: number)
  readonly size: number
  readonly count: number
  readonly remaining: number
  add(value: P9Dirent): boolean
  bytes(): Uint8Array
}

export interface Tversion { msize: number; version: string }
export type Rversion = Tversion
export interface Tauth { afid: number; uname: string; aname: string; nUname: number }
export interface Rauth { aqid: P9Qid }
export interface Tattach { fid: number; afid: number; uname: string; aname: string; nUname: number }
export interface Rattach { qid: P9Qid }
export interface Rlerror { ecode: number }
export interface Tflush { oldtag: number }
export interface Twalk { fid: number; newfid: number; wnames: string[] }
export interface Rwalk { wqids: P9Qid[] }
export interface Tread { fid: number; offset: bigint; count: number }
export interface Rread { data: Uint8Array }
export interface Twrite { fid: number; offset: bigint; data: Uint8Array }
export interface Rwrite { count: number }
export interface FidRequest { fid: number }
export interface Rstatfs {
  type: number; bsize: number; blocks: bigint; bfree: bigint; bavail: bigint
  files: bigint; ffree: bigint; fsid: bigint; namelen: number
}
export interface Tlopen { fid: number; flags: number }
export interface Rlopen { qid: P9Qid; iounit: number }
export type Rlcreate = Rlopen
export interface Tlcreate { fid: number; name: string; flags: number; mode: number; gid: number }
export interface Tsymlink { dfid: number; name: string; symtgt: string; gid: number }
export interface QidReply { qid: P9Qid }
export interface Tmknod { dfid: number; name: string; mode: number; major: number; minor: number; gid: number }
export interface Tmkdir { dfid: number; name: string; mode: number; gid: number }
export interface Trename { fid: number; dfid: number; name: string }
export interface Trenameat { olddirfid: number; oldname: string; newdirfid: number; newname: string }
export interface Tunlinkat { dirfid: number; name: string; flags: number }
export interface Tlink { dfid: number; fid: number; name: string }
export interface Rreadlink { target: string }
export interface Tgetattr { fid: number; requestMask: bigint }
export interface Rgetattr {
  valid: bigint; qid: P9Qid; mode: number; uid: number; gid: number; nlink: bigint
  rdev: bigint; size: bigint; blksize: bigint; blocks: bigint; atime: P9Time
  mtime: P9Time; ctime: P9Time; btime: P9Time; gen: bigint; dataVersion: bigint
}
export interface Tsetattr {
  fid: number; valid: number; mode: number; uid: number; gid: number; size: bigint
  atime: P9Time; mtime: P9Time
}
export interface Txattrwalk { fid: number; newfid: number; name: string }
export interface Rxattrwalk { size: bigint }
export interface Txattrcreate { fid: number; name: string; attrSize: bigint; flags: number }
export interface Treaddir { fid: number; offset: bigint; count: number }
export interface Rreaddir { data: Uint8Array }
export interface P9Dirent { qid: P9Qid; offset: bigint; type: number; name: string }
export interface Tfsync { fid: number; datasync: number }
export interface Tlock {
  fid: number; type: number; flags: number; start: bigint; length: bigint; procId: number; clientId: string
}
export interface Rlock { status: number }
export interface Tgetlock {
  fid: number; type: number; start: bigint; length: bigint; procId: number; clientId: string
}
export interface Rgetlock {
  type: number; start: bigint; length: bigint; procId: number; clientId: string
}

export declare function encodeP9(write: (writer: P9Writer) => void, capacity?: number): Uint8Array
export declare function decodeP9<T>(bytes: Uint8Array, read: (reader: P9Reader) => T, what?: string): T
export declare function readHeader(reader: P9Reader): P9Header
export declare function writeHeader(writer: P9Writer, header: P9Header): void
export declare function readTime(reader: P9Reader, what?: string): P9Time
export declare function writeTime(writer: P9Writer, value: P9Time): void
export declare function encodeMessage(type: number, tag: number, write?: (writer: P9Writer) => void, capacity?: number): Uint8Array
export declare function decodeMessage(bytes: Uint8Array): P9Message
export declare function decodeMessageAs<T>(bytes: Uint8Array, read: (reader: P9Reader) => T): P9Header & { value: T }
export declare function readEmptyBody(reader: P9Reader, what?: string): void
export declare const EMPTY_BODY: ReadonlySet<number>
export declare function stringByteLength(value: string): number
export declare function direntSize(name: string): number
export declare function readDirents(bytes: Uint8Array): P9Dirent[]
export declare function framesFrom(chunks: AsyncIterable<Uint8Array>, limit?: number): AsyncGenerator<Uint8Array>

export declare function writeTversion(writer: P9Writer, value: Tversion): void
export declare function readTversion(reader: P9Reader): Tversion
export declare function writeRversion(writer: P9Writer, value: Rversion): void
export declare function readRversion(reader: P9Reader): Rversion
export declare function writeTauth(writer: P9Writer, value: Tauth): void
export declare function readTauth(reader: P9Reader): Tauth
export declare function writeRauth(writer: P9Writer, value: Rauth): void
export declare function readRauth(reader: P9Reader): Rauth
export declare function writeTattach(writer: P9Writer, value: Tattach): void
export declare function readTattach(reader: P9Reader): Tattach
export declare function writeRattach(writer: P9Writer, value: Rattach): void
export declare function readRattach(reader: P9Reader): Rattach
export declare function writeRlerror(writer: P9Writer, value: Rlerror): void
export declare function readRlerror(reader: P9Reader): Rlerror
export declare function writeTflush(writer: P9Writer, value: Tflush): void
export declare function readTflush(reader: P9Reader): Tflush
export declare function writeTwalk(writer: P9Writer, value: Twalk): void
export declare function readTwalk(reader: P9Reader): Twalk
export declare function writeRwalk(writer: P9Writer, value: Rwalk): void
export declare function readRwalk(reader: P9Reader): Rwalk
export declare function writeTread(writer: P9Writer, value: Tread): void
export declare function readTread(reader: P9Reader): Tread
export declare function writeRread(writer: P9Writer, value: Rread): void
export declare function readRread(reader: P9Reader): Rread
export declare function writeTwrite(writer: P9Writer, value: Twrite): void
export declare function readTwrite(reader: P9Reader): Twrite
export declare function writeRwrite(writer: P9Writer, value: Rwrite): void
export declare function readRwrite(reader: P9Reader): Rwrite
export declare function writeFidRequest(writer: P9Writer, value: FidRequest): void
export declare function readFidRequest(reader: P9Reader): FidRequest
export declare function writeRstatfs(writer: P9Writer, value: Rstatfs): void
export declare function readRstatfs(reader: P9Reader): Rstatfs
export declare function writeTlopen(writer: P9Writer, value: Tlopen): void
export declare function readTlopen(reader: P9Reader): Tlopen
export declare function writeRlopen(writer: P9Writer, value: Rlopen): void
export declare function readRlopen(reader: P9Reader): Rlopen
export declare function writeTlcreate(writer: P9Writer, value: Tlcreate): void
export declare function readTlcreate(reader: P9Reader): Tlcreate
export declare function writeTsymlink(writer: P9Writer, value: Tsymlink): void
export declare function readTsymlink(reader: P9Reader): Tsymlink
export declare function writeQidReply(writer: P9Writer, value: QidReply): void
export declare function readQidReply(reader: P9Reader): QidReply
export declare function writeTmknod(writer: P9Writer, value: Tmknod): void
export declare function readTmknod(reader: P9Reader): Tmknod
export declare function writeTmkdir(writer: P9Writer, value: Tmkdir): void
export declare function readTmkdir(reader: P9Reader): Tmkdir
export declare function writeTrename(writer: P9Writer, value: Trename): void
export declare function readTrename(reader: P9Reader): Trename
export declare function writeTrenameat(writer: P9Writer, value: Trenameat): void
export declare function readTrenameat(reader: P9Reader): Trenameat
export declare function writeTunlinkat(writer: P9Writer, value: Tunlinkat): void
export declare function readTunlinkat(reader: P9Reader): Tunlinkat
export declare function writeTlink(writer: P9Writer, value: Tlink): void
export declare function readTlink(reader: P9Reader): Tlink
export declare function writeRreadlink(writer: P9Writer, value: Rreadlink): void
export declare function readRreadlink(reader: P9Reader): Rreadlink
export declare function writeTgetattr(writer: P9Writer, value: Tgetattr): void
export declare function readTgetattr(reader: P9Reader): Tgetattr
export declare function writeRgetattr(writer: P9Writer, value: Rgetattr): void
export declare function readRgetattr(reader: P9Reader): Rgetattr
export declare function writeTsetattr(writer: P9Writer, value: Tsetattr): void
export declare function readTsetattr(reader: P9Reader): Tsetattr
export declare function writeTxattrwalk(writer: P9Writer, value: Txattrwalk): void
export declare function readTxattrwalk(reader: P9Reader): Txattrwalk
export declare function writeRxattrwalk(writer: P9Writer, value: Rxattrwalk): void
export declare function readRxattrwalk(reader: P9Reader): Rxattrwalk
export declare function writeTxattrcreate(writer: P9Writer, value: Txattrcreate): void
export declare function readTxattrcreate(reader: P9Reader): Txattrcreate
export declare function writeTreaddir(writer: P9Writer, value: Treaddir): void
export declare function readTreaddir(reader: P9Reader): Treaddir
export declare function writeRreaddir(writer: P9Writer, value: Rreaddir): void
export declare function readRreaddir(reader: P9Reader): Rreaddir
export declare function writeDirent(writer: P9Writer, value: P9Dirent): void
export declare function readDirent(reader: P9Reader): P9Dirent
export declare function writeTfsync(writer: P9Writer, value: Tfsync): void
export declare function readTfsync(reader: P9Reader): Tfsync
export declare function writeTlock(writer: P9Writer, value: Tlock): void
export declare function readTlock(reader: P9Reader): Tlock
export declare function writeRlock(writer: P9Writer, value: Rlock): void
export declare function readRlock(reader: P9Reader): Rlock
export declare function writeTgetlock(writer: P9Writer, value: Tgetlock): void
export declare function readTgetlock(reader: P9Reader): Tgetlock
export declare function writeRgetlock(writer: P9Writer, value: Rgetlock): void
export declare function readRgetlock(reader: P9Reader): Rgetlock

export declare function writeTclunk(writer: P9Writer, value: FidRequest): void
export declare function readTclunk(reader: P9Reader): FidRequest
export declare function writeTremove(writer: P9Writer, value: FidRequest): void
export declare function readTremove(reader: P9Reader): FidRequest
export declare function writeTstatfs(writer: P9Writer, value: FidRequest): void
export declare function readTstatfs(reader: P9Reader): FidRequest
export declare function writeTreadlink(writer: P9Writer, value: FidRequest): void
export declare function readTreadlink(reader: P9Reader): FidRequest
export declare function writeRsymlink(writer: P9Writer, value: QidReply): void
export declare function readRsymlink(reader: P9Reader): QidReply
export declare function writeRmknod(writer: P9Writer, value: QidReply): void
export declare function readRmknod(reader: P9Reader): QidReply
export declare function writeRmkdir(writer: P9Writer, value: QidReply): void
export declare function readRmkdir(reader: P9Reader): QidReply
export declare function writeRlcreate(writer: P9Writer, value: Rlcreate): void
export declare function readRlcreate(reader: P9Reader): Rlcreate
