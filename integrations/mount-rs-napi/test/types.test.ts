import {
  ERRNO_CODES,
  createDriver,
  mount,
  type FsDriver,
  FileHandle,
  Filesystem,
  PathLock,
  __napiBindingTarget,
  basename,
  createChunkedDriver,
  createMemoryDriver as createRootMemoryDriver,
  createNodeFsDriver,
  dirname,
  errnoOf,
  fileTypeMode,
  fsError,
  isFsError,
  isNormalizedPath,
  isPathInside,
  isSpecialMode,
  joinPath,
  normalizePath,
  rangeError,
  resolvePath,
  splitPath,
  type JsCapabilities,
  type JsAutoMountOptions,
  type Mounted,
  type JsChunkedOptions,
  type JsMemoryOptions,
  type JsReadResult,
  type JsResolvedPath,
  type JsStats,
  type JsStatsFs,
  type JsWriteResult,
  type ErrnoCode,
  type FsError,
  type FsErrorOptions,
} from "@mount-rs/core"
import {
  createMemoryDriver,
  type MemoryDriver,
  type MemoryDriverOptions,
} from "@mount-rs/core/drivers/memory"
import {
  createUnstorageDriver,
  type JsUnstorageOptions,
} from "@mount-rs/core/drivers/unstorage"
import {
  createNfsServer,
  type NfsConnection,
  type Nfs4IdMap,
  type Nfs4StateKnobs,
  type NfsServer,
  type NfsServerOptions,
} from "@mount-rs/core/nfs"
import {
  createP9Server,
  FidTable,
  FIRST_QID_PATH,
  MESSAGE_NAMES,
  P9_GETATTR_ALL,
  P9_NOFID,
  P9_TVERSION,
  P9_VERSION_DOTL,
  messageName,
  qidType,
  qidVersion,
  walkStep,
  type P9AttachOptions,
  type P9Connection,
  type DirCursor,
  type DirResume,
  type Fid,
  type FidOpenState,
  type FidTableOptions,
  type P9Lock,
  type P9LockClient,
  type P9LockHolder,
  type P9LockRequest,
  P9LockTable,
  type P9LockTableOptions,
  type P9Server,
  type P9ServerOptions,
  type P9Session,
  type P9SessionOptions,
  type P9SessionStats,
  type P9User,
} from "@mount-rs/core/9p"
import {
  createS3Server,
  type S3Server,
  type S3ServerOptions,
  type S3RequestHead,
  type S3RequestStreamBody,
  type S3Response,
  type S3SessionOptionsView,
  type S3Session,
  type S3SessionStats,
  type S3StreamResponse,
} from "@mount-rs/core/s3"
import {
  createWebdavServer,
  type WebdavServer,
  type WebdavServerOptions,
} from "@mount-rs/core/webdav"
import type { Duplex } from "node:stream"
import {
  decodeLkIn,
  decodeLkOut,
  encodeLkIn,
  encodeLkOut,
  FUSE_ASYNC_DIO,
  FUSE_PARALLEL_DIROPS,
  FUSE_SETXATTR_EXT,
  FUSE_LK_FLOCK,
  F_UNLCK,
  FuseSession,
  F_WRLCK,
  InodeTable,
  type Inode,
  type NativeFuseFileLock,
  type NativeFuseLkIn,
  type NativeFuseLkOut,
  type FuseSessionOptions,
} from "@mount-rs/core/fuse"

// node:fs/promises and minimal structural drivers satisfy the public boundary.
import * as nodeFs from "node:fs/promises"
function checkStructuralFactories(driver: FsDriver): void {
  createDriver(driver)
  createNfsServer(driver)
  createP9Server(driver)
  createWebdavServer(driver)
  createS3Server(driver)
  createS3Server({ buckets: { structural: driver, native: Filesystem.memory() } })
  void mount(driver, "/typecheck-only")
  void mount(driver, "/typecheck-fuse-callback", {
    transport: "fuse",
    onTransportError(error, peer) {
      void error
      void peer
    },
  })
}
const nodeStructural: FsDriver = nodeFs
void nodeStructural
void checkStructuralFactories

