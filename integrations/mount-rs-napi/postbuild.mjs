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
    return `export declare class P9Connection {${body}\n}`
  },
)
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
await writeFile(declarations, types)
