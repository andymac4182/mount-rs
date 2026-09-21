import { readFile, writeFile } from "node:fs/promises"

const loader = new URL("./index.js", import.meta.url)
const marker = "require('./postlude.cjs')(module.exports)"
const chunkedExportMarker = "module.exports.createChunkedDriver = nativeBinding.createChunkedDriver"
const facadeExportMarkers = [
  ...["isNormalizedPath", "splitPath", "normalizePath", "resolvePath", "joinPathParts", "dirname", "basename", "isPathInside", "fsError", "rangeError", "isFsError", "errnoOf", "fileTypeMode", "isSpecialMode", "PathLock"].map((name) => `module.exports.${name} = nativeBinding.${name}`),
  "module.exports.ERRNO_CODES = Object.freeze(nativeBinding.errnoCodes())",
  "module.exports.joinPath = (...parts) => nativeBinding.joinPathParts(parts)",
  "module.exports.createUnstorageDriver = nativeBinding.createUnstorageDriver",
  "module.exports.createDriver = nativeBinding.createDriver",
  "module.exports.createNfsServer = nativeBinding.createNfsServer",
  "module.exports.createP9Server = nativeBinding.createP9Server",
  "module.exports.createS3Server = nativeBinding.createS3Server",
  "module.exports.createWebdavServer = nativeBinding.createWebdavServer",
  "module.exports.createMemoryDriver = nativeBinding.createMemoryDriver",
  "module.exports.Mounted = nativeBinding.Mounted",
  "module.exports.createNodeFsDriver = nativeBinding.createNodeFsDriver",
  "module.exports.probeTransports = nativeBinding.probeTransports",
  "module.exports.mount = nativeBinding.mount",
  "module.exports.liveMounts = nativeBinding.liveMounts",
  "module.exports.unmountAll = nativeBinding.unmountAll",
]
let source = await readFile(loader, "utf8")
let changed = false
if (!source.includes(marker)) {
  source += `\n${marker}\n`
  changed = true
}
// Keep the free factory visible to Node's CommonJS-to-ESM named-export
// detector. The generated loader already aliases native classes explicitly;
// this marker makes the new free function follow the same contract without
// requiring a hand edit to generated index.js.
if (!source.includes(chunkedExportMarker)) {
  source += `\n${chunkedExportMarker}\n`
  changed = true
}
for (const exportMarker of facadeExportMarkers) {
  if (!source.includes(exportMarker)) {
    source += `\n${exportMarker}\n`
    changed = true
  }
}
for (const postlude of ["postlude-utilities.cjs", "postlude-servers.cjs"]) {
  const installMarker = `require('./${postlude}')(module.exports)`
  if (!source.includes(installMarker)) {
    source += `\n${installMarker}\n`
    changed = true
  }
}
const harnessMarker = "const publicHarness = require('./postlude-harness.cjs')(module.exports)"
if (!source.includes(harnessMarker)) {
  source += `\n${harnessMarker}\nmodule.exports.createLoopback = publicHarness.createLoopback\nmodule.exports.resolveCapabilities = publicHarness.resolveCapabilities\n`
  changed = true
}
if (changed) await writeFile(loader, source)

