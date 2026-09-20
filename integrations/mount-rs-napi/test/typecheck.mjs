import { existsSync, readdirSync } from "node:fs"
import { spawnSync } from "node:child_process"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const testDirectory = dirname(fileURLToPath(import.meta.url))
const packageDirectory = resolve(testDirectory, "..")
const fixture = join(testDirectory, "types.test.ts")

const directCandidates = [
  join(packageDirectory, "node_modules", "typescript", "bin", "tsc"),
]
const pnpmDirectory = join(packageDirectory, "node_modules", ".pnpm")
const scopedCandidates = existsSync(pnpmDirectory)
  ? readdirSync(pnpmDirectory)
      .filter((entry) => entry.startsWith("typescript@"))
      .sort()
      .map((entry) =>
        join(
          pnpmDirectory,
          entry,
          "node_modules",
          "typescript",
          "bin",
          "tsc",
        ),
      )
  : []
const compiler = [...directCandidates, ...scopedCandidates].find(existsSync)

if (!compiler) {
  console.error(
    "TypeScript is required for the package type regression; add it as a devDependency before running this check.",
  )
  process.exit(1)
}

const result = spawnSync(
  process.execPath,
  [
    compiler,
    "--noEmit",
    "--strict",
    "--module",
    "NodeNext",
    "--moduleResolution",
    "NodeNext",
    "--target",
    "ES2022",
    fixture,
  ],
  { cwd: packageDirectory, stdio: "inherit" },
)

if (result.error) {
  console.error(result.error)
  process.exit(1)
}

process.exit(result.status ?? 1)
