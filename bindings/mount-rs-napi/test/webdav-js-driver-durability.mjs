import assert from "node:assert/strict"

import { createDriver, createMemoryDriver, createWebdavServer } from "../index.js"

const payload = Buffer.from("structural durable WebDAV bytes")

async function exercise(mode) {
  const failSyncfs = mode === "failure"
  const backing = createMemoryDriver()
  const calls = []
  const boundedCalls = []
  const driver = {
    capabilities: { ...backing.capabilities, durableWrites: true },
    stat: backing.stat.bind(backing),
    readdir: backing.readdir.bind(backing),
    readdirBounded: async (path, maxEntries) => {
      boundedCalls.push({ path, maxEntries })
      if (path === "/adapter-overflow") {
        // Deliberately violate the optional callback contract so the native
        // adapter's returned-length guard is exercised separately from the
        // provider's own EOVERFLOW response.
        return backing.readdir(path, { withFileTypes: true })
      }
      return backing.readdirBounded(path, maxEntries)
    },
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
  }
  if (mode !== "missing") {
    driver.syncfs = async () => {
      calls.push("syncfs")
      if (failSyncfs) {
        const error = new Error("durability barrier rejected by structural driver")
        Object.assign(error, { code: "EIO", errno: -5, syscall: "syncfs" })
        throw error
      }
    }
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
    if (mode === "failure") {
      assert.equal(response.status, 500)
    } else if (mode === "missing") {
      assert.equal(response.status, 501)
    } else {
      assert.ok([200, 201, 204].includes(response.status))
    }
    assert.deepEqual(calls, mode === "missing" ? [] : ["syncfs"])

    if (mode === "success") {
      const request = (method, target, headers = [], body = null) =>
        server.session.handleRequest({ method, target, headers }, body)
      assert.equal((await request("MKCOL", "/bounded-tree")).status, 201)
      assert.ok([200, 201, 204].includes(
        (await request("PUT", "/bounded-tree/member.txt", [], Buffer.from("bounded bytes"))).status,
      ))

      const propfind = await request(
        "PROPFIND",
        "/bounded-tree",
        [{ name: "depth", value: "1" }],
        Buffer.from('<D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>'),
      )
      assert.equal(propfind.status, 207)
      assert.ok(
        boundedCalls.some(({ path, maxEntries }) => path === "/bounded-tree" && maxEntries === 4096),
        "WebDAV traversal passes its 4,096-entry ceiling to the structural driver",
      )

      const copy = await request(
        "COPY",
        "/bounded-tree",
        [{ name: "destination", value: "/bounded-copy" }],
      )
      assert.ok([201, 204, 207].includes(copy.status))
      const copied = await request("GET", "/bounded-copy/member.txt")
      assert.equal(copied.status, 200)
      assert.deepEqual(copied.body, Buffer.from("bounded bytes"))

      const remove = await request("DELETE", "/bounded-copy")
      assert.ok([204, 207].includes(remove.status))
      assert.equal((await request("GET", "/bounded-copy/member.txt")).status, 404)

      await backing.mkdir("/bounded-overflow")
      await backing.writeFile("/bounded-overflow/alpha", Buffer.from("alpha"))
      await backing.writeFile("/bounded-overflow/beta", Buffer.from("beta"))
      await assert.rejects(
        () => filesystem.readdirBounded("/bounded-overflow", 1),
        (error) => {
          assert.equal(error.code, "EOVERFLOW")
          assert.equal(error.syscall, "scandir")
          return true
        },
      )
      assert.ok(boundedCalls.some(({ path, maxEntries }) => path === "/bounded-overflow" && maxEntries === 1))

      await backing.mkdir("/adapter-overflow")
      await backing.writeFile("/adapter-overflow/alpha", Buffer.from("alpha"))
      await backing.writeFile("/adapter-overflow/beta", Buffer.from("beta"))
      await assert.rejects(
        () => filesystem.readdirBounded("/adapter-overflow", 1),
        (error) => {
          assert.equal(error.code, "EOVERFLOW")
          assert.equal(error.syscall, "readdirBounded")
          return true
        },
      )

      const unboundedDriver = { ...driver }
      delete unboundedDriver.readdirBounded
      const unboundedFilesystem = createDriver(unboundedDriver)
      const unboundedServer = createWebdavServer(unboundedFilesystem, {
        host: "127.0.0.1",
        port: 0,
      })
      try {
        const unsupported = await unboundedServer.session.handleRequest(
          {
            method: "PROPFIND",
            target: "/bounded-tree",
            headers: [{ name: "depth", value: "1" }],
          },
          Buffer.from('<D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>'),
        )
        assert.equal(unsupported.status, 501)
      } finally {
        await unboundedServer.close().catch(() => {})
        await unboundedFilesystem.shutdown().catch(() => {})
      }
    }
  } finally {
    await server.close().catch(() => {})
    await filesystem.shutdown().catch(() => {})
    await backing.shutdown().catch(() => {})
  }
}

await exercise("success")
await exercise("failure")
await exercise("missing")

console.log("mount-rs N-API WebDAV structural-driver durability: PASS")
