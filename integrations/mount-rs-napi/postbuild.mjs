import { readFile, writeFile } from "node:fs/promises"

const loader = new URL("./index.js", import.meta.url)
const marker = "require('./postlude.cjs')(module.exports)"
const chunkedExportMarker = "module.exports.createChunkedDriver = nativeBinding.createChunkedDriver"
const facadeExportMarkers = [
  ...["isNormalizedPath", "splitPath", "normalizePath", "resolvePath", "joinPathParts", "dirname", "basename", "isPathInside", "fsError", "rangeError", "isFsError", "errnoOf", "fileTypeMode", "isSpecialMode", "PathLock"].map((name) => `module.exports.${name} = nativeBinding.${name}`),
  "module.exports.ERRNO_CODES = Object.freeze(nativeBinding.errnoCodes())",
  "module.exports.joinPath = (...parts) => nativeBinding.joinPathParts(parts)",
  "module.exports.createUnstorageDriver = nativeBinding.createUnstorageDriver",
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
if (changed) await writeFile(loader, source)
const utilitiesMarker = "require('./postlude-utilities.cjs')(module.exports)"
if (!source.includes(utilitiesMarker)) {
  await writeFile(loader, `${source}\n${utilitiesMarker}\n`)
}

// Native lock scheduling is retained; the utility postlude preserves generic
// callback values and error identity across its Promise<void> boundary.
const declarations = new URL("./index.d.ts", import.meta.url)
let types = await readFile(declarations, "utf8")
types = types.replace(
  /\b(read|write)\(callback: \(\) => Promise<undefined>\): Promise<undefined>/g,
  "$1<T>(callback: () => T | Promise<T>): Promise<T>",
)
const utilityTypes = 'import type { FsError, FsErrorOptions } from "./types/root.js"'
if (!types.includes(utilityTypes)) {
  types += `\n${utilityTypes}\nexport type { ErrnoCode, FsError, FsErrorOptions } from "./types/root.js"\nexport { ERRNO_CODES, joinPath } from "./types/root.js"\n`
}
await writeFile(declarations, types)
