import {
  ERRNO_CODES,
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
} from "@andymac4182/mount-rs"
import {
  createMemoryDriver,
  type MemoryDriver,
  type MemoryDriverOptions,
} from "@andymac4182/mount-rs/drivers/memory"
import {
  createUnstorageDriver,
  type JsUnstorageOptions,
} from "@andymac4182/mount-rs/drivers/unstorage"
import {
  createNfsServer,
  type NfsServer,
  type NfsServerOptions,
} from "@andymac4182/mount-rs/nfs"
import {
  createP9Server,
  type P9Connection,
  type P9Server,
  type P9ServerOptions,
} from "@andymac4182/mount-rs/9p"
import {
  createS3Server,
  type S3Server,
  type S3ServerOptions,
} from "@andymac4182/mount-rs/s3"
import {
  createWebdavServer,
  type WebdavServer,
  type WebdavServerOptions,
} from "@andymac4182/mount-rs/webdav"

const nativeBindingTarget: "native" | "wasm32-wasi" | "wasm32-wasip1" =
  __napiBindingTarget

async function checkFilesystemAndHandles(): Promise<void> {
  const filesystem: Filesystem = Filesystem.memory()
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
  const nfsOptions: NfsServerOptions = {
    host: "127.0.0.1",
    port: 0,
    allowRemote: false,
    verifier: new Uint8Array(8),
  }
  const p9Options: P9ServerOptions = {
    host: "127.0.0.1",
    port: 0,
    path: "/",
    allowRemote: false,
    socketMode: 0o600,
    readOnly: true,
  }
  const s3Options: S3ServerOptions = {
    host: "127.0.0.1",
    port: 0,
    bucket: "mount-rs",
    credentials: {
      accessKeyId: "access-key",
      secretAccessKey: "secret-key",
    },
    region: "us-east-1",
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
  const webdavServer: WebdavServer = createWebdavServer(filesystem, webdavOptions)
  const kvFilesystem: Filesystem = createUnstorageDriver({}, kvOptions)

  const nfsHost: string = nfsServer.host
  const nfsPort: number = nfsServer.port
  const nfsListen: Promise<void> = nfsServer.listen()
  const nfsClose: Promise<void> = nfsServer.close()
  const p9Address: string | null = p9Server.address()
  const p9Connections: Array<P9Connection> = p9Server.clients()
  const p9Path: string | null = p9Server.path
  const p9Listen: Promise<void> = p9Server.listen()
  const p9Close: Promise<void> = p9Server.close()
  const s3Url: string = s3Server.url
  const s3Buckets: Array<string> = s3Server.buckets
  const s3Listen: Promise<void> = s3Server.listen()
  const s3Close: Promise<void> = s3Server.close()
  const webdavUrl: string = webdavServer.url
  const webdavConnections: number = webdavServer.connections
  const webdavListen: Promise<void> = webdavServer.listen()
  const webdavClose: Promise<void> = webdavServer.close()

  const p9Connection: P9Connection = p9Connections[0]
  const connectionId: number = p9Connection.id
  const connectionPeer: string | null = p9Connection.peer
  const connectionClosed: boolean = p9Connection.isClosed
  const connectionClose: Promise<void> = p9Connection.close()
  const connectionWaitClosed: Promise<void> = p9Connection.waitClosed()

  // @ts-expect-error server ports are numbers in the exported subpath options.
  createNfsServer(filesystem, { port: "0" })

  void kvFilesystem
  void nfsHost
  void nfsPort
  void nfsListen
  void nfsClose
  void p9Address
  void p9Path
  void p9Listen
  void p9Close
  void s3Url
  void s3Buckets
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
}

void checkFilesystemAndHandles
void checkFactories
void checkMemorySubpath
void checkUtilities
void checkServerAndKvSubpaths
