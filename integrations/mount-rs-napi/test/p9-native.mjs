import assert from "node:assert/strict"
import { createNodeFsDriver } from "@mount-rs/core"
import p9 from "@mount-rs/core/9p"
import { mkdtemp, readFile, rmdir, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

const { live9pMounts, mount9p, p9ClientProbe, unmountAll9p } = p9

if (process.env.MOUNT_RS_NAPI_P9_NATIVE_MOUNT !== "1") {
  console.log(
    "mount-rs N-API direct 9P mount: SKIP (set MOUNT_RS_NAPI_P9_NATIVE_MOUNT=1 to require a host mount)",
  )
  process.exit(0)
}

const probe = p9ClientProbe()
assert.equal(probe.platform, "linux")
assert.equal(probe.usable, true, probe.reason ?? "the Linux 9P client is unavailable")
assert.equal(probe.kernel, true)
assert.equal(probe.transport, true)
assert.equal(probe.modules, true)
assert.equal(probe.root, true)

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-p9-native-root-"))
const mountpoint = await mkdtemp(join(tmpdir(), "mount-rs-napi-p9-native-mount-"))
let driver
let mounted
let teardownFailure
let safeToRemove = false

try {
  await writeFile(join(root, "seed.txt"), "direct 9P seed")
  driver = createNodeFsDriver(root)
  mounted = await mount9p(driver, mountpoint, {
    transport: "unix",
    mountMsize: 256 * 1024,
    access: "client",
    cache: "none",
    uname: "root",
    aname: "/",
  })
  assert.equal(mounted.active, true)
  assert.equal(mounted.transport, "9p")
  assert.equal(typeof mounted.source, "string")
  assert.ok(mounted.source.length > 0)
  assert.equal(mounted.trans, "unix")
  assert.ok(mounted.server)
  assert.ok(mounted.connection)
  assert.equal(typeof mounted.waitClosed, "function")
  assert.equal((await live9pMounts()).length, 1)

  assert.equal(await readFile(join(mountpoint, "seed.txt"), "utf8"), "direct 9P seed")
  const mountedFile = join(mountpoint, "direct-node-fs.txt")
  await writeFile(mountedFile, "direct 9P write")
  assert.equal(await readFile(mountedFile, "utf8"), "direct 9P write")
} catch (error) {
  teardownFailure = error
} finally {
  try {
    if (mounted) await mounted.unmount()
  } catch (error) {
    teardownFailure ??= error
  }
  try {
    const failures = await unmountAll9p()
    if (failures.length > 0) {
      throw new Error(
        `direct 9P cleanup failed: ${failures.map(({ message }) => message).join("; ")}`,
      )
    }
  } catch (error) {
    teardownFailure ??= error
  }
  try {
    const live = (await live9pMounts()).filter(({ active }) => active)
    if (live.length > 0) throw new Error(`direct 9P cleanup left ${live.length} live mount(s)`)
  } catch (error) {
    teardownFailure ??= error
  }
  try {
    if (driver) await driver.shutdown()
  } catch (error) {
    teardownFailure ??= error
  }
  safeToRemove = !teardownFailure && mounted?.active === false
}

if (teardownFailure) {
  console.error(`direct 9P mountpoint preserved for inspection: ${mountpoint}`)
  console.error(`direct 9P root preserved for inspection: ${root}`)
  throw teardownFailure
}

assert.equal(mounted?.active, false)
if (safeToRemove) {
  await rmdir(mountpoint)
  await rm(root, { recursive: true, force: true })
} else {
  console.error(`direct 9P mountpoint preserved for inspection: ${mountpoint}`)
  console.error(`direct 9P root preserved for inspection: ${root}`)
  throw new Error("direct 9P cleanup did not reach a removable state")
}

console.log("mount-rs N-API direct 9P mount: PASS (probe, mounted I/O, views, and cleanup)")
