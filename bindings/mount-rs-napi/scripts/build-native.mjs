import { spawnSync } from "node:child_process"

const env = { ...process.env }
const requestedFeatures = env.MOUNT_RS_NAPI_FEATURES?.trim()
const args = [
  "exec", "napi", "build", "--platform", ...process.argv.slice(2),
  ...(requestedFeatures ? ["--features", requestedFeatures] : []),
]

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

const npmExecPath = process.env.npm_execpath
const isJavaScriptCli = /\.(?:cjs|js|mjs)$/iu.test(npmExecPath ?? "")
const pnpm = isJavaScriptCli
  ? process.execPath
  : npmExecPath ?? (process.platform === "win32" ? "pnpm.cmd" : "pnpm")
const pnpmArgs = isJavaScriptCli ? [npmExecPath, ...args] : args
const result = spawnSync(pnpm, pnpmArgs, { env, stdio: "inherit" })

if (result.error) throw result.error
process.exit(result.status ?? 1)
