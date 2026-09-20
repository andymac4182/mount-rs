import assert from "node:assert/strict"
import { mkdtemp, readFile, realpath, rm, rmdir, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import {
  createNodeFsDriver,
  liveMounts,
  mount,
  probeTransports,
  unmountAll,
} from "../index.js"

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-native-root-"))
const mountpoint = await mkdtemp(join(tmpdir(), "mount-rs-napi-native-mount-"))
const transportEnv = process.env.MOUNT_RS_NAPI_TRANSPORT
const mountOptIn = process.env.MOUNT_RS_NAPI_NATIVE_MOUNT === "1"
let mountpointMayBeRemoved = false
let failure
let cleanupFailure

try {
  await writeFile(join(root, "seed.txt"), "rooted host driver")
  const driver = createNodeFsDriver(root)
  assert.equal(Buffer.from(await driver.readFile("/seed.txt")).toString(), "rooted host driver")
  await driver.writeFile("/created.txt", Buffer.from("created through N-API"))
  assert.equal(Buffer.from(await driver.readFile("/created.txt")).toString(), "created through N-API")
  await driver.shutdown()

  const readOnly = createNodeFsDriver(root, { readOnly: true })
  assert.equal(readOnly.capabilities.readOnly, true)
  await readOnly.shutdown()

  const probe = await probeTransports()
  assert.equal(probe.platform, process.platform)
  assert.ok(Array.isArray(probe.preference))
  assert.deepEqual([...probe.preference].sort(), ["9p", "fuse", "nfs"].sort())
  for (const name of ["fuse", "9p", "nfs"]) {
    assert.equal(typeof probe[name].usable, "boolean", `${name} probe`)
    assert.ok(probe[name].reason === undefined || typeof probe[name].reason === "string")
  }
  assert.deepEqual(await liveMounts(), [])

  await assert.rejects(
    () => mount(driver, mountpoint, { transport: "unknown" }),
    (error) => error.code === "EINVAL",
  )
  await assert.rejects(
    () => mount(driver, mountpoint, { transport: "auto", unmountTimeoutMs: 1.5 }),
    (error) => error.code === "ERR_OUT_OF_RANGE",
  )

  if (!mountOptIn) {
    console.log(
      "mount-rs N-API native mount: SKIP (set MOUNT_RS_NAPI_NATIVE_MOUNT=1 to require a host mount)",
    )
    mountpointMayBeRemoved = true
    process.exitCode = 0
  } else {
    const requested = transportEnv ?? "auto"
    assert.ok(["auto", "fuse", "9p", "nfs"].includes(requested), "invalid MOUNT_RS_NAPI_TRANSPORT")
    const chosen = requested === "auto" ? probe.chosen : requested
    assert.ok(chosen, `native mount requested but no ${requested} transport is usable`)
    const availability = probe[chosen === "9p" ? "9p" : chosen]
    assert.equal(availability.usable, true, `${chosen} probe rejected an opted-in mount`)

    let mounted
    let mountDriver
    let teardownFailure
    let liveAfterTeardown = []
    try {
      // Keep the pre-mount driver used for rootless checks separate. This
      // instance is deliberately fresh and remains live for the whole mount.
      mountDriver = createNodeFsDriver(root)
      mounted = await mount(mountDriver, mountpoint, { transport: requested })
      assert.equal(mounted.active, true)
      assert.equal(await realpath(mounted.mountpoint), await realpath(mountpoint))
      assert.ok(["fuse", "9p", "nfs"].includes(mounted.transport))
      assert.equal((await liveMounts()).length, 1)
      const mountedFile = join(mountpoint, "node-fs-mounted.txt")
      await writeFile(mountedFile, "native mount write")
      assert.equal(await readFile(mountedFile, "utf8"), "native mount write")
      await writeFile(mountedFile, "native mount rewrite")
      assert.equal(await readFile(mountedFile, "utf8"), "native mount rewrite")
    } finally {
      try {
        if (mounted) await mounted.unmount()
      } catch (error) {
        teardownFailure = error
      }
      try {
        const failures = await unmountAll()
        if (failures.length > 0) {
          throw new Error(
            `native mount cleanup failed: ${failures.map(({ message }) => message).join("; ")}`,
          )
        }
      } catch (error) {
        teardownFailure ??= error
      }
      try {
        liveAfterTeardown = (await liveMounts()).filter(({ active }) => active)
        if (liveAfterTeardown.length > 0) {
          throw new Error(`native mount cleanup left ${liveAfterTeardown.length} live mount(s)`)
        }
      } catch (error) {
        teardownFailure ??= error
      }
      try {
        if (mountDriver) await mountDriver.shutdown()
      } catch (error) {
        teardownFailure ??= error
      }
    }
    // The mountpoint may be removed only after the mounted I/O succeeded and
    // teardown proved both inactive and absent from the process registry.
    if (mounted && !teardownFailure && liveAfterTeardown.length === 0) {
      assert.equal(mounted.active, false)
      mountpointMayBeRemoved = true
    }
    if (teardownFailure) throw teardownFailure
    console.log(`mount-rs N-API native mount: PASS (${mounted.transport})`)
  }
} catch (error) {
  // A failed or partially-unmounted opt-in mount must leave its paths for
  // inspection and manual recovery. In particular, never recursively delete
  // a potentially active mountpoint from the outer cleanup.
  failure = error
} finally {
  if (mountpointMayBeRemoved) {
    try {
      await rmdir(mountpoint)
    } catch (error) {
      cleanupFailure = error
      console.error(`mount-rs N-API native mountpoint preserved: ${mountpoint}`)
    }
  } else {
    console.error(`mount-rs N-API native mountpoint preserved: ${mountpoint}`)
  }
  if (!failure && !cleanupFailure) {
    try {
      await rm(root, { recursive: true, force: true })
    } catch (error) {
      cleanupFailure = error
      console.error(`mount-rs N-API native root preserved: ${root}`)
    }
  } else {
    console.error(`mount-rs N-API native root preserved: ${root}`)
  }
}

if (failure) throw failure
if (cleanupFailure) throw cleanupFailure

console.log("mount-rs N-API native facade integration: PASS")
