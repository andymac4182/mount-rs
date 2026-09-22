import assert from "node:assert/strict"

import { createDriver, createMemoryDriver, createWebdavServer } from "../index.js"

const payload = Buffer.from("structural durable WebDAV bytes")

async function exercise(failSyncfs) {
  const backing = createMemoryDriver()
  const calls = []
  const driver = {
    capabilities: { ...backing.capabilities, durableWrites: true },
    stat: backing.stat.bind(backing),
    readdir: backing.readdir.bind(backing),
    open: backing.open.bind(backing),
    mkdir: backing.mkdir.bind(backing),
    rmdir: backing.rmdir.bind(backing),
    unlink: backing.unlink.bind(backing),
    rename: backing.rename.bind(backing),
    link: backing.link.bind(backing),
    symlink: backing.symlink.bind(backing),
    readlink: backing.readlink.bind(backing),
    chmod: backing.chmod.bind(backing),
    chown: backing.chown.bind(backing),
    lchown: backing.lchown.bind(backing),
    truncate: backing.truncate.bind(backing),
    utimes: backing.utimes.bind(backing),
    lutimes: backing.lutimes.bind(backing),
    async syncfs() {
      calls.push("syncfs")
      if (failSyncfs) {
        const error = new Error("durability barrier rejected by structural driver")
        Object.assign(error, { code: "EIO", errno: -5, syscall: "syncfs" })
        throw error
      }
    },
  }
  const filesystem = createDriver(driver)
  const server = createWebdavServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
  })

  try {
    const response = await server.session.handleRequest(
      { method: "PUT", target: "/structural-durable.txt", headers: [] },
      payload,
    )
    if (failSyncfs) {
      assert.equal(response.status, 500)
    } else {
      assert.ok([200, 201, 204].includes(response.status))
    }
    assert.deepEqual(calls, ["syncfs"])
  } finally {
    await server.close().catch(() => {})
    await filesystem.shutdown().catch(() => {})
    await backing.shutdown().catch(() => {})
  }
}

await exercise(false)
await exercise(true)

console.log("mount-rs N-API WebDAV structural-driver durability: PASS")
