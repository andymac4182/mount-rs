import assert from "node:assert/strict"
import {
  createDriver,
  createMemoryDriver,
  liveMounts,
  mount,
  probeTransports,
  unmountAll,
} from "../index.js"
import {
  mkdtemp,
  readFile,
  rmdir,
  writeFile,
} from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

const SUPPORTED_NATIVE_PLATFORMS = new Set(["darwin", "linux"])
const runNativeMount = process.env.MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT === "1"
const requestedTransport = process.env.MOUNT_RS_NAPI_STRUCTURAL_TRANSPORT ?? "auto"

function structuralView(backing) {
  const calls = Object.create(null)
  const driver = {
    capabilities: backing.capabilities,
    mountx: backing.mountx,
  }
  for (const name of [
    "stat",
    "lstat",
    "statfs",
    "readdir",
    "open",
    "mkdir",
    "rmdir",
    "unlink",
    "rename",
    "link",
    "symlink",
    "readlink",
    "chmod",
    "chown",
    "lchown",
    "truncate",
    "utimes",
    "lutimes",
  ]) {
    const method = backing[name]
    if (typeof method !== "function") continue
    driver[name] = (...args) => {
      calls[name] = (calls[name] ?? 0) + 1
      return method.apply(backing, args)
    }
  }
  return { driver, calls }
}

function mountTransportKey(transport) {
  return transport === "9p" ? "9p" : transport
}

function allowedTransport(value) {
  return value === "auto" || value === "fuse" || value === "9p" || value === "nfs"
}

async function withTimeout(promise, milliseconds, label) {
  let timer
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out after ${milliseconds}ms`)), milliseconds)
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

const backing = createMemoryDriver()
const { driver: structural, calls } = structuralView(backing)
let adapter
try {
  // This is a portable, non-privileged proof that the structural callback
  // adapter can retain a caller-owned FsDriver and complete normal I/O.
  adapter = createMemoryDriver()
  const adapterView = structuralView(adapter).driver
  const adapted = createDriver(adapterView)
  await adapted.writeFile("/structural-adapter.txt", "adapter path")
  assert.equal(
    Buffer.from(await adapted.readFile("/structural-adapter.txt")).toString(),
    "adapter path",
  )
  await adapted.shutdown()

  // A malformed object must fail before any host mount attempt. This locks
  // the public capability boundary and keeps native errors actionable.
  await assert.rejects(
    () => mount({}, "/mount-rs-structural-invalid", { transport: "fuse" }),
    (error) => error?.message === "FsDriver method 'stat' must be a function",
  )

  await backing.writeFile("/seed.txt", "seed through structural driver")

  if (!runNativeMount) {
    console.log(
      "structural native mount: SKIP (set MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT=1 to opt in)",
    )
  } else if (!SUPPORTED_NATIVE_PLATFORMS.has(process.platform)) {
    console.log(
      `structural native mount: SKIP (native lifecycle test is limited to macOS/Linux; platform=${process.platform})`,
    )
  } else {
    assert.ok(allowedTransport(requestedTransport),
      "MOUNT_RS_NAPI_STRUCTURAL_TRANSPORT must be auto, fuse, 9p, or nfs")
    const probe = await probeTransports()
    const chosen = requestedTransport === "auto" ? probe.chosen : requestedTransport
    assert.ok(chosen, `no usable native transport was selected for ${requestedTransport}`)
    const availability = probe[mountTransportKey(chosen)]
    assert.equal(availability?.usable, true,
      `${chosen} is not usable: ${availability?.reason ?? "no probe reason"}`)

    const mountpoint = await mkdtemp(join(tmpdir(), "mount-rs-napi-structural-native-"))
    let mounted
    let teardownFailure
    let safeToRemove = false
    try {
      // Pass the plain structural object directly. The N-API mount binding
      // must retain it for the complete native server lifecycle.
      mounted = await withTimeout(
        mount(structural, mountpoint, { transport: requestedTransport }),
        60_000,
        "structural native mount",
      )
      assert.equal(mounted.active, true)
      assert.ok(["fuse", "9p", "nfs"].includes(mounted.transport))

      const seed = await withTimeout(readFile(join(mountpoint, "seed.txt"), "utf8"), 30_000, "mounted read")
      assert.equal(seed, "seed through structural driver")
      const mountedPath = join(mountpoint, "written-through-native.txt")
      const writtenValue = "write through structural native mount"
      await withTimeout(writeFile(mountedPath, writtenValue), 30_000, "mounted write")
      assert.equal(await withTimeout(readFile(mountedPath, "utf8"), 30_000, "mounted readback"), writtenValue)
      assert.ok((calls.open ?? 0) > 0, "native I/O did not reach structural open callback")
      assert.ok(
        (calls.stat ?? 0) + (calls.lstat ?? 0) > 0,
        "native I/O did not reach structural stat callback",
      )
      console.log(`structural native mount: PASS (${mounted.transport}; read/write callback reachability)`)
    } catch (error) {
      teardownFailure = error
    } finally {
      try {
        if (mounted) await withTimeout(mounted.unmount(), 30_000, "structural native unmount")
      } catch (error) {
        teardownFailure ??= error
      }
      try {
        const failures = await withTimeout(unmountAll(), 30_000, "structural native cleanup")
        if (failures.length > 0) {
          throw new Error(failures.map(({ message }) => message).join("; "))
        }
      } catch (error) {
        teardownFailure ??= error
      }
      try {
        const active = (await liveMounts()).filter(({ active: isActive }) => isActive)
        if (active.length > 0) throw new Error(`cleanup left ${active.length} live native mount(s)`)
      } catch (error) {
        teardownFailure ??= error
      }
      if (!teardownFailure && mounted?.active === false) {
        safeToRemove = true
      }
      if (safeToRemove) {
        try {
          await rmdir(mountpoint)
        } catch (error) {
          teardownFailure ??= error
        }
      } else {
        console.error(`structural native mountpoint preserved for inspection: ${mountpoint}`)
      }
    }
    if (teardownFailure) throw teardownFailure
    assert.equal(await backing.readFile("/written-through-native.txt").then((value) => Buffer.from(value).toString()), "write through structural native mount")
  }
} finally {
  // Native mounts do not own the caller's structural driver's shutdown. Keep
  // this explicit so a future lifecycle change cannot hide ownership bugs.
  await backing.shutdown()
  if (adapter) await adapter.shutdown().catch(() => {})
}

console.log("structural native mount acceptance checks passed")
