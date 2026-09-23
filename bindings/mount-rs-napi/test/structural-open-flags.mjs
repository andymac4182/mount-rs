import assert from "node:assert/strict"
import { constants } from "node:fs"
import { mkdtemp, open, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { createDriver } from "../index.js"

const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-open-flags-"))
try {
  await writeFile(join(root, "existing"), "keep these bytes")
  await writeFile(join(root, "writable"), "truncate these bytes")

  let validTruncateFlags = constants.O_RDWR | constants.O_TRUNC
  if (process.platform === "win32") {
    const controlPath = join(root, "host-control")
    await writeFile(controlPath, "host truncate control")
    let control
    try {
      control = await open(controlPath, validTruncateFlags)
    } catch (error) {
      if (error.code !== "EINVAL") throw error
      // Some libuv Windows releases reject TRUNCATE_EXISTING on host files:
      // https://github.com/libuv/libuv/discussions/4291
      validTruncateFlags |= constants.O_CREAT
      control = await open(controlPath, validTruncateFlags)
      console.log("Windows host truncate requires O_CREAT; numeric forwarding remains checked")
    }
    await control.close()
    assert.equal((await readFile(controlPath)).length, 0)
  }

  let openCalls = 0
  const receivedFlags = []
  const structural = {
    stat: async () => { throw new Error("stat should not be called") },
    readdir: async () => { throw new Error("readdir should not be called") },
    open: (path, flags, mode) => {
      openCalls++
      receivedFlags.push(flags)
      return open(join(root, path.slice(1)), flags, mode)
    },
  }
  const filesystem = createDriver(structural)
  try {
    let invalidError
    try {
      const handle = await filesystem.open("/existing", constants.O_TRUNC)
      await handle.close()
    } catch (error) {
      invalidError = error
    }
    assert.equal(
      await readFile(join(root, "existing"), "utf8"),
      "keep these bytes",
      "a rejected truncate must leave the host file unchanged",
    )
    assert.equal(openCalls, 0, "invalid decoded flags must not reach the JS open callback")
    assert.equal(invalidError?.code, "EINVAL")
    assert.equal(invalidError.syscall, "open")
    assert.equal(invalidError.path, "/existing")

    const handle = await filesystem.open("/writable", validTruncateFlags)
    await handle.close()
    assert.equal(openCalls, 1, "truncate with write access must reach the JS callback")
    assert.equal(receivedFlags.at(-1), validTruncateFlags, "valid flags must preserve the host namespace")
    assert.equal((await readFile(join(root, "writable"))).length, 0)

    for (const [input, expected] of [
      [NaN, constants.O_RDONLY],
      [Infinity, constants.O_RDONLY],
      [-Infinity, constants.O_RDONLY],
      [1.9, constants.O_WRONLY],
      [4_294_967_297.9, constants.O_WRONLY],
    ]) {
      const coerced = await filesystem.open("/existing", input)
      await coerced.close()
      assert.equal(receivedFlags.at(-1), expected, "Node numeric flags must use ToInt32")
    }
    assert.equal(openCalls, 6)
    assert.equal(await readFile(join(root, "existing"), "utf8"), "keep these bytes")
  } finally {
    await filesystem.shutdown()
  }
} finally {
  await rm(root, { recursive: true, force: true })
}

console.log("mount-rs N-API decoded truncate flags: PASS")
