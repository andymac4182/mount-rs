import { access, cp, mkdir } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const siteRoot = fileURLToPath(new URL('..', import.meta.url))
const sourceDir = `${siteRoot}/public/downloads`
const outputDirs = [`${siteRoot}/.output/public/downloads`]

try {
  await access(`${siteRoot}/.vercel/output/static`)
  outputDirs.push(`${siteRoot}/.vercel/output/static/downloads`)
} catch {
  // The standard local build does not create a Vercel output directory.
}

for (const outputDir of outputDirs) {
  await mkdir(outputDir, { recursive: true })
  await cp(sourceDir, outputDir, { recursive: true, force: true })
}

console.log(`Restored byte-preserving download assets in ${outputDirs.length} generated output directories`)