// Native lock scheduling is retained; the utility postlude preserves generic
// callback values and error identity across its Promise<void> boundary.
const declarations = new URL("./index.d.ts", import.meta.url)
let types = await readFile(declarations, "utf8")
// Codec buffers are Node Buffers in the native ABI. Import their type
// explicitly rather than requiring consumers to enable ambient Node globals.
const bufferType = 'import type { Buffer } from "node:buffer"'
if (/\bBuffer\b/.test(types) && !types.includes(bufferType)) {
  types = `${bufferType}\n${types}`
}
const duplexType = 'import type { Duplex } from "node:stream"'
if (!types.includes(duplexType)) {
  types = `${duplexType}\n${types}`
}
const disposableLib = '/// <reference lib="esnext.disposable" />'
const nodeTypes = '/// <reference types="node" />'
// Triple-slash directives must precede imports; repeat generation safely.
types = types.replaceAll(`${disposableLib}\n`, "").replaceAll(`${nodeTypes}\n`, "")
types = `${disposableLib}\n${nodeTypes}\n${types}`
types = types.replace(
  /\b(read|write)\(callback: \(\) => Promise<undefined>\): Promise<undefined>/g,
  "$1<T>(callback: () => T | Promise<T>): Promise<T>",
)
// These signatures describe the public lifecycle postlude, not the narrower
// native Promise<void> ABI. Keep generation repeatable after every napi build.
types = types.replace(
  /export declare class (NfsServer|P9Server|S3Server|WebdavServer) \{([\s\S]*?)\n\}/g,
  (declaration, name, body) => {
    body = body.replace(/listen\(\): Promise<[^>]+>/, `listen(): Promise<${name}>`)
    if (!body.includes("[Symbol.asyncDispose]")) {
      body += "\n  [Symbol.asyncDispose](): Promise<void>"
    }
    return `export declare class ${name} {${body}\n}`
  },
)
types = types.replace(
  /export declare class P9Connection \{([\s\S]*?)\n\}/g,
  (declaration, body) => {
    if (!/\bclosed\s*:/.test(body)) body += "\n  readonly closed: Promise<void>"
    if (!/\bstream\s*:/.test(body)) body += "\n  readonly stream: Duplex | undefined"
    return `export declare class P9Connection {${body}\n}`
  },
)
types = types.replace(
  /export declare class NfsConnection \{([\s\S]*?)\n\}/g,
  (declaration, body) => {
    if (!/\bclosed\s*:/.test(body)) body += "\n  readonly closed: Promise<void>"
    return `export declare class NfsConnection {${body}\n}`
  },
)
types = types.replace(
  /export declare class P9Session \{([\s\S]*?)\n\}/g,
  (declaration, body) => {
    if (!/\bhandleCall\(/.test(body)) {
      body = `\n  handleCall(bytes: Uint8Array): Promise<Buffer | null>${body}`
    }
    if (!/\bdestroy\(/.test(body)) body = `\n  destroy(): Promise<void>${body}`
    return `export declare class P9Session {${body}\n}`
  },
)
// The fid facade translates N-API's null option representation to the
// upstream 9P contract's undefined values in p9.cjs. Keep the generated
// declarations aligned with that public boundary after every native build.
types = types.replaceAll("JsFileHandle", "FileHandle")
types = types.replaceAll("get open(): P9FidOpenState | null", "get open(): P9FidOpenState | undefined")
types = types.replaceAll("get cursor(): P9FidCursor | null", "get cursor(): P9FidCursor | undefined")
types = types.replaceAll("get handle(): FileHandle | null", "get handle(): FileHandle | undefined")
types = types.replaceAll("get qid(): NativeP9Qid | null", "get qid(): NativeP9Qid | undefined")
types = types.replaceAll("get(fid: number): P9Fid | null", "get(fid: number): P9Fid | undefined")
types = types.replaceAll("resume(entry: P9Fid, offset: bigint): P9DirResume | null", "resume(entry: P9Fid, offset: bigint): P9DirResume | undefined")
types = types.replace(
  /export declare class P9Server \{([\s\S]*?)\n\}/g,
  (declaration, body) => {
    if (!/\battach\(/.test(body)) {
      body += '\n  attach(stream: Duplex, options?: P9AttachOptions): P9Connection'
    }
    return `export declare class P9Server {${body}\n}`
  },
)
if (!types.includes("export interface P9AttachOptions {")) {
  types = types.replace(
    "export interface P9ServerOptions {",
    `export interface P9AttachOptions {
  peer?: string
  own?: boolean
  maxFrame?: number
  maxInFlight?: number
}

export interface P9ServerOptions {`,
  )
}
// The FUSE postlude wraps the native async class with the public
// mount-free/session facade. Keep generated declarations aligned with that
// runtime layer after every clean `napi build`.
types = types.replace(
  /export declare class FuseSession \{[\s\S]*?\n\}/,
  `export declare class FuseSession {
  constructor(filesystem: Filesystem, options?: FuseSessionOptions | undefined | null)
  readonly options: FuseSessionOptions
  readonly stats: FuseSessionStats
  readonly assertions: string[]
  readonly negotiated: NativeFuseNegotiatedSession | undefined
  readonly protocol: NativeFuseProtocolContext | undefined
  readonly destroyed: boolean
  readonly openHandles: number
  readonly inodes: FuseSessionInodeTable
  handle(bytes: Buffer): Promise<Buffer | null>
  handleMessage(bytes: Buffer): Promise<Buffer | null>
  destroy(): Promise<void>
  notifyInvalInode(ino: bigint, off?: bigint, len?: bigint): Buffer
  notifyInvalEntry(parent: bigint, name: string, flags?: number): Buffer
}`,
)
const fuseSessionTypes = `
export type FuseFlushMechanism = "sync" | "enosys" | "noflush"
export const DEFAULT_ATTR_TIMEOUT: 10
export const DEFAULT_ENTRY_TIMEOUT: 10
export const DEFAULT_FLUSH_MECHANISM: FuseFlushMechanism
export function createFuseSession(filesystem: Filesystem, options?: FuseSessionOptions | undefined | null): FuseSession

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
`
if (!types.includes("export interface FuseSessionStats")) types += fuseSessionTypes
const s3RequestStreamTypes = `
export type S3RequestStreamBody = AsyncIterable<Uint8Array> | ReadableStream<Uint8Array>
`
types = types.replace(
  /handleRequestStream\(head: S3RequestHead, body: ReadableStream<Buffer>\): S3StreamRequest/g,
  "handleRequestStream(head: S3RequestHead, body: S3RequestStreamBody): Promise<S3StreamResponse>",
)
if (!types.includes("export type S3RequestStreamBody =")) types += s3RequestStreamTypes
const utilityTypes = 'import type { FsError, FsErrorOptions } from "./types/root.js"'
if (!types.includes(utilityTypes)) {
  types += `\n${utilityTypes}\nexport type { ErrnoCode, FsError, FsErrorOptions } from "./types/root.js"\nexport { ERRNO_CODES, joinPath } from "./types/root.js"\n`
}
const driverTypes = 'import type { FsDriver } from "./types/driver.js"'
if (!types.includes(driverTypes)) {
  types += `\n${driverTypes}\nexport type { FsDriver, FileHandleLike, DirentLike } from "./types/driver.js"\n`
}
types = types.replace(/(function (?:mount|createNfsServer|createP9Server|createWebdavServer)\(driver: )Filesystem(?=,)/g, "$1Filesystem | FsDriver")
types = types.replace(/(function createS3Server\(source: )Filesystem \| \{ buckets: Record<string, Filesystem> \}/g, "$1Filesystem | FsDriver | { buckets: Record<string, Filesystem | FsDriver> }")
types = types.replace(/function createDriver\(driver: object\)/g, "function createDriver(driver: FsDriver)")
// These dirent helpers are implemented by the JavaScript FUSE postlude rather
// than by napi-rs' generated native declarations. Preserve their public type
// surface across every clean `napi build`; otherwise a release build silently
// removes APIs that `fuse.cjs` still exports at runtime.
const direntTypes = `export interface NativeFuseDirent {
  ino: bigint
  off: bigint
  type: number
  name: string
}

export interface NativeFuseDirentPlus {
  entry: NativeFuseEntryOut
  dirent: NativeFuseDirent
}
`
if (!types.includes("export interface NativeFuseDirent")) {
  types = types.replace("export interface NativeFuseInterruptIn", `${direntTypes}\nexport interface NativeFuseInterruptIn`)
}
const direntFunctions = `export declare function packDirents(entries: Iterable<NativeFuseDirent>, maxSize: number): { buffer: Buffer; packed: number }

export declare function unpackDirents(body: Uint8Array): Array<NativeFuseDirent>

export declare function packDirentsPlus(entries: Iterable<NativeFuseDirentPlus>, maxSize: number, context?: NativeFuseProtocolContext | undefined | null): { buffer: Buffer; packed: number }

export declare function unpackDirentsPlus(body: Uint8Array, context?: NativeFuseProtocolContext | undefined | null): Array<NativeFuseDirentPlus>
`
if (!types.includes("export declare function packDirents(")) {
  types = types.replace("export declare function isFsError", `${direntFunctions}\nexport declare function isFsError`)
}
const harnessTypes = 'export { createLoopback, resolveCapabilities } from "./types/harness.js"'
if (!types.includes(harnessTypes)) {
  types += `\n${harnessTypes}\nexport type { Loopback, ResolvedCapabilities } from "./types/harness.js"\n`
}
const webdavRequestStreamTypes = `
export type WebdavRequestStreamBody = AsyncIterable<Uint8Array> | ReadableStream<Uint8Array>
`
types = types.replace(
  /handleRequestStream\(head: WebdavRequestHead, body: ReadableStream<Buffer>\): WebdavStreamRequest/g,
  "handleRequestStream(head: WebdavRequestHead, body: WebdavRequestStreamBody): Promise<WebdavStreamResponse>",
)
if (!types.includes("export type WebdavRequestStreamBody =")) types += webdavRequestStreamTypes
types = types.replace(
  /(export interface WebdavSessionStats \{[\s\S]*?methods: )Record<string, number>/,
  "$1Map<string, number>",
)
await writeFile(declarations, types)
