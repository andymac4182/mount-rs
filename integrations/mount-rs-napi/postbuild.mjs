import { readFile, writeFile } from "node:fs/promises"

const loader = new URL("./index.js", import.meta.url)
const marker = "require('./postlude.cjs')(module.exports)"
const chunkedExportMarker = "module.exports.createChunkedDriver = nativeBinding.createChunkedDriver"
const facadeExportMarkers = [
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