const autoMountOptions: JsAutoMountOptions = {
  transport: "fuse",
  onTransportError: (error, peer) => {
    void error
    void peer
  },
}
void autoMountOptions
declare const mounted: Mounted
const mountedDisposal: Promise<void> = mounted[Symbol.asyncDispose]()
void mountedDisposal
void (FUSE_ASYNC_DIO | FUSE_PARALLEL_DIROPS | FUSE_SETXATTR_EXT)

const nativeBindingTarget: "native" | "wasm32-wasi" | "wasm32-wasip1" =
  __napiBindingTarget

async function checkFilesystemAndHandles(): Promise<void> {
  const filesystem: Filesystem = Filesystem.memory()
  const sessionOptions: FuseSessionOptions = {
    maxRequest: 65_536,
    useDriverIno: false,
    attrTimeout: 1.25,
    entryTimeout: 2.5,
    negativeTimeout: 3.75,
    keepCache: false,
    flushMechanism: "noflush",
    onError: (error, request) => {
      void error
      void request
    },
  }
  const fuseSession = new FuseSession(filesystem, sessionOptions)
  void fuseSession.handle(new Uint8Array())
  void fuseSession.negotiated
  void fuseSession.inodes
  const handle: FileHandle = await filesystem.open("/file", "w+", 0o644)
  const buffer = new Uint8Array(8)

  const writeResult: JsWriteResult = await handle.write(buffer)
  const readResult: JsReadResult = await handle.read(buffer)
  const handleStats: JsStats = await handle.stat()

  const stats: JsStats = await filesystem.stat("/")
  const lstats: JsStats = await filesystem.lstat("/")
  const statsFs: JsStatsFs = await filesystem.statfs("/")
  const capabilities: JsCapabilities = filesystem.capabilities
  const compatibilityCapabilities: JsCapabilities = filesystem.getCapabilities()
  const entries = await filesystem.readdir("/", { withFileTypes: true })

  await filesystem.mkdir("/directory", { recursive: true, mode: 0o755 })
  await filesystem.writeFile("/directory/file", buffer)
  await filesystem.readFile("/directory/file")
  await filesystem.rename("/directory/file", "/directory/renamed")
  await filesystem.link("/directory/renamed", "/directory/hard-link")
  await filesystem.symlink("/directory/renamed", "/directory/symbolic-link")
  await filesystem.readlink("/directory/symbolic-link")
  await filesystem.chmod("/directory/renamed", 0o644)
  await filesystem.chown("/directory/renamed", 1, 2)
  await filesystem.lchown("/directory/symbolic-link", 1, 2)
  await filesystem.truncate("/directory/renamed", 0)
  await filesystem.utimes("/directory/renamed", new Date(), 0)
  await filesystem.lutimes("/directory/symbolic-link", 0, new Date())
  await filesystem.mountx.mknod("/directory/node", 0o600, 0)
  await filesystem.mknod("/directory/node-2", 0o600, 0)
  await filesystem.rmdir("/directory")
  await filesystem.unlink("/directory/renamed")

  await handle.truncate(0)
  await handle.sync()
  await handle.datasync()
  await handle.close()
  await filesystem.shutdown()

  void nativeBindingTarget
  void writeResult
  void readResult
  void handleStats
  void stats
  void lstats
  void statsFs
  void capabilities
  void compatibilityCapabilities
  void entries
}

