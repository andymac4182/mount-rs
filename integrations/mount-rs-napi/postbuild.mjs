import { readFile, writeFile } from "node:fs/promises"

const loader = new URL("./index.js", import.meta.url)
const marker = "require('./postlude.cjs')(module.exports)"
let source = await readFile(loader, "utf8")
if (!source.includes(marker)) {
  source += `\n${marker}\n`
  await writeFile(loader, source)
}
