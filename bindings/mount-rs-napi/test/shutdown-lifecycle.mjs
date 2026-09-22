import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { Filesystem } from "../index.js"

const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-shutdown-"))
const database = join(directory, "filesystem.sqlite")
const filesystem = await Filesystem.sqlite(database)
const retainedMountx = filesystem.mountx

try {
  await filesystem.writeFile("/retained", Buffer.from("native provider"))

  // The first caller is intentionally not awaited. A second caller must join
  // the same native close attempt rather than start a competing close or
  // report success before the provider reference is detached.
  void filesystem.shutdown()
  await filesystem.shutdown()
  await filesystem.shutdown()

  await assert.rejects(
    () => retainedMountx.mknod("/after-close", 0o100644, 0),
    (error) => error?.code === "EBADF",
  )

  // No retry or deletion workaround: Windows must be able to unlink the
  // SQLite file immediately after the awaited native close completes.
  await rm(database)
} finally {
  await rm(directory, { recursive: true, force: true })
}

console.log("mount-rs N-API shutdown lifecycle: PASS")