async function checkFactories(): Promise<void> {
  const chunkedOptions: JsChunkedOptions = {
    metadata: { kind: "memory" },
    blocks: { kind: "memory" },
    chunkSize: 4096,
    uid: 501,
    gid: 20,
    umask: 0o22,
    rootMode: 0o755,
  }
  const chunkedFilesystem: Filesystem = await createChunkedDriver(chunkedOptions)
  const nativeFilesystem: Filesystem = createNodeFsDriver("/tmp", {
    readOnly: true,
  })
  const rootMemoryOptions: JsMemoryOptions = {
    uid: 501,
    gid: 20,
    umask: 0o22,
    rootMode: 0o755,
  }
  const rootMemoryFilesystem: Filesystem = createRootMemoryDriver(rootMemoryOptions)

  await chunkedFilesystem.shutdown()
  await nativeFilesystem.shutdown()
  await rootMemoryFilesystem.shutdown()

  // @ts-expect-error uid is a number in the root memory-driver options.
  createRootMemoryDriver({ uid: "501" })
  createChunkedDriver({
    metadata: { kind: "memory" },
    blocks: { kind: "memory" },
    // @ts-expect-error chunkSize is a number in the chunked-driver options.
    chunkSize: "4096",
  })
}

function checkMemorySubpath(): void {
  const options: MemoryDriverOptions = {
    uid: 501,
    gid: 20,
    umask: 0o22,
    rootMode: 0o755,
  }
  const memoryFilesystem: MemoryDriver = createMemoryDriver(options)
  const defaultMemoryFilesystem: Filesystem = createMemoryDriver()
  const nullOptionsMemoryFilesystem: Filesystem = createMemoryDriver(null)

  // @ts-expect-error uid is a number in the memory subpath options.
  createMemoryDriver({ uid: "501" })

  void memoryFilesystem
  void defaultMemoryFilesystem
  void nullOptionsMemoryFilesystem
}

function checkUtilities(): void {
  const lock = new PathLock()
  const readNumber: Promise<number> = lock.read(() => 42)
  const writeString: Promise<string> = lock.write(async () => "written")

  const errorCode: ErrnoCode = "ENOENT"
  const errorOptions: FsErrorOptions = {
    syscall: "stat",
    path: "/missing",
    cause: new Error("missing"),
  }
  const error: FsError = fsError(errorCode, errorOptions)
  const errorNumber: number = errnoOf(error)
  const errorMatches: boolean = isFsError(error, errorCode)
  const unknown: unknown = error
  if (isFsError(unknown, errorCode)) {
    const narrowedError: FsError = unknown
    void narrowedError
  }

  const normalized: string = normalizePath("/tmp/../var")
  const parts: string[] = splitPath(normalized)
  const resolved: JsResolvedPath = resolvePath(normalized)
  const joined: string = joinPath(...parts)
  const parent: string = dirname(joined)
  const base: string = basename(joined)
  const inside: boolean = isPathInside(joined, parent)
  const mode: number = fileTypeMode(0o100644)
  const special: boolean = isSpecialMode(0)
  const normalizedCheck: boolean = isNormalizedPath(normalized)
  const range: RangeError = rangeError("length", "a non-negative number", 1)
  const enoent: number = ERRNO_CODES.ENOENT

  void readNumber
  void writeString
  void errorNumber
  void errorMatches
  void resolved
  void joined
  void parent
  void base
  void inside
  void mode
  void special
  void normalizedCheck
  void range
  void enoent
}

