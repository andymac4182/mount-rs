import { spawnSync } from "node:child_process"

const args = ["exec", "napi", "build", "--platform", ...process.argv.slice(2)]
const env = { ...process.env }

if (process.platform === "darwin") {
  // Rust 1.95's LLVM 22 Mach-O post-link layout can leave the __LINKEDIT
  // string table four-byte aligned. macOS 27 dyld rejects those cdylibs.
  // Keep the old toolchain on the pre-chained-fixups SDK declaration until
  // the repository moves to the Rust release containing the upstream fix.
  // See https://github.com/rust-lang/rust/issues/157750.
  const rustc = spawnSync("rustc", ["--version"], { encoding: "utf8" })
  const match = /^rustc (\d+)\.(\d+)/u.exec(rustc.stdout ?? "")
  const needsWorkaround =
    !match || (Number(match[1]) === 1 && Number(match[2]) < 98)
  if (needsWorkaround) {
    const deploymentTarget = "11.0"
    const linkArgs = `-C link-arg=-Wl,-platform_version,macos,${deploymentTarget},26.0`
    env.MACOSX_DEPLOYMENT_TARGET = deploymentTarget
    if (!env.RUSTFLAGS?.includes(linkArgs)) {
      env.RUSTFLAGS = [env.RUSTFLAGS, linkArgs].filter(Boolean).join(" ")
    }
  }
}

const pnpm = process.env.npm_execpath
  ? process.execPath
  : process.platform === "win32"
    ? "pnpm.cmd"
    : "pnpm"
const pnpmArgs = process.env.npm_execpath
  ? [process.env.npm_execpath, ...args]
  : args
const result = spawnSync(pnpm, pnpmArgs, { env, stdio: "inherit" })

if (result.error) throw result.error
process.exit(result.status ?? 1)