function checkServerAndKvSubpaths(): void {
  const filesystem = Filesystem.memory()
  const nfs4IdMap: Nfs4IdMap = {
    domain: "example.test",
    users: { root: 0 },
    groups: { root: 0 },
  }
  const nfs4Options: Nfs4StateKnobs = {
    idmap: nfs4IdMap,
    seed: 0x10203040,
    leaseSeconds: 90,
    maxSessions: 4,
    maxForeSlots: 64,
    maxOperations: 64,
    maxRequestSize: 1024 * 1024,
    maxCachedResponseSize: 64 * 1024,
    maxOpensPerFile: 256,
    maxLocksPerFile: 1024,
    requireReclaimComplete: true,
  }
  const nfsOptions: NfsServerOptions = {
    host: "127.0.0.1",
    port: 0,
    allowRemote: false,
    verifier: new Uint8Array(8),
    nfs4: nfs4Options,
  }
  const p9Options: P9ServerOptions = {
    host: "127.0.0.1",
    port: 0,
    path: "/",
    allowRemote: false,
    socketMode: 0o600,
    readOnly: true,
  }
  const p9AttachOptions: P9AttachOptions = {
    peer: "attached-types",
    own: false,
    maxFrame: 8192,
    maxInFlight: 2,
  }
  void p9AttachOptions
  const p9MessageType: number = P9_TVERSION
  const p9NoFid: number = P9_NOFID
  const p9GetattrAll: bigint = P9_GETATTR_ALL
  const p9Version: "9P2000.L" = P9_VERSION_DOTL
  const p9MessageNames: Readonly<Record<number, string>> = MESSAGE_NAMES
  const p9MessageName: string = messageName(p9MessageType)
  const p9FidOptions: FidTableOptions = { useDriverIno: true }
  const p9FidTable: FidTable = new FidTable(p9FidOptions)
  const p9Fid = p9FidTable.create(1, "/")
  const p9FidResume: DirResume<string> = p9FidTable.snapshot(p9Fid, ["entry"])
  const p9FidCursor: DirCursor<string> = {
    entries: ["entry"],
    offsets: new Map<bigint, number>(),
  }
  const p9FidOpen: FidOpenState = {
    flags: 0,
    handle: undefined,
    directory: true,
  }
  const p9FidShape: Fid<string> = {
    fid: 1,
    path: "/",
    open: p9FidOpen,
    iounit: 0,
    cursor: p9FidCursor,
  }
  const p9FirstQidPath: 1n = FIRST_QID_PATH
  const p9QidType: number = qidType(0o100644)
  const p9QidVersion: number = qidVersion({ mtimeMs: 1 })
  const p9Walked: string = walkStep("/", "entry")
  const s3Options: S3ServerOptions = {
    host: "127.0.0.1",
    port: 0,
    bucket: "mount-rs",
    credentials: {
      accessKeyId: "access-key",
      secretAccessKey: "secret-key",
    },
    region: "us-east-1",
    drainTimeout: 1000,
    debug: true,
    onTransportError: (_error, _peer) => {},
  }
  const webdavOptions: WebdavServerOptions = {
    host: "127.0.0.1",
    port: 0,
    credentials: {
      username: "user",
      password: "password",
    },
    realm: "mount-rs",
    debug: false,
    onError(error, head) {
      void error
      void head
    },
  }
  const kvOptions: JsUnstorageOptions = {
    uid: 501,
    gid: 20,
    fileMode: 0o644,
    dirMode: 0o755,
    readOnly: true,
  }

  const nfsServer: NfsServer = createNfsServer(filesystem, nfsOptions)
  const p9Server: P9Server = createP9Server(filesystem, p9Options)
  const s3Server: S3Server = createS3Server(filesystem, s3Options)
  const multiBucket: S3Server = createS3Server({ buckets: { files: filesystem } }, s3Options)
  // @ts-expect-error Every bucket must be a filesystem, not a path or an arbitrary value.
  createS3Server({ buckets: { files: "/tmp/files" } }, s3Options)
  void multiBucket
  const webdavServer: WebdavServer = createWebdavServer(filesystem, webdavOptions)
  const kvFilesystem: Filesystem = createUnstorageDriver({}, kvOptions)

  const nfsHost: string = nfsServer.host
  const nfsPort: number = nfsServer.port
  const nfsConnections: Array<NfsConnection> = nfsServer.clients()
  const nfsListen: Promise<NfsServer> = nfsServer.listen()
  const nfsClose: Promise<void> = nfsServer.close()
  const nfsConnection: NfsConnection = nfsConnections[0]
  const nfsConnectionId: number = nfsConnection.id
  const nfsConnectionPeer: string | null = nfsConnection.peer
  const nfsConnectionClosed: boolean = nfsConnection.isClosed
  const nfsConnectionClose: Promise<void> = nfsConnection.close()
  const nfsConnectionWaitClosed: Promise<void> = nfsConnection.waitClosed()
  const p9Address: string | null = p9Server.address()
  const p9Connections: Array<P9Connection> = p9Server.clients()
  const p9Configured: P9ServerOptions = p9Server.options
  const p9Path: string | null = p9Server.path
  const p9Listen: Promise<P9Server> = p9Server.listen()
  const p9Close: Promise<void> = p9Server.close()
  const s3Url: string = s3Server.url
  const s3Connections: number = s3Server.connections
  const s3Buckets: Array<string> = s3Server.buckets
  const s3Session: S3Session = s3Server.session
  const s3Drivers: Record<string, Filesystem> = s3Session.buckets
  const s3OptionsView: S3SessionOptionsView = s3Session.options
  const s3Assertions: Array<string> = s3Session.assertions
  const s3Head: S3RequestHead = { method: "GET", target: "/", headers: [] }
  const s3RequestBody: S3RequestStreamBody = (async function* () {})()
  const s3Buffered: Promise<S3Response> = s3Session.handleRequest(s3Head)
  const s3Streamed: Promise<S3StreamResponse> = s3Session.handleRequestStream(
    s3Head,
    s3RequestBody,
  )
  const s3Stats: Promise<S3SessionStats> = s3Session.stats()
  void s3Buffered
  void s3Streamed
  void s3Stats
  const s3Listen: Promise<S3Server> = s3Server.listen()
  const s3Close: Promise<void> = s3Server.close()
  const webdavUrl: string = webdavServer.url
  const webdavConnections: number = webdavServer.connections
  const webdavMethodCounts: Map<string, number> = webdavServer.session.stats.methods
  const webdavListen: Promise<WebdavServer> = webdavServer.listen()
  const webdavClose: Promise<void> = webdavServer.close()
  void webdavMethodCounts

  const p9Connection: P9Connection = p9Connections[0]
  const p9Session: P9Session = p9Connection.session
  const p9SessionOptions: P9SessionOptions = p9Session.options
  const p9SessionStats: P9SessionStats = p9Session.stats
  const p9Locks: P9LockClient = p9Session.locks
  const p9LockTable: P9LockTable = p9Locks.table
  const p9LockTableOptions: P9LockTableOptions = { maxLocksPerFile: 2 }
  const p9LockRequest: P9LockRequest = {
    path: "/",
    fid: 1,
    type: 1,
    start: 0n,
    length: 1n,
    procId: 1,
    clientId: "types",
  }
  const p9Lock: P9Lock | undefined = p9LockTable.at("/")[0]
  const p9LockHolder: P9LockHolder | null = p9Locks.getlock(p9LockRequest)
  const p9User: P9User | null = p9Session.userFor(1)
  const connectionId: number = p9Connection.id
  const connectionPeer: string | null | undefined = p9Connection.peer
  const connectionClosed: boolean = p9Connection.isClosed
  const connectionCompletion: Promise<void> = p9Connection.closed
  const attachedP9: P9Connection = p9Server.attach({} as Duplex)
  const attachedP9Peer: string | null | undefined = attachedP9.peer
  const attachedP9Stream: Duplex | undefined = attachedP9.stream
  const attachedP9Call = attachedP9.session.handleCall(Buffer.alloc(0))
  const attachedP9Destroy: Promise<void> = attachedP9.session.destroy()
  const disposal: Promise<void>[] = [
    nfsServer[Symbol.asyncDispose](),
    p9Server[Symbol.asyncDispose](),
    s3Server[Symbol.asyncDispose](),
    webdavServer[Symbol.asyncDispose](),
  ]
  void connectionCompletion
  void attachedP9
  void p9LockTableOptions
  void p9Lock
  void p9LockHolder
  void attachedP9Peer
  void attachedP9Stream
  void attachedP9Call
  void attachedP9Destroy
  void disposal
  const connectionClose: Promise<void> = p9Connection.close()
  const connectionWaitClosed: Promise<void> = p9Connection.waitClosed()

  // @ts-expect-error server ports are numbers in the exported subpath options.
  createNfsServer(filesystem, { port: "0" })

  void kvFilesystem
  void nfsHost
  void nfsPort
  void nfsConnections
  void nfsListen
  void nfsClose
  void nfsConnection
  void nfsConnectionId
  void nfsConnectionPeer
  void nfsConnectionClosed
  void nfsConnectionClose
  void nfsConnectionWaitClosed
  void p9Address
  void p9Configured
  void p9Path
  void p9Listen
  void p9Close
  void s3Url
  void s3Connections
  void s3Buckets
  void s3Drivers
  void s3OptionsView
  void s3Assertions
  void s3Listen
  void s3Close
  void webdavUrl
  void webdavConnections
  void webdavListen
  void webdavClose
  void connectionId
  void connectionPeer
  void connectionClosed
  void connectionClose
  void connectionWaitClosed
  void p9Session
  void p9SessionOptions
  void p9SessionStats
  void p9User
  void p9NoFid
  void p9GetattrAll
  void p9Version
  void p9MessageNames
  void p9MessageName
  void p9FidTable
  void p9FidResume
  void p9FidShape
  void p9FirstQidPath
  void p9QidType
  void p9QidVersion
  void p9Walked
}

function checkFuseInodeSubpath(): void {
  const table = new InodeTable({ useDriverIno: true })
  const root: Inode = table.root
  const inode: Inode = table.bind("/file", { dev: 1, ino: 2 })
  const acquired: Inode = table.acquire(inode)
  const path: string = table.pathOf(acquired)
  const byPath: Inode | undefined = table.at(path)
  const nodeids: bigint[] = table.nodeids()
  const paths: Set<string> = root.paths
  const forgotten: boolean = table.forget(inode.nodeid, 1n)

  void root
  void byPath
  void nodeids
  void paths
  void forgotten
}

function checkFuseLockCodecSubpath(): void {
  const lock: NativeFuseFileLock = {
    start: 1n,
    end: 2n,
    type: F_WRLCK,
    pid: 3,
  }
  const input: NativeFuseLkIn = {
    fh: 4n,
    owner: 5n,
    lk: lock,
    lkFlags: FUSE_LK_FLOCK,
  }
  const reply: NativeFuseLkOut = {
    lk: { ...lock, type: F_UNLCK },
  }
  const requestBytes: Uint8Array = encodeLkIn(input)
  const decodedInput: NativeFuseLkIn = decodeLkIn(requestBytes)
  const replyBytes: Uint8Array = encodeLkOut(reply)
  const decodedReply: NativeFuseLkOut = decodeLkOut(replyBytes)

  void [decodedInput, decodedReply]
}

void checkFilesystemAndHandles
// Public harness types and functions are available from the package root.
import { createLoopback, resolveCapabilities, type Loopback, type ResolvedCapabilities } from "@mount-rs/core"
function checkHarness(driver: FsDriver, native: Filesystem) {
  const loop: Loopback = createLoopback(driver)
  const nativeLoop: Loopback<Filesystem> = createLoopback(native)
  const resolved: ResolvedCapabilities = resolveCapabilities(driver)
  const readOnly: boolean = resolved.readOnly
  const extensions: readonly string[] = resolved.extensions
  // @ts-expect-error mknod is an extension, not a resolved harness capability
  resolved.mknod
  const original: FsDriver = loop.driver
  const read: Promise<Uint8Array> = loop.readFile("/file")
  const write: Promise<void> = loop.writeFile("/file", "data")
  const optionalNowRequired: Promise<void> = loop.unlink("/file")
  // @ts-expect-error paths are strings
  loop.stat(1)
  void [nativeLoop, readOnly, extensions, original, read, write, optionalNowRequired]
}
void checkHarness
void checkFactories
void checkMemorySubpath
void checkUtilities
void checkServerAndKvSubpaths
void checkFuseInodeSubpath
void checkFuseLockCodecSubpath
